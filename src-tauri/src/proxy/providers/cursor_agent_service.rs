//! Driver for Cursor's `agent.v1.AgentService/Run` — orchestrates the h2
//! stream, session registry, request-context handshake, internal-tool
//! rejection, MCP tool-call surfacing, and the KV blob channel.
//!
//! Entry point is [`run_agent`]. It returns a [`ProxyResponse`] shaped like
//! the original `ChatService` path so the existing cursor_claude /
//! cursor_codex / cursor_apikey entry points can swap protocols transparently.

use super::cursor_agent_proto as proto;
use super::cursor_agent_proto::{
    decode_agent_server_message, decode_exec_server_event, decode_kv_server_event, ExecServerEvent,
    InteractionDelta, KvServerEvent, McpToolDef,
};
use super::cursor_debug::{self, cold_resume_reject};
use super::cursor_event_emitter::{
    AgentEvent, AgentSseWriter, CapturedToolCall, ComposerMarkerFilter, MarkerEvent,
};
use super::cursor_h2_client::{agent_connect_headers, CursorH2Stream};
use super::cursor_image::load_images;
use super::cursor_oauth_auth::CursorAccountData;
use super::cursor_protocol::{cursor_identity_headers, CursorResponseFormat};
use super::cursor_request_builder::{
    estimate_input_tokens, retry_prompt_after_missing_tool, retry_prompt_after_unmapped_tool,
    AgentRunPlan, ToolResultBlock,
};
use super::cursor_session::{CursorSession, CursorSessionManager, PendingToolCall, SessionState};
use super::cursor_tool_bridge::{
    bridge_builtin_tool, bridge_grep_tool, bridge_ls_or_glob_tool, bridge_mcp_exec_tool,
    bridge_read_lints_tool, bridge_read_tool, bridge_write_or_edit_tool, is_declared_tool,
    resolve_shell_mcp_tool_name, BuiltinBridgeKind,
};
use crate::proxy::hyper_client::ProxyResponse;
use crate::proxy::ProxyError;
use async_stream::stream;
use bytes::Bytes;
use http::StatusCode;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// Wall-clock deadline for meaningful upstream output. Heartbeats and h2
/// keepalives reset the inter-frame timer but must not stall tool turns forever.
const MEANINGFUL_PROGRESS_TIMEOUT: Duration = Duration::from_secs(90);

/// Overall stream safety timeout (OmniRoute `CURSOR_STREAM_TIMEOUT_MS` default 300s).
const STREAM_SAFETY_TIMEOUT: Duration = Duration::from_secs(300);

/// Retries when tools are declared but the model narrates without calling one.
const DEFAULT_TOOL_RETRY_ATTEMPTS: usize = 3;

pub struct AgentRunOptions<'a> {
    pub account: &'a CursorAccountData,
    pub access_token: &'a str,
    pub session_manager: &'a CursorSessionManager,
    pub session_key: String,
    pub plan: AgentRunPlan,
    pub format: CursorResponseFormat,
    pub response_model: String,
    pub stream: bool,
}

/// Run one AgentService turn. Handles tool-result resumption automatically.
pub async fn run_agent(options: AgentRunOptions<'_>) -> Result<ProxyResponse, ProxyError> {
    let AgentRunOptions {
        account,
        access_token,
        session_manager,
        session_key,
        plan,
        format,
        response_model,
        stream: want_stream,
    } = options;

    let images = load_images(plan.images.clone()).await?;

    log::debug!(
        "[CursorAgent] AgentService stream={} tools={} model={} wd={}",
        want_stream,
        plan.tools.len(),
        plan.model_id,
        plan.working_directory
    );

    if want_stream {
        run_stream(
            account,
            access_token,
            session_manager.clone(),
            session_key,
            plan,
            images,
            format,
            response_model,
        )
        .await
    } else {
        run_non_stream(
            account,
            access_token,
            session_manager.clone(),
            session_key,
            plan,
            images,
            format,
            response_model,
        )
        .await
    }
}

// ─── Session acquisition ───────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExecHandleResult {
    Continue,
    StopForTool,
}

#[allow(clippy::too_many_arguments)]
async fn acquire_or_open_session(
    account: &CursorAccountData,
    access_token: &str,
    session_manager: &CursorSessionManager,
    session_key: &str,
    model_id: &str,
    user_text: &str,
    system_prompt: Option<&str>,
    tools: &[McpToolDef],
    images: Vec<proto::EncodedImage>,
    tool_results: &[ToolResultBlock],
    working_directory: &str,
    previous_response_id: Option<&str>,
) -> Result<Arc<Mutex<CursorSession>>, ProxyError> {
    if !tool_results.is_empty() {
        if let Some(entry) = session_manager.acquire(session_key).await {
            match try_resume_with_tool_results(&entry, tool_results).await {
                ToolResumeOutcome::Full | ToolResumeOutcome::Partial => {
                    cursor_debug::log_session(
                        "resume",
                        session_key,
                        &format!("tool_results={} pending_after=partial_or_full", tool_results.len()),
                    );
                    return Ok(entry);
                }
                ToolResumeOutcome::NotFound => {
                    session_manager
                        .release(entry.clone(), SessionState::Closed)
                        .await;
                }
            }
        }
        let pending_count = tool_results.len();
        let prev = previous_response_id.unwrap_or("");
        cursor_debug::log_session(
            "cold_resume",
            session_key,
            &format!(
                "tool_results={pending_count} previous_response_id={prev} parked=false"
            ),
        );
        if cold_resume_reject() {
            return Err(ProxyError::InvalidRequest(format!(
                "Cursor session 已过期，无法续接 tool_result（session_key={session_key}）。\
                 请关闭 CC_SWITCH_CURSOR_COLD_RESUME=reject 以允许冷恢复"
            )));
        }
        log::warn!(
            "[CursorAgent] 未找到匹配的 parked session（key={session_key}，tool_results={pending_count}，\
             previous_response_id={prev}），使用冷恢复"
        );
    }

    let headers = build_agent_headers(account, access_token);
    let mut blob_store: HashMap<String, Bytes> = HashMap::new();
    let encoded_body = {
        let mut input = proto::AgentRunInput {
            model_id,
            user_text,
            conversation_id: Some(session_key),
            message_id: None,
            tools: tools.to_vec(),
            system_prompt,
            blob_store: Some(&mut blob_store),
            images,
        };
        proto::encode_agent_run_request(&mut input)
    };
    let first_frame = proto::wrap_connect_frame(&encoded_body);
    let stream_handle = CursorH2Stream::open(headers, first_frame).await?;
    let tool_names: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();
    let session = session_manager
        .open(
            session_key.to_string(),
            stream_handle,
            blob_store,
            tool_names,
            working_directory.to_string(),
        )
        .await;
    if should_half_close_after_run_request(tools.len(), tool_results.len()) {
        let mut s = session.lock().await;
        s.stream.close_writer();
    }
    Ok(session)
}

async fn try_resume_with_tool_results(
    entry: &Arc<Mutex<CursorSession>>,
    tool_results: &[ToolResultBlock],
) -> ToolResumeOutcome {
    let mut s = entry.lock().await;
    if tool_results.is_empty() {
        return ToolResumeOutcome::NotFound;
    }
    let mut matched = false;
    for tr in tool_results {
        if s.pending_tool_calls.contains_key(&tr.tool_call_id) {
            matched = true;
            break;
        }
    }
    if !matched {
        return ToolResumeOutcome::NotFound;
    }
    for tr in tool_results {
        let Some(pending) = s.pending_tool_calls.remove(&tr.tool_call_id) else {
            continue;
        };
        let frame = proto::encode_exec_mcp_result(
            pending.exec_msg_id,
            &pending.exec_id,
            &tr.content,
            tr.is_error,
        );
        if let Err(e) = s.stream.send_frame(frame) {
            log::warn!("[CursorAgent] 写 tool_result 失败：{e}");
            return ToolResumeOutcome::NotFound;
        }
    }
    if s.pending_tool_calls.is_empty() {
        s.stream.close_writer();
        ToolResumeOutcome::Full
    } else {
        ToolResumeOutcome::Partial
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolResumeOutcome {
    Full,
    Partial,
    NotFound,
}

fn build_agent_headers(account: &CursorAccountData, access_token: &str) -> Vec<(String, String)> {
    let mut headers = agent_connect_headers();
    headers.push((
        "authorization".to_string(),
        format!("Bearer {access_token}"),
    ));
    for (k, v) in cursor_identity_headers(account, access_token) {
        headers.push((k.to_ascii_lowercase(), v));
    }
    headers.push((
        "x-amzn-trace-id".to_string(),
        uuid::Uuid::new_v4().to_string(),
    ));
    headers
}

async fn upstream_status_message(stream: &mut CursorH2Stream) -> String {
    let mut acc = Vec::new();
    while let Ok(Some(frame)) = stream.next_frame().await {
        acc.extend_from_slice(&frame.payload);
        if acc.len() > 64 * 1024 {
            break;
        }
    }
    String::from_utf8_lossy(&acc).into_owned()
}

// ─── Exec event dedup ──────────────────────────────────────────────────────

#[derive(Debug, Default)]
struct ExecDedup {
    seen: HashSet<String>,
}

impl ExecDedup {
    fn track(&mut self, exec: &ExecServerEvent) -> bool {
        self.seen.insert(exec.dedup_key())
    }
}

// ─── Drive loop ────────────────────────────────────────────────────────────

/// How a [`drive_loop`] finished. Session release is the caller's duty when
/// [`drive_loop`] is invoked with `release_session: false`.
enum DriveEnd {
    Completed,
    ParkedForTool,
}

/// Run the read/write loop until the cursor stream signals turn_ended or the
/// h2 stream closes. When `release_session` is true (legacy/default), this
/// function releases the session before returning; otherwise the caller must.
async fn drive_loop(
    session_entry: Arc<Mutex<CursorSession>>,
    session_manager: &CursorSessionManager,
    session_key: &str,
    writer: &mut AgentSseWriter,
    filter: &mut ComposerMarkerFilter,
    saw_tool_call: &mut bool,
    unmapped_tool_name: &mut Option<String>,
    release_session: bool,
) -> Result<DriveEnd, ProxyError> {
    let mut exec_dedup = ExecDedup::default();
    loop {
        let next = {
            let mut s = session_entry.lock().await;
            s.stream.next_frame().await
        };
        let frame = match next {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => return Err(e),
        };

        if let Some(kv) = decode_kv_server_event(&frame.payload) {
            handle_kv_event(kv, &session_entry).await;
        }

        if let Some(exec) = decode_exec_server_event(&frame.payload) {
            let exec_result = handle_exec_event(
                exec,
                &mut exec_dedup,
                &session_entry,
                session_manager,
                session_key,
                writer,
                &mut Vec::new(),
                saw_tool_call,
                unmapped_tool_name,
            )
            .await;
            if exec_result == ExecHandleResult::StopForTool {
                if release_session {
                    session_manager
                        .release(session_entry.clone(), SessionState::AwaitingToolResult)
                        .await;
                }
                return Ok(DriveEnd::ParkedForTool);
            }
        }

        for delta in decode_agent_server_message(&frame.payload) {
            if matches!(delta, InteractionDelta::TurnEnded) {
                for ev in filter.flush() {
                    let mut sink = Vec::new();
                    push_marker_event(writer, ev, &mut sink);
                }
                if release_session {
                    let has_pending = {
                        let s = session_entry.lock().await;
                        !s.pending_tool_calls.is_empty()
                    };
                    let final_state = if has_pending {
                        SessionState::AwaitingToolResult
                    } else {
                        SessionState::Closed
                    };
                    session_manager
                        .release(session_entry.clone(), final_state)
                        .await;
                }
                return Ok(DriveEnd::Completed);
            }
            let _ = emit_interaction_delta(delta, writer, filter);
        }
    }

    // Stream ended without TurnEnded.
    for ev in filter.flush() {
        let mut sink = Vec::new();
        push_marker_event(writer, ev, &mut sink);
    }
    if release_session {
        let has_pending = {
            let s = session_entry.lock().await;
            !s.pending_tool_calls.is_empty()
        };
        let final_state = if has_pending {
            SessionState::AwaitingToolResult
        } else {
            SessionState::Closed
        };
        session_manager
            .release(session_entry.clone(), final_state)
            .await;
    }
    Ok(DriveEnd::Completed)
}

fn push_marker_event(writer: &mut AgentSseWriter, ev: MarkerEvent, sink: &mut Vec<String>) {
    match ev {
        MarkerEvent::Text(txt) => sink.extend(writer.event(&AgentEvent::Text(txt))),
        MarkerEvent::ToolCall(tc) => sink.extend(writer.event(&AgentEvent::ToolCall(tc))),
    }
}

fn emit_interaction_delta(
    delta: InteractionDelta,
    writer: &mut AgentSseWriter,
    filter: &mut ComposerMarkerFilter,
) -> Vec<String> {
    let mut out = Vec::new();
    match delta {
        InteractionDelta::Text(t) => {
            for ev in filter.push(&t) {
                push_marker_event(writer, ev, &mut out);
            }
        }
        InteractionDelta::Thinking(t) => {
            out.extend(writer.event(&AgentEvent::Thinking(t)));
        }
        InteractionDelta::ThinkingComplete => {
            out.extend(writer.event(&AgentEvent::ThinkingComplete));
        }
        InteractionDelta::TokenDelta(tokens) if tokens > 0 => {
            let output = tokens.min(u64::from(u32::MAX)) as u32;
            out.extend(writer.event(&AgentEvent::Usage { input: 0, output }));
        }
        InteractionDelta::ToolCallStarted | InteractionDelta::ToolCallCompleted => {}
        InteractionDelta::TokenDelta(_) | InteractionDelta::Heartbeat => {}
        InteractionDelta::Unknown(_) | InteractionDelta::KvServerMessage | InteractionDelta::TurnEnded => {}
    }
    out
}

async fn handle_exec_event(
    exec: ExecServerEvent,
    exec_dedup: &mut ExecDedup,
    session_entry: &Arc<Mutex<CursorSession>>,
    session_manager: &CursorSessionManager,
    session_key: &str,
    writer: &mut AgentSseWriter,
    events: &mut Vec<String>,
    saw_tool_call: &mut bool,
    unmapped_tool_name: &mut Option<String>,
) -> ExecHandleResult {
    if !exec_dedup.track(&exec) {
        log::debug!(
            "[CursorAgent] skip duplicate exec event: {}",
            exec.dedup_key()
        );
        return ExecHandleResult::Continue;
    }

    match exec {
        ExecServerEvent::RequestContext {
            exec_msg_id,
            exec_id,
        } => {
            let (reply, wd) = {
                let s = session_entry.lock().await;
                (
                    proto::encode_rich_request_context_response(
                        exec_msg_id,
                        &exec_id,
                        &s.working_directory,
                    ),
                    s.working_directory.clone(),
                )
            };
            log::debug!("[CursorAgent] RequestContext rich ack (wd={wd})");
            let s = session_entry.lock().await;
            if let Err(e) = s.stream.send_frame(reply) {
                log::warn!("[CursorAgent] RequestContext ack 写入失败：{e}");
            }
            ExecHandleResult::Continue
        }
        ExecServerEvent::Read {
            exec_msg_id,
            exec_id,
            path,
            tool_call_id,
            offset,
            limit,
        } => {
            let declared = {
                let s = session_entry.lock().await;
                s.declared_tool_names.clone()
            };
            if let Some((name, args)) = bridge_read_tool(&declared, &path, offset, limit) {
                log::debug!("[CursorAgent] bridging built-in read → MCP {name}");
                return surface_mcp_tool_call(
                    exec_msg_id,
                    &exec_id,
                    name,
                    &tool_call_id,
                    args,
                    session_entry,
                    session_manager,
                    session_key,
                    writer,
                    events,
                    saw_tool_call,
                )
                .await;
            }
            let s = session_entry.lock().await;
            let _ = s.stream.send_frame(proto::encode_exec_read_rejected(
                exec_msg_id,
                &exec_id,
                &path,
                "cc-switch 不执行 Cursor 内置 read 工具。请改用客户端 MCP 工具。",
            ));
            ExecHandleResult::Continue
        }
        ExecServerEvent::Write {
            exec_msg_id,
            exec_id,
            path,
            file_text,
            stream_content,
            tool_call_id,
        } => {
            let declared = {
                let s = session_entry.lock().await;
                s.declared_tool_names.clone()
            };
            if let Some((name, args)) =
                bridge_write_or_edit_tool(&declared, &path, &file_text, &stream_content)
            {
                log::debug!("[CursorAgent] bridging built-in write/edit → MCP {name}");
                return surface_mcp_tool_call(
                    exec_msg_id,
                    &exec_id,
                    name,
                    &tool_call_id,
                    args,
                    session_entry,
                    session_manager,
                    session_key,
                    writer,
                    events,
                    saw_tool_call,
                )
                .await;
            }
            let s = session_entry.lock().await;
            let _ = s.stream.send_frame(proto::encode_exec_write_rejected(
                exec_msg_id,
                &exec_id,
                &path,
                "cc-switch 不执行 Cursor 内置 write 工具。请改用客户端 MCP 工具。",
            ));
            ExecHandleResult::Continue
        }
        ExecServerEvent::Delete {
            exec_msg_id,
            exec_id,
            path,
        } => {
            let declared = {
                let s = session_entry.lock().await;
                s.declared_tool_names.clone()
            };
            if let Some((name, args)) =
                bridge_builtin_tool(BuiltinBridgeKind::Delete, &declared, &path, "", "")
            {
                log::debug!("[CursorAgent] bridging built-in delete → MCP {name}");
                return surface_mcp_tool_call(
                    exec_msg_id,
                    &exec_id,
                    name,
                    "",
                    args,
                    session_entry,
                    session_manager,
                    session_key,
                    writer,
                    events,
                    saw_tool_call,
                )
                .await;
            }
            let s = session_entry.lock().await;
            let _ = s.stream.send_frame(proto::encode_exec_delete_rejected(
                exec_msg_id,
                &exec_id,
                &path,
                "cc-switch 不执行 Cursor 内置 delete 工具。请改用客户端 MCP 工具。",
            ));
            ExecHandleResult::Continue
        }
        ExecServerEvent::Ls {
            exec_msg_id,
            exec_id,
            path,
        } => {
            let declared = {
                let s = session_entry.lock().await;
                s.declared_tool_names.clone()
            };
            if let Some((name, args)) = bridge_ls_or_glob_tool(&declared, &path) {
                log::debug!("[CursorAgent] bridging built-in ls/glob → MCP {name}");
                return surface_mcp_tool_call(
                    exec_msg_id,
                    &exec_id,
                    name,
                    "",
                    args,
                    session_entry,
                    session_manager,
                    session_key,
                    writer,
                    events,
                    saw_tool_call,
                )
                .await;
            }
            let s = session_entry.lock().await;
            let _ = s.stream.send_frame(proto::encode_exec_ls_rejected(
                exec_msg_id,
                &exec_id,
                &path,
                "cc-switch 不执行 Cursor 内置 ls 工具。请改用客户端 MCP 工具。",
            ));
            ExecHandleResult::Continue
        }
        ExecServerEvent::Grep {
            exec_msg_id,
            exec_id,
            pattern,
            path,
            glob,
            output_mode,
            case_insensitive,
            head_limit,
        } => {
            let declared = {
                let s = session_entry.lock().await;
                s.declared_tool_names.clone()
            };
            if let Some((name, args)) = bridge_grep_tool(
                &declared,
                &pattern,
                &path,
                &glob,
                &output_mode,
                case_insensitive,
                head_limit,
            ) {
                log::debug!("[CursorAgent] bridging built-in grep → MCP {name}");
                return surface_mcp_tool_call(
                    exec_msg_id,
                    &exec_id,
                    name,
                    "",
                    args,
                    session_entry,
                    session_manager,
                    session_key,
                    writer,
                    events,
                    saw_tool_call,
                )
                .await;
            }
            let s = session_entry.lock().await;
            let _ = s.stream.send_frame(proto::encode_exec_grep_error(
                exec_msg_id,
                &exec_id,
                "cc-switch 不执行 Cursor 内置 grep 工具。",
            ));
            ExecHandleResult::Continue
        }
        ExecServerEvent::Diagnostics {
            exec_msg_id,
            exec_id,
        } => {
            let declared = {
                let s = session_entry.lock().await;
                s.declared_tool_names.clone()
            };
            if let Some((name, args)) = bridge_read_lints_tool(&declared, &[]) {
                log::debug!("[CursorAgent] bridging built-in diagnostics → MCP {name}");
                return surface_mcp_tool_call(
                    exec_msg_id,
                    &exec_id,
                    name,
                    "",
                    args,
                    session_entry,
                    session_manager,
                    session_key,
                    writer,
                    events,
                    saw_tool_call,
                )
                .await;
            }
            let s = session_entry.lock().await;
            let _ = s
                .stream
                .send_frame(proto::encode_exec_diagnostics_result(exec_msg_id, &exec_id));
            ExecHandleResult::Continue
        }
        ExecServerEvent::Shell {
            exec_msg_id,
            exec_id,
            command,
            working_dir,
        }
        | ExecServerEvent::ShellStream {
            exec_msg_id,
            exec_id,
            command,
            working_dir,
        } => {
            let bridge_name = {
                let s = session_entry.lock().await;
                resolve_shell_mcp_tool_name(&s.declared_tool_names)
            };
            if let Some(name) = bridge_name {
                let mut args_map = serde_json::Map::new();
                args_map.insert("command".into(), Value::String(command));
                if !working_dir.is_empty() {
                    args_map.insert("workdir".into(), Value::String(working_dir));
                }
                log::debug!("[CursorAgent] bridging built-in shell exec → MCP tool {name}");
                return surface_mcp_tool_call(
                    exec_msg_id,
                    &exec_id,
                    name,
                    "",
                    Value::Object(args_map),
                    session_entry,
                    session_manager,
                    session_key,
                    writer,
                    events,
                    saw_tool_call,
                )
                .await;
            }
            let s = session_entry.lock().await;
            send_exec_frame(
                &s.stream,
                proto::encode_exec_shell_rejected(
                    exec_msg_id,
                    &exec_id,
                    &command,
                    &working_dir,
                    "cc-switch 不执行 Cursor 内置 shell 工具。请改用客户端 MCP 工具。",
                ),
                "shell_reject",
            );
            ExecHandleResult::Continue
        }
        ExecServerEvent::BackgroundShell {
            exec_msg_id,
            exec_id,
            command,
            working_dir,
        } => {
            let bridge_name = {
                let s = session_entry.lock().await;
                resolve_shell_mcp_tool_name(&s.declared_tool_names)
            };
            if let Some(name) = bridge_name {
                let mut args_map = serde_json::Map::new();
                args_map.insert("command".into(), Value::String(command));
                if !working_dir.is_empty() {
                    args_map.insert("workdir".into(), Value::String(working_dir));
                }
                log::debug!("[CursorAgent] bridging background shell exec → MCP tool {name}");
                return surface_mcp_tool_call(
                    exec_msg_id,
                    &exec_id,
                    name,
                    "",
                    Value::Object(args_map),
                    session_entry,
                    session_manager,
                    session_key,
                    writer,
                    events,
                    saw_tool_call,
                )
                .await;
            }
            let s = session_entry.lock().await;
            send_exec_frame(
                &s.stream,
                proto::encode_exec_background_shell_rejected(
                    exec_msg_id,
                    &exec_id,
                    &command,
                    &working_dir,
                    "cc-switch 不执行 Cursor 内置 shell 工具。",
                ),
                "background_shell_reject",
            );
            ExecHandleResult::Continue
        }
        ExecServerEvent::Fetch {
            exec_msg_id,
            exec_id,
            url,
        } => {
            let declared = {
                let s = session_entry.lock().await;
                s.declared_tool_names.clone()
            };
            if let Some((name, args)) =
                bridge_builtin_tool(BuiltinBridgeKind::Fetch, &declared, "", &url, "")
            {
                log::debug!("[CursorAgent] bridging built-in fetch → MCP {name}");
                return surface_mcp_tool_call(
                    exec_msg_id,
                    &exec_id,
                    name,
                    "",
                    args,
                    session_entry,
                    session_manager,
                    session_key,
                    writer,
                    events,
                    saw_tool_call,
                )
                .await;
            }
            let s = session_entry.lock().await;
            let _ = s.stream.send_frame(proto::encode_exec_fetch_error(
                exec_msg_id,
                &exec_id,
                &url,
                "cc-switch 不执行 Cursor 内置 fetch 工具。",
            ));
            ExecHandleResult::Continue
        }
        ExecServerEvent::WriteShellStdin {
            exec_msg_id,
            exec_id,
        } => {
            let s = session_entry.lock().await;
            let _ = s
                .stream
                .send_frame(proto::encode_exec_write_shell_stdin_error(
                    exec_msg_id,
                    &exec_id,
                    "cc-switch 不执行 Cursor 内置 shell 工具。",
                ));
            ExecHandleResult::Continue
        }
        ExecServerEvent::Mcp {
            exec_msg_id,
            exec_id,
            tool_name,
            tool_call_id,
            args,
        } => {
            let declared = {
                let s = session_entry.lock().await;
                s.declared_tool_names.clone()
            };
            let original_name = tool_name.clone();
            let (tool_name, args) =
                if let Some(remapped) = bridge_mcp_exec_tool(&declared, &tool_name, args.clone()) {
                    cursor_debug::log_bridge(&original_name, &remapped.0);
                    remapped
                } else {
                    (tool_name, args)
                };
            if !is_declared_tool(&declared, &tool_name) {
                let msg = format!("Tool `{tool_name}` is not in the client tool inventory");
                cursor_debug::log_exec("mcp_unmapped", &exec_id, &tool_name);
                let s = session_entry.lock().await;
                let _ = s.stream.send_frame(proto::encode_exec_mcp_error(
                    exec_msg_id,
                    &exec_id,
                    &msg,
                ));
                *unmapped_tool_name = Some(tool_name);
                return ExecHandleResult::Continue;
            }
            surface_mcp_tool_call(
                exec_msg_id,
                &exec_id,
                tool_name,
                &tool_call_id,
                args,
                session_entry,
                session_manager,
                session_key,
                writer,
                events,
                saw_tool_call,
            )
            .await
        }
    }
}

async fn handle_kv_event(kv: KvServerEvent, session_entry: &Arc<Mutex<CursorSession>>) {
    match kv {
        KvServerEvent::GetBlob {
            kv_id,
            blob_id,
            request_metadata,
        } => {
            let key = hex::encode(&blob_id);
            let s = session_entry.lock().await;
            let blob = s.blob_store.get(&key).cloned().unwrap_or_else(Bytes::new);
            let frame = proto::encode_kv_get_blob_result(kv_id, &blob, request_metadata.as_deref());
            let _ = s.stream.send_frame(frame);
        }
        KvServerEvent::SetBlob {
            kv_id,
            blob_id,
            blob_data,
            request_metadata,
        } => {
            let key = hex::encode(&blob_id);
            let mut s = session_entry.lock().await;
            s.blob_store.insert(key, blob_data);
            let frame = proto::encode_kv_set_blob_result(kv_id, request_metadata.as_deref());
            let _ = s.stream.send_frame(frame);
        }
    }
}

// ─── Streaming wrapper ─────────────────────────────────────────────────────

async fn run_stream(
    account: &CursorAccountData,
    access_token: &str,
    session_manager: CursorSessionManager,
    session_key: String,
    plan: AgentRunPlan,
    images: Vec<proto::EncodedImage>,
    format: CursorResponseFormat,
    response_model: String,
) -> Result<ProxyResponse, ProxyError> {
    let max_attempts = tool_retry_max(&plan);
    let base_user_text = plan.user_text.clone();
    let account = account.clone();
    let access_token = access_token.to_string();
    let body = stream! {
        let mut writer = AgentSseWriter::new(
            response_model,
            format,
            estimate_input_tokens(&base_user_text),
        );
        if matches!(format, CursorResponseFormat::OpenAiResponses) {
            session_manager
                .bind_response_id(writer.message_id(), &session_key)
                .await;
        }
        for e in writer.start_events() {
            yield Ok::<_, io::Error>(Bytes::from(e));
        }

        let mut attempt_user_text = plan.user_text.clone();
        for attempt in 1..=max_attempts {
            let session_entry = match acquire_or_open_session(
                &account,
                &access_token,
                &session_manager,
                &session_key,
                &plan.model_id,
                &attempt_user_text,
                plan.system_prompt.as_deref(),
                &plan.tools,
                images.clone(),
                &plan.tool_results,
                &plan.working_directory,
                plan.previous_response_id.as_deref(),
            )
            .await
            {
                Ok(s) => s,
                Err(e) => {
                    for ev in writer.error_events(&format!("{e}")) {
                        yield Ok::<_, io::Error>(Bytes::from(ev));
                    }
                    for ev in writer.done_events() {
                        yield Ok::<_, io::Error>(Bytes::from(ev));
                    }
                    return;
                }
            };

            let initial_status = {
                let s = session_entry.lock().await;
                s.stream.status()
            };
            if !initial_status.is_success() {
                let msg = {
                    let mut s = session_entry.lock().await;
                    upstream_status_message(&mut s.stream).await
                };
                session_manager
                    .release(session_entry.clone(), SessionState::Closed)
                    .await;
                for ev in writer.error_events(&msg) {
                    yield Ok::<_, io::Error>(Bytes::from(ev));
                }
                for ev in writer.done_events() {
                    yield Ok::<_, io::Error>(Bytes::from(ev));
                }
                return;
            }

            let buffer_this_attempt = max_attempts > 1
                && !plan.tools.is_empty()
                && plan.tool_results.is_empty()
                && attempt < max_attempts;
            let mut attempt_buffer: Vec<String> = Vec::new();

            let mut filter = ComposerMarkerFilter::default();
            let mut exec_dedup = ExecDedup::default();
            let mut saw_tool_call = false;
            let mut unmapped_tool_name: Option<String> = None;
            let stream_started = Instant::now();
            let mut last_meaningful = Instant::now();
            let mut turn_ended = false;
            let mut stream_ended_naturally = false;

            loop {
                let stall_left = MEANINGFUL_PROGRESS_TIMEOUT.saturating_sub(last_meaningful.elapsed());
                let safety_left = STREAM_SAFETY_TIMEOUT.saturating_sub(stream_started.elapsed());
                let wait = stall_left.min(safety_left);
                if wait.is_zero() {
                    let msg = if safety_left.is_zero() {
                        "Cursor AgentService 响应超时：整流超过 300s"
                    } else {
                        "Cursor AgentService 响应超时：上游在 90s 内无实质输出"
                    };
                    for ev in writer.error_events(msg) {
                        yield Ok::<_, io::Error>(Bytes::from(ev));
                    }
                    session_manager
                        .release(session_entry.clone(), SessionState::Closed)
                        .await;
                    for ev in writer.done_events() {
                        yield Ok::<_, io::Error>(Bytes::from(ev));
                    }
                    return;
                }

                let frame_result = tokio::time::timeout(wait, async {
                    let mut s = session_entry.lock().await;
                    s.stream.next_frame().await
                })
                .await;

                let frame = match frame_result {
                    Ok(Ok(Some(f))) => f,
                    Ok(Ok(None)) => {
                        stream_ended_naturally = true;
                        break;
                    }
                    Ok(Err(e)) => {
                        for ev in writer.error_events(&format!("{e}")) {
                            yield Ok::<_, io::Error>(Bytes::from(ev));
                        }
                        session_manager
                            .release(session_entry.clone(), SessionState::Closed)
                            .await;
                        for ev in writer.done_events() {
                            yield Ok::<_, io::Error>(Bytes::from(ev));
                        }
                        return;
                    }
                    Err(_) => {
                        for ev in writer.error_events(
                            "Cursor AgentService 响应超时：上游在 90s 内无实质输出",
                        ) {
                            yield Ok::<_, io::Error>(Bytes::from(ev));
                        }
                        session_manager
                            .release(session_entry.clone(), SessionState::Closed)
                            .await;
                        for ev in writer.done_events() {
                            yield Ok::<_, io::Error>(Bytes::from(ev));
                        }
                        return;
                    }
                };

                let mut meaningful = false;

                if let Some(kv) = decode_kv_server_event(&frame.payload) {
                    handle_kv_event(kv, &session_entry).await;
                    meaningful = true;
                }

                if let Some(exec) = decode_exec_server_event(&frame.payload) {
                    let mut sink = Vec::new();
                    let exec_result = handle_exec_event(
                        exec,
                        &mut exec_dedup,
                        &session_entry,
                        &session_manager,
                        &session_key,
                        &mut writer,
                        &mut sink,
                        &mut saw_tool_call,
                        &mut unmapped_tool_name,
                    )
                    .await;
                    if buffer_this_attempt {
                        attempt_buffer.append(&mut sink);
                    } else {
                        for out in sink {
                            yield Ok::<_, io::Error>(Bytes::from(out));
                        }
                    }
                    if exec_result == ExecHandleResult::StopForTool {
                        if buffer_this_attempt {
                            for out in attempt_buffer {
                                yield Ok::<_, io::Error>(Bytes::from(out));
                            }
                        }
                        session_manager
                            .release(session_entry.clone(), SessionState::AwaitingToolResult)
                            .await;
                        for ev in writer.done_events() {
                            yield Ok::<_, io::Error>(Bytes::from(ev));
                        }
                        return;
                    }
                    meaningful = true;
                }

                for delta in decode_agent_server_message(&frame.payload) {
                    if is_meaningful_interaction(&delta) {
                        meaningful = true;
                    }
                    if matches!(delta, InteractionDelta::TurnEnded) {
                        turn_ended = true;
                        break;
                    }
                    let lines = emit_interaction_delta(delta, &mut writer, &mut filter);
                    if buffer_this_attempt {
                        attempt_buffer.extend(lines);
                    } else {
                        for out in lines {
                            yield Ok::<_, io::Error>(Bytes::from(out));
                        }
                    }
                }

                if meaningful {
                    last_meaningful = Instant::now();
                }
            }

            for ev in filter.flush() {
                let mut sink = Vec::new();
                push_marker_event(&mut writer, ev, &mut sink);
                if buffer_this_attempt {
                    attempt_buffer.extend(sink);
                } else {
                    for out in sink {
                        yield Ok::<_, io::Error>(Bytes::from(out));
                    }
                }
            }

            let has_pending = {
                let s = session_entry.lock().await;
                !s.pending_tool_calls.is_empty()
            };

            if has_pending {
                session_manager
                    .release(session_entry.clone(), SessionState::AwaitingToolResult)
                    .await;
                for ev in writer.done_events() {
                    yield Ok::<_, io::Error>(Bytes::from(ev));
                }
                return;
            }

            session_manager
                .release(session_entry.clone(), SessionState::Closed)
                .await;

            let should_retry_missing = !saw_tool_call
                && !plan.tools.is_empty()
                && plan.tool_results.is_empty()
                && attempt < max_attempts
                && (turn_ended || stream_ended_naturally);

            let should_retry_unmapped = unmapped_tool_name.is_some()
                && !saw_tool_call
                && plan.tool_results.is_empty()
                && attempt < max_attempts
                && (turn_ended || stream_ended_naturally);

            if should_retry_missing {
                cursor_debug::log_retry("missing_tool_call", attempt, max_attempts);
                log::warn!(
                    "[CursorAgent] 工具回合未产生 tool_call，重试 {}/{}",
                    attempt,
                    max_attempts
                );
                attempt_buffer.clear();
                writer.reset_for_retry();
                attempt_user_text =
                    retry_prompt_after_missing_tool(&base_user_text, attempt, max_attempts);
                continue;
            }

            if should_retry_unmapped {
                let tool = unmapped_tool_name.clone().unwrap_or_default();
                cursor_debug::log_retry("unmapped_tool", attempt, max_attempts);
                log::warn!(
                    "[CursorAgent] 不可映射工具 `{tool}`，重试 {}/{}",
                    attempt,
                    max_attempts
                );
                attempt_buffer.clear();
                writer.reset_for_retry();
                attempt_user_text = retry_prompt_after_unmapped_tool(
                    &base_user_text,
                    &tool,
                    attempt,
                    max_attempts,
                );
                continue;
            }

            if buffer_this_attempt {
                for out in attempt_buffer {
                    yield Ok::<_, io::Error>(Bytes::from(out));
                }
            }

            for ev in writer.done_events() {
                yield Ok::<_, io::Error>(Bytes::from(ev));
            }
            return;
        }

        for ev in writer.done_events() {
            yield Ok::<_, io::Error>(Bytes::from(ev));
        }
    };
    Ok(ProxyResponse::local_sse(Box::pin(body)))
}

// ─── Non-streaming wrapper ─────────────────────────────────────────────────

async fn run_non_stream(
    account: &CursorAccountData,
    access_token: &str,
    session_manager: CursorSessionManager,
    session_key: String,
    plan: AgentRunPlan,
    images: Vec<proto::EncodedImage>,
    format: CursorResponseFormat,
    response_model: String,
) -> Result<ProxyResponse, ProxyError> {
    let max_attempts = tool_retry_max(&plan);
    let base_user_text = plan.user_text.clone();
    let mut attempt_user_text = plan.user_text.clone();
    let mut writer = AgentSseWriter::new(
        response_model.clone(),
        format,
        estimate_input_tokens(&base_user_text),
    );
    if matches!(format, CursorResponseFormat::OpenAiResponses) {
        session_manager
            .bind_response_id(writer.message_id(), &session_key)
            .await;
    }
    let _ = writer.start_events();

    for attempt in 1..=max_attempts {
        let mut filter = ComposerMarkerFilter::default();
        let mut saw_tool_call = false;
        let mut unmapped_tool_name: Option<String> = None;
        let session_entry = acquire_or_open_session(
            account,
            access_token,
            &session_manager,
            &session_key,
            &plan.model_id,
            &attempt_user_text,
            plan.system_prompt.as_deref(),
            &plan.tools,
            images.clone(),
            &plan.tool_results,
            &plan.working_directory,
            plan.previous_response_id.as_deref(),
        )
        .await?;

        let initial_status = {
            let s = session_entry.lock().await;
            s.stream.status()
        };
        if !initial_status.is_success() {
            let msg = {
                let mut s = session_entry.lock().await;
                upstream_status_message(&mut s.stream).await
            };
            session_manager
                .release(session_entry.clone(), SessionState::Closed)
                .await;
            return Err(ProxyError::UpstreamError {
                status: initial_status.as_u16(),
                body: Some(msg),
            });
        }

        let drive_end = drive_loop(
            session_entry.clone(),
            &session_manager,
            &session_key,
            &mut writer,
            &mut filter,
            &mut saw_tool_call,
            &mut unmapped_tool_name,
            false,
        )
        .await?;

        match drive_end {
            DriveEnd::ParkedForTool => {
                session_manager
                    .release(session_entry, SessionState::AwaitingToolResult)
                    .await;
                break;
            }
            DriveEnd::Completed => {
                session_manager
                    .release(session_entry, SessionState::Closed)
                    .await;

                if saw_tool_call || plan.tools.is_empty() || !plan.tool_results.is_empty() {
                    break;
                }
                if attempt >= max_attempts {
                    break;
                }
                if let Some(tool) = unmapped_tool_name.clone() {
                    cursor_debug::log_retry("unmapped_tool", attempt, max_attempts);
                    log::warn!(
                        "[CursorAgent] 不可映射工具 `{tool}`，重试 {}/{}",
                        attempt,
                        max_attempts
                    );
                    writer.reset_for_retry();
                    attempt_user_text = retry_prompt_after_unmapped_tool(
                        &base_user_text,
                        &tool,
                        attempt,
                        max_attempts,
                    );
                } else {
                    cursor_debug::log_retry("missing_tool_call", attempt, max_attempts);
                    log::warn!(
                        "[CursorAgent] 工具回合未产生 tool_call，重试 {}/{}",
                        attempt,
                        max_attempts
                    );
                    writer.reset_for_retry();
                    attempt_user_text =
                        retry_prompt_after_missing_tool(&base_user_text, attempt, max_attempts);
                }
            }
        }
    }

    let _ = writer.done_events();
    let body = Bytes::from(serde_json::to_vec(&writer.json_response()).unwrap_or_default());
    Ok(ProxyResponse::local_json(StatusCode::OK, body))
}

#[cfg(test)]
mod tests {
    use crate::proxy::providers::cursor_tool_bridge::{
        bridge_builtin_tool, bridge_grep_tool, resolve_shell_mcp_tool_name, BuiltinBridgeKind,
    };
    use super::proto;
    use super::*;
    use bytes::Bytes;

    #[test]
    fn exec_dedup_rejects_duplicate_request_context() {
        let mut dedup = ExecDedup::default();
        let event = ExecServerEvent::RequestContext {
            exec_msg_id: 3,
            exec_id: "exec-dup".to_string(),
        };
        assert!(dedup.track(&event));
        assert!(!dedup.track(&event));
    }

    #[test]
    fn request_context_ack_uses_empty_tools_slice() {
        let frame = proto::encode_request_context_response(5, "exec-rc", &[]);
        let tool = McpToolDef {
            name: "Bash".to_string(),
            description: "bash".to_string(),
            input_schema: Bytes::from_static(br#"{"type":"object"}"#),
            provider_identifier: "cc-switch".to_string(),
            tool_name: "Bash".to_string(),
        };
        let with_tools = proto::encode_request_context_response(5, "exec-rc", &[tool]);
        assert!(frame.len() < with_tools.len());
    }

    #[test]
    fn half_close_only_when_no_tools_or_results() {
        assert!(should_half_close_after_run_request(0, 0));
        assert!(!should_half_close_after_run_request(1, 0));
        assert!(!should_half_close_after_run_request(0, 1));
    }

    #[test]
    fn resolve_shell_mcp_tool_name_prefers_bash_alias() {
        let names = vec!["WebSearch".into(), "Bash".into()];
        assert_eq!(
            resolve_shell_mcp_tool_name(&names).as_deref(),
            Some("Bash")
        );
    }

    #[test]
    fn bridge_read_maps_to_declared_tool() {
        let names = vec!["Read".into(), "Bash".into()];
        let (name, args) = bridge_builtin_tool(
            BuiltinBridgeKind::Read,
            &names,
            "/tmp/x",
            "",
            "",
        )
        .unwrap();
        assert_eq!(name, "Read");
        assert_eq!(args.get("path").and_then(Value::as_str), Some("/tmp/x"));
    }

    #[test]
    fn bridge_grep_maps_pattern_and_path() {
        let names = vec!["Grep".into()];
        let (name, args) = bridge_grep_tool(
            &names,
            "fn main",
            "/proj",
            "*.rs",
            "content",
            true,
            Some(50),
        )
        .unwrap();
        assert_eq!(name, "Grep");
        assert_eq!(args.get("pattern").and_then(Value::as_str), Some("fn main"));
        assert_eq!(args.get("path").and_then(Value::as_str), Some("/proj"));
        assert_eq!(args.get("glob").and_then(Value::as_str), Some("*.rs"));
        assert!(args.get("case_insensitive").and_then(Value::as_bool).unwrap());
    }

    #[test]
    fn rich_request_context_larger_than_empty() {
        let empty = proto::encode_request_context_response(1, "exec-a", &[]);
        let rich = proto::encode_rich_request_context_response(1, "exec-a", "/proj");
        assert!(rich.len() > empty.len());
    }

    #[test]
    fn tool_retry_max_single_on_tool_results() {
        let plan = AgentRunPlan {
            system_prompt: None,
            user_text: "x".into(),
            tools: vec![McpToolDef {
                name: "Bash".into(),
                description: String::new(),
                input_schema: Bytes::new(),
                provider_identifier: "cc".into(),
                tool_name: "Bash".into(),
            }],
            images: vec![],
            tool_results: vec![ToolResultBlock {
                tool_call_id: "c1".into(),
                content: "ok".into(),
                is_error: false,
            }],
            model_id: "m".into(),
            previous_response_id: None,
            working_directory: ".".into(),
        };
        assert_eq!(tool_retry_max(&plan), 1);
    }
}

/// Whether to half-close the AgentService client stream right after RunRequest.
fn should_half_close_after_run_request(tool_count: usize, tool_result_count: usize) -> bool {
    tool_count == 0 && tool_result_count == 0
}

fn is_meaningful_interaction(delta: &InteractionDelta) -> bool {
    match delta {
        InteractionDelta::TokenDelta(n) if *n > 0 => true,
        InteractionDelta::TokenDelta(_)
            | InteractionDelta::Heartbeat
            | InteractionDelta::KvServerMessage
            | InteractionDelta::ToolCallStarted
            | InteractionDelta::ToolCallCompleted
            | InteractionDelta::Unknown(_)
            | InteractionDelta::TurnEnded => false,
        _ => true,
    }
}

fn send_exec_frame(stream: &CursorH2Stream, frame: Bytes, label: &str) {
    if let Err(e) = stream.send_frame(frame) {
        log::warn!("[CursorAgent] {label} 写入失败：{e}");
    }
}

async fn surface_mcp_tool_call(
    exec_msg_id: u64,
    exec_id: &str,
    tool_name: String,
    tool_call_id: &str,
    args: Value,
    session_entry: &Arc<Mutex<CursorSession>>,
    session_manager: &CursorSessionManager,
    session_key: &str,
    writer: &mut AgentSseWriter,
    events: &mut Vec<String>,
    saw_tool_call: &mut bool,
) -> ExecHandleResult {
    let client_call_id = if tool_call_id.is_empty() {
        format!("call_{}", uuid::Uuid::new_v4().simple())
    } else {
        tool_call_id.to_string()
    };
    {
        let mut s = session_entry.lock().await;
        s.pending_tool_calls.insert(
            client_call_id.clone(),
            PendingToolCall {
                exec_msg_id,
                exec_id: exec_id.to_string(),
                tool_name: tool_name.clone(),
            },
        );
    }
    session_manager
        .bind_tool_call_id(&client_call_id, session_key)
        .await;
    let arguments_json = serde_json::to_string(&args).unwrap_or_else(|_| "{}".to_string());
    let tc = CapturedToolCall {
        id: client_call_id,
        name: tool_name,
        arguments_json,
    };
    events.extend(writer.event(&AgentEvent::ToolCall(tc)));
    *saw_tool_call = true;
    ExecHandleResult::StopForTool
}

fn tool_retry_max(plan: &AgentRunPlan) -> usize {
    if plan.tools.is_empty() || !plan.tool_results.is_empty() {
        return 1;
    }
    std::env::var("CC_SWITCH_CURSOR_TOOL_RETRY_ATTEMPTS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|n| (1..=5).contains(n))
        .unwrap_or(DEFAULT_TOOL_RETRY_ATTEMPTS)
}
