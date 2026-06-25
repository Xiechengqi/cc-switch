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
use super::cursor_event_emitter::{
    AgentEvent, AgentSseWriter, CapturedToolCall, ComposerMarkerFilter, MarkerEvent,
};
use super::cursor_h2_client::{agent_connect_headers, CursorH2Stream};
use super::cursor_image::load_images;
use super::cursor_oauth_auth::CursorAccountData;
use super::cursor_protocol::{cursor_identity_headers, CursorResponseFormat};
use super::cursor_request_builder::{AgentRunPlan, ToolResultBlock};
use super::cursor_session::{CursorSession, CursorSessionManager, PendingToolCall, SessionState};
use crate::proxy::hyper_client::ProxyResponse;
use crate::proxy::ProxyError;
use async_stream::stream;
use bytes::Bytes;
use http::StatusCode;
use std::collections::{HashMap, HashSet};
use std::io;
use std::sync::Arc;
use tokio::sync::Mutex;

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

    let images = load_images(plan.images).await?;

    log::debug!(
        "[CursorAgent] AgentService stream={} tools={} model={}",
        want_stream,
        plan.tools.len(),
        plan.model_id
    );

    let session_entry = acquire_or_open_session(
        account,
        access_token,
        session_manager,
        &session_key,
        &plan.model_id,
        &plan.user_text,
        plan.system_prompt.as_deref(),
        &plan.tools,
        images,
        &plan.tool_results,
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

    if want_stream {
        run_stream(
            session_entry,
            session_manager.clone(),
            session_key,
            format,
            response_model,
        )
        .await
    } else {
        run_non_stream(
            session_entry,
            session_manager.clone(),
            session_key,
            format,
            response_model,
        )
        .await
    }
}

// ─── Session acquisition ───────────────────────────────────────────────────

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
) -> Result<Arc<Mutex<CursorSession>>, ProxyError> {
    if !tool_results.is_empty() {
        if let Some(entry) = session_manager.acquire(session_key).await {
            let resumed = try_resume_with_tool_results(&entry, tool_results).await;
            if resumed {
                return Ok(entry);
            }
            session_manager
                .release(entry.clone(), SessionState::Closed)
                .await;
        }
        log::warn!("[CursorAgent] 未找到匹配的 parked session，使用冷恢复方式继续处理 tool_result");
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
    let session = session_manager
        .open(session_key.to_string(), stream_handle, blob_store)
        .await;
    // Half-close the client-stream after the initial RunRequest. Cursor's
    // AgentService is client-streaming + server-streaming: if the writer
    // stays open, upstream may wait forever for more client data before it
    // starts processing a tool-bearing request. For a fresh session with no
    // pending tool results, the initial RunRequest is the only client frame
    // this turn, so we signal EOF now. The server-stream remains readable.
    {
        let mut s = session.lock().await;
        s.stream.close_writer();
    }
    Ok(session)
}

async fn try_resume_with_tool_results(
    entry: &Arc<Mutex<CursorSession>>,
    tool_results: &[ToolResultBlock],
) -> bool {
    let mut s = entry.lock().await;
    if !tool_results
        .iter()
        .all(|tr| s.pending_tool_calls.contains_key(&tr.tool_call_id))
    {
        return false;
    }
    for tr in tool_results {
        if let Some(pending) = s.pending_tool_calls.remove(&tr.tool_call_id) {
            let frame = proto::encode_exec_mcp_result(
                pending.exec_msg_id,
                &pending.exec_id,
                &tr.content,
                tr.is_error,
            );
        if let Err(e) = s.stream.send_frame(frame) {
            log::warn!("[CursorAgent] 写 tool_result 失败：{e}");
            return false;
        }
        }
    }
    let all_done = s.pending_tool_calls.is_empty();
    if all_done {
        // All tool results sent — signal client-stream EOF so upstream
        // knows no more MCP results are coming on this turn.
        s.stream.close_writer();
    }
    all_done
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

/// Run the read/write loop until the cursor stream signals turn_ended or the
/// h2 stream closes. Returns the collected SSE event strings (already
/// formatted by `AgentSseWriter`) and the final session state to apply.
async fn drive_loop(
    session_entry: Arc<Mutex<CursorSession>>,
    session_manager: &CursorSessionManager,
    session_key: &str,
    writer: &mut AgentSseWriter,
    filter: &mut ComposerMarkerFilter,
) -> Result<Vec<String>, ProxyError> {
    let mut events: Vec<String> = Vec::new();
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

        for delta in decode_agent_server_message(&frame.payload) {
            match delta {
                InteractionDelta::Text(t) => {
                    for ev in filter.push(&t) {
                        push_marker_event(writer, ev, &mut events);
                    }
                }
                InteractionDelta::Thinking(t) => {
                    events.extend(writer.event(&AgentEvent::Thinking(t)));
                }
                InteractionDelta::ThinkingComplete => {
                    events.extend(writer.event(&AgentEvent::ThinkingComplete));
                }
                InteractionDelta::ToolCallStarted | InteractionDelta::ToolCallCompleted => {}
                InteractionDelta::TokenDelta(_) | InteractionDelta::Heartbeat => {}
                InteractionDelta::Unknown(_) | InteractionDelta::KvServerMessage => {}
                InteractionDelta::TurnEnded => {
                    for ev in filter.flush() {
                        push_marker_event(writer, ev, &mut events);
                    }
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
                    return Ok(events);
                }
            }
        }

        if let Some(exec) = decode_exec_server_event(&frame.payload) {
            let should_return_for_tool = handle_exec_event(
                exec,
                &mut exec_dedup,
                &session_entry,
                session_manager,
                session_key,
                writer,
                &mut events,
            )
            .await;
            if should_return_for_tool {
                session_manager
                    .release(session_entry.clone(), SessionState::AwaitingToolResult)
                    .await;
                return Ok(events);
            }
        }

        if let Some(kv) = decode_kv_server_event(&frame.payload) {
            handle_kv_event(kv, &session_entry).await;
        }
    }

    // Stream ended without TurnEnded.
    for ev in filter.flush() {
        push_marker_event(writer, ev, &mut events);
    }
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
    Ok(events)
}

fn push_marker_event(writer: &mut AgentSseWriter, ev: MarkerEvent, sink: &mut Vec<String>) {
    match ev {
        MarkerEvent::Text(txt) => sink.extend(writer.event(&AgentEvent::Text(txt))),
        MarkerEvent::ToolCall(tc) => sink.extend(writer.event(&AgentEvent::ToolCall(tc))),
    }
}

async fn handle_exec_event(
    exec: ExecServerEvent,
    exec_dedup: &mut ExecDedup,
    session_entry: &Arc<Mutex<CursorSession>>,
    session_manager: &CursorSessionManager,
    session_key: &str,
    writer: &mut AgentSseWriter,
    events: &mut Vec<String>,
) -> bool {
    if !exec_dedup.track(&exec) {
        log::debug!(
            "[CursorAgent] skip duplicate exec event: {}",
            exec.dedup_key()
        );
        return false;
    }

    match exec {
        ExecServerEvent::RequestContext {
            exec_msg_id,
            exec_id,
        } => {
            log::debug!("[CursorAgent] RequestContext ack (empty; tools already in RunRequest)");
            let reply = proto::encode_request_context_response(exec_msg_id, &exec_id, &[]);
            let s = session_entry.lock().await;
            let _ = s.stream.send_frame(reply);
            false
        }
        ExecServerEvent::Read {
            exec_msg_id,
            exec_id,
            path,
        } => {
            let s = session_entry.lock().await;
            let _ = s.stream.send_frame(proto::encode_exec_read_rejected(
                exec_msg_id,
                &exec_id,
                &path,
                "cc-switch 不执行 Cursor 内置 read 工具。请改用客户端 MCP 工具。",
            ));
            false
        }
        ExecServerEvent::Write {
            exec_msg_id,
            exec_id,
            path,
        } => {
            let s = session_entry.lock().await;
            let _ = s.stream.send_frame(proto::encode_exec_write_rejected(
                exec_msg_id,
                &exec_id,
                &path,
                "cc-switch 不执行 Cursor 内置 write 工具。请改用客户端 MCP 工具。",
            ));
            false
        }
        ExecServerEvent::Delete {
            exec_msg_id,
            exec_id,
            path,
        } => {
            let s = session_entry.lock().await;
            let _ = s.stream.send_frame(proto::encode_exec_delete_rejected(
                exec_msg_id,
                &exec_id,
                &path,
                "cc-switch 不执行 Cursor 内置 delete 工具。请改用客户端 MCP 工具。",
            ));
            false
        }
        ExecServerEvent::Ls {
            exec_msg_id,
            exec_id,
            path,
        } => {
            let s = session_entry.lock().await;
            let _ = s.stream.send_frame(proto::encode_exec_ls_rejected(
                exec_msg_id,
                &exec_id,
                &path,
                "cc-switch 不执行 Cursor 内置 ls 工具。请改用客户端 MCP 工具。",
            ));
            false
        }
        ExecServerEvent::Grep {
            exec_msg_id,
            exec_id,
        } => {
            let s = session_entry.lock().await;
            let _ = s.stream.send_frame(proto::encode_exec_grep_error(
                exec_msg_id,
                &exec_id,
                "cc-switch 不执行 Cursor 内置 grep 工具。",
            ));
            false
        }
        ExecServerEvent::Diagnostics {
            exec_msg_id,
            exec_id,
        } => {
            let s = session_entry.lock().await;
            let _ = s
                .stream
                .send_frame(proto::encode_exec_diagnostics_result(exec_msg_id, &exec_id));
            false
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
            let s = session_entry.lock().await;
            let _ = s.stream.send_frame(proto::encode_exec_shell_rejected(
                exec_msg_id,
                &exec_id,
                &command,
                &working_dir,
                "cc-switch 不执行 Cursor 内置 shell 工具。请改用客户端 MCP 工具。",
            ));
            false
        }
        ExecServerEvent::BackgroundShell {
            exec_msg_id,
            exec_id,
            command,
            working_dir,
        } => {
            let s = session_entry.lock().await;
            let _ = s
                .stream
                .send_frame(proto::encode_exec_background_shell_rejected(
                    exec_msg_id,
                    &exec_id,
                    &command,
                    &working_dir,
                    "cc-switch 不执行 Cursor 内置 shell 工具。",
                ));
            false
        }
        ExecServerEvent::Fetch {
            exec_msg_id,
            exec_id,
            url,
        } => {
            let s = session_entry.lock().await;
            let _ = s.stream.send_frame(proto::encode_exec_fetch_error(
                exec_msg_id,
                &exec_id,
                &url,
                "cc-switch 不执行 Cursor 内置 fetch 工具。",
            ));
            false
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
            false
        }
        ExecServerEvent::Mcp {
            exec_msg_id,
            exec_id,
            tool_name,
            tool_call_id,
            args,
        } => {
            let client_call_id = if tool_call_id.is_empty() {
                format!("call_{}", uuid::Uuid::new_v4().simple())
            } else {
                tool_call_id
            };
            {
                let mut s = session_entry.lock().await;
                s.pending_tool_calls.insert(
                    client_call_id.clone(),
                    PendingToolCall {
                        exec_msg_id,
                        exec_id: exec_id.clone(),
                        tool_name: tool_name.clone(),
                    },
                );
            }
            session_manager
                .bind_tool_call_id(&client_call_id, session_key)
                .await;
            let tc = CapturedToolCall {
                id: client_call_id,
                name: tool_name,
                arguments_json: args.to_string(),
            };
            events.extend(writer.event(&AgentEvent::ToolCall(tc)));
            true
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
    session_entry: Arc<Mutex<CursorSession>>,
    session_manager: CursorSessionManager,
    session_key: String,
    format: CursorResponseFormat,
    response_model: String,
) -> Result<ProxyResponse, ProxyError> {
    let body = stream! {
        let mut writer = AgentSseWriter::new(response_model, format, 0);
        if matches!(format, CursorResponseFormat::OpenAiResponses) {
            session_manager
                .bind_response_id(writer.message_id(), &session_key)
                .await;
        }
        for e in writer.start_events() {
            yield Ok::<_, io::Error>(Bytes::from(e));
        }
        let mut filter = ComposerMarkerFilter::default();
        let mut exec_dedup = ExecDedup::default();
        loop {
            let next = {
                let mut s = session_entry.lock().await;
                s.stream.next_frame().await
            };
            let frame = match next {
                Ok(Some(f)) => f,
                Ok(None) => break,
                Err(e) => {
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
            };

            for delta in decode_agent_server_message(&frame.payload) {
                match delta {
                    InteractionDelta::Text(t) => {
                        for ev in filter.push(&t) {
                            let mut sink = Vec::new();
                            push_marker_event(&mut writer, ev, &mut sink);
                            for out in sink {
                                yield Ok::<_, io::Error>(Bytes::from(out));
                            }
                        }
                    }
                    InteractionDelta::Thinking(t) => {
                        for ev in writer.event(&AgentEvent::Thinking(t)) {
                            yield Ok::<_, io::Error>(Bytes::from(ev));
                        }
                    }
                    InteractionDelta::ThinkingComplete => {
                        for ev in writer.event(&AgentEvent::ThinkingComplete) {
                            yield Ok::<_, io::Error>(Bytes::from(ev));
                        }
                    }
                    InteractionDelta::ToolCallStarted | InteractionDelta::ToolCallCompleted => {}
                    InteractionDelta::TokenDelta(_) | InteractionDelta::Heartbeat => {}
                    InteractionDelta::Unknown(_) | InteractionDelta::KvServerMessage => {}
                    InteractionDelta::TurnEnded => {
                        for ev in filter.flush() {
                            let mut sink = Vec::new();
                            push_marker_event(&mut writer, ev, &mut sink);
                            for out in sink {
                                yield Ok::<_, io::Error>(Bytes::from(out));
                            }
                        }
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
                        for ev in writer.done_events() {
                            yield Ok::<_, io::Error>(Bytes::from(ev));
                        }
                        return;
                    }
                }
            }

            if let Some(exec) = decode_exec_server_event(&frame.payload) {
                let mut sink = Vec::new();
                let should_return_for_tool = handle_exec_event(
                    exec,
                    &mut exec_dedup,
                    &session_entry,
                    &session_manager,
                    &session_key,
                    &mut writer,
                    &mut sink,
                )
                .await;
                for out in sink {
                    yield Ok::<_, io::Error>(Bytes::from(out));
                }
                if should_return_for_tool {
                    session_manager
                        .release(session_entry.clone(), SessionState::AwaitingToolResult)
                        .await;
                    for ev in writer.done_events() {
                        yield Ok::<_, io::Error>(Bytes::from(ev));
                    }
                    return;
                }
            }

            if let Some(kv) = decode_kv_server_event(&frame.payload) {
                handle_kv_event(kv, &session_entry).await;
            }
        }

        for ev in filter.flush() {
            let mut sink = Vec::new();
            push_marker_event(&mut writer, ev, &mut sink);
            for out in sink {
                yield Ok::<_, io::Error>(Bytes::from(out));
            }
        }
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
        for ev in writer.done_events() {
            yield Ok::<_, io::Error>(Bytes::from(ev));
        }
    };
    Ok(ProxyResponse::local_sse(Box::pin(body)))
}

// ─── Non-streaming wrapper ─────────────────────────────────────────────────

async fn run_non_stream(
    session_entry: Arc<Mutex<CursorSession>>,
    session_manager: CursorSessionManager,
    session_key: String,
    format: CursorResponseFormat,
    response_model: String,
) -> Result<ProxyResponse, ProxyError> {
    let mut writer = AgentSseWriter::new(response_model.clone(), format, 0);
    if matches!(format, CursorResponseFormat::OpenAiResponses) {
        session_manager
            .bind_response_id(writer.message_id(), &session_key)
            .await;
    }
    let mut filter = ComposerMarkerFilter::default();
    let _ = writer.start_events();
    let _ = drive_loop(
        session_entry,
        &session_manager,
        &session_key,
        &mut writer,
        &mut filter,
    )
    .await?;
    let _ = writer.done_events();
    let body = Bytes::from(serde_json::to_vec(&writer.json_response()).unwrap_or_default());
    Ok(ProxyResponse::local_json(StatusCode::OK, body))
}

#[cfg(test)]
mod tests {
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
}
