//! Kiro OAuth provider for Claude-compatible proxy requests.

use crate::commands::KiroOAuthState;
use crate::provider::Provider;
use crate::proxy::hyper_client::ProxyResponse;
use crate::proxy::ProxyError;
use bytes::{Buf, Bytes, BytesMut};
use futures::{Stream, StreamExt};
use http::StatusCode;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use tauri::Manager;

use super::kiro_oauth_auth::{machine_id_from_refresh_token, KiroAccountData};

const DEFAULT_KIRO_VERSION: &str = "2.3.0";
const DEFAULT_SYSTEM_VERSION: &str = "macos";
const DEFAULT_NODE_VERSION: &str = "22.22.0";
const BUILDER_ID_PROFILE_ARN: &str =
    "arn:aws:codewhisperer:us-east-1:638616132270:profile/AAAACCCCXXXX";

pub async fn forward_kiro_claude(
    app_handle: Option<&tauri::AppHandle>,
    provider: &Provider,
    body: &Value,
) -> Result<ProxyResponse, ProxyError> {
    let Some(app_handle) = app_handle else {
        return Err(ProxyError::AuthError(
            "Kiro OAuth 认证不可用（无 AppHandle）".to_string(),
        ));
    };

    let state = app_handle.state::<KiroOAuthState>();
    let manager = state.0.read().await;
    let account_id = provider
        .meta
        .as_ref()
        .and_then(|m| m.managed_account_id_for("kiro_oauth"));

    let resolved_account = match account_id.as_deref() {
        Some(id) => manager.get_account(id).await,
        None => manager.get_default_account().await,
    }
    .ok_or_else(|| ProxyError::AuthError("未找到可用 Kiro OAuth 账号".to_string()))?;

    let token = match account_id.as_deref() {
        Some(id) => manager.get_valid_token_for_account(id).await,
        None => manager.get_valid_token().await,
    }
    .map_err(|e| ProxyError::AuthError(format!("Kiro OAuth 认证失败: {e}")))?;

    let request_body = anthropic_to_kiro_request(body, &resolved_account)?;
    let response = send_kiro_request(&resolved_account, &token, request_body.clone()).await?;

    let response = if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        manager
            .invalidate_cached_token(&resolved_account.account_id)
            .await;
        let token = manager
            .get_valid_token_for_account(&resolved_account.account_id)
            .await
            .map_err(|e| ProxyError::AuthError(format!("Kiro OAuth 认证失败: {e}")))?;
        send_kiro_request(&resolved_account, &token, request_body).await?
    } else {
        response
    };

    if !response.status().is_success() {
        let status = response.status().as_u16();
        let body = response.text().await.ok();
        return Err(ProxyError::UpstreamError { status, body });
    }

    let model = body
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("claude-sonnet-4-5");
    let is_stream = body
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if is_stream {
        let stream = kiro_event_stream_to_anthropic_sse(response.bytes_stream(), model.to_string());
        Ok(ProxyResponse::local_sse(Box::pin(stream)))
    } else {
        let bytes = response
            .bytes()
            .await
            .map_err(|e| ProxyError::ForwardFailed(format!("读取 Kiro 响应失败: {e}")))?;
        let message = kiro_event_bytes_to_anthropic_json(&bytes, model);
        Ok(ProxyResponse::local_json(
            StatusCode::OK,
            Bytes::from(serde_json::to_vec(&message).unwrap_or_default()),
        ))
    }
}

async fn send_kiro_request(
    account: &KiroAccountData,
    token: &str,
    body: Value,
) -> Result<reqwest::Response, ProxyError> {
    let region = if account.api_region.trim().is_empty() {
        "us-east-1"
    } else {
        account.api_region.as_str()
    };
    let host = format!("q.{region}.amazonaws.com");
    let url = format!("https://{host}/generateAssistantResponse");
    let machine_id = account
        .machine_id
        .clone()
        .unwrap_or_else(|| machine_id_from_refresh_token(&account.refresh_token));
    let x_amz_user_agent = format!("aws-sdk-js/1.0.34 KiroIDE-{DEFAULT_KIRO_VERSION}-{machine_id}");
    let user_agent = format!(
        "aws-sdk-js/1.0.34 ua/2.1 os/{DEFAULT_SYSTEM_VERSION} lang/js md/nodejs#{DEFAULT_NODE_VERSION} api/codewhispererstreaming#1.0.34 m/E KiroIDE-{DEFAULT_KIRO_VERSION}-{machine_id}"
    );

    reqwest::Client::new()
        .post(url)
        .header("content-type", "application/json")
        .header("Connection", "close")
        .header("x-amzn-codewhisperer-optout", "true")
        .header("x-amzn-kiro-agent-mode", "vibe")
        .header("x-amz-user-agent", x_amz_user_agent)
        .header("user-agent", user_agent)
        .header("host", host)
        .header("amz-sdk-invocation-id", uuid::Uuid::new_v4().to_string())
        .header("amz-sdk-request", "attempt=1; max=3")
        .header("Authorization", format!("Bearer {token}"))
        .json(&body)
        .send()
        .await
        .map_err(|e| ProxyError::ForwardFailed(format!("Kiro 请求失败: {e}")))
}

fn anthropic_to_kiro_request(body: &Value, account: &KiroAccountData) -> Result<Value, ProxyError> {
    let model = body
        .get("model")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ProxyError::InvalidRequest("missing model".to_string()))?;
    let model_id = map_model(model)
        .ok_or_else(|| ProxyError::InvalidRequest(format!("Kiro OAuth 不支持该模型: {model}")))?;
    let messages = body
        .get("messages")
        .and_then(|v| v.as_array())
        .ok_or_else(|| ProxyError::InvalidRequest("missing messages".to_string()))?;
    if messages.is_empty() {
        return Err(ProxyError::InvalidRequest("messages is empty".to_string()));
    }

    let last_user_idx = messages
        .iter()
        .rposition(|m| m.get("role").and_then(|v| v.as_str()) == Some("user"))
        .ok_or_else(|| ProxyError::InvalidRequest("missing user message".to_string()))?;

    let tools = convert_tools(body.get("tools"));
    let (content, images, tool_results) =
        parse_user_content(messages[last_user_idx].get("content"));
    let current_message = json!({
        "userInputMessage": {
            "userInputMessageContext": {
                "envState": env_state(),
                "toolResults": tool_results,
                "tools": tools
            },
            "content": content,
            "modelId": model_id,
            "images": images,
            "origin": "AI_EDITOR"
        }
    });

    let mut history = Vec::new();
    for msg in &messages[..last_user_idx] {
        let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
        match role {
            "user" => {
                let (content, images, tool_results) = parse_user_content(msg.get("content"));
                history.push(json!({
                    "userInputMessage": {
                        "content": content,
                        "modelId": model_id,
                        "origin": "AI_EDITOR",
                        "images": images,
                        "userInputMessageContext": {
                            "envState": env_state(),
                            "toolResults": tool_results
                        }
                    }
                }));
            }
            "assistant" => {
                let (content, tool_uses) = parse_assistant_content(msg.get("content"));
                history.push(json!({
                    "assistantResponseMessage": {
                        "content": content,
                        "toolUses": tool_uses
                    }
                }));
            }
            _ => {}
        }
    }

    let profile_arn = account
        .profile_arn
        .clone()
        .unwrap_or_else(|| BUILDER_ID_PROFILE_ARN.to_string());
    Ok(json!({
        "conversationState": {
            "agentTaskType": "vibe",
            "chatTriggerType": "MANUAL",
            "currentMessage": current_message,
            "conversationId": conversation_id(body),
            "history": history
        },
        "profileArn": profile_arn
    }))
}

fn map_model(model: &str) -> Option<&'static str> {
    let m = model.to_ascii_lowercase();
    if m.contains("sonnet") {
        if m.contains("4-6") || m.contains("4.6") {
            Some("claude-sonnet-4.6")
        } else if m.contains("4-5") || m.contains("4.5") {
            Some("claude-sonnet-4.5")
        } else {
            Some("claude-sonnet-4.5")
        }
    } else if m.contains("opus") {
        if m.contains("4-7") || m.contains("4.7") {
            Some("claude-opus-4.7")
        } else if m.contains("4-6") || m.contains("4.6") {
            Some("claude-opus-4.6")
        } else if m.contains("4-5") || m.contains("4.5") {
            Some("claude-opus-4.5")
        } else {
            Some("claude-opus-4.5")
        }
    } else if m.contains("haiku") {
        Some("claude-haiku-4.5")
    } else {
        None
    }
}

fn env_state() -> Value {
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "/".to_string());
    json!({
        "operatingSystem": DEFAULT_SYSTEM_VERSION,
        "currentWorkingDirectory": cwd
    })
}

fn conversation_id(body: &Value) -> String {
    let metadata = body.get("metadata");
    if let Some(session_id) = metadata
        .and_then(|m| m.get("session_id"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        return stable_uuid_like(session_id);
    }
    if let Some(user_id) = metadata
        .and_then(|m| m.get("user_id"))
        .and_then(|v| v.as_str())
    {
        if let Some(pos) = user_id.find("session_") {
            return stable_uuid_like(&user_id[pos + 8..]);
        }
        return stable_uuid_like(user_id);
    }
    uuid::Uuid::new_v4().to_string()
}

fn stable_uuid_like(input: &str) -> String {
    if uuid::Uuid::parse_str(input).is_ok() {
        return input.to_string();
    }
    let digest = Sha256::digest(input.as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    uuid::Uuid::from_bytes(bytes).to_string()
}

fn convert_tools(tools: Option<&Value>) -> Vec<Value> {
    tools
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|tool| {
                    let name = tool.get("name")?.as_str()?.to_string();
                    let description = tool
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let schema = tool
                        .get("input_schema")
                        .cloned()
                        .unwrap_or_else(|| json!({"type":"object","properties":{}}));
                    Some(json!({
                        "toolSpecification": {
                            "name": name,
                            "description": description,
                            "inputSchema": { "json": normalize_schema(schema) }
                        }
                    }))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn normalize_schema(mut schema: Value) -> Value {
    if let Some(obj) = schema.as_object_mut() {
        obj.remove("$schema");
        obj.entry("type").or_insert_with(|| json!("object"));
        obj.entry("properties").or_insert_with(|| json!({}));
        obj.entry("required").or_insert_with(|| json!([]));
        obj.entry("additionalProperties")
            .or_insert_with(|| json!(true));
    }
    schema
}

fn parse_user_content(content: Option<&Value>) -> (String, Vec<Value>, Vec<Value>) {
    let mut text = String::new();
    let mut images = Vec::new();
    let mut tool_results = Vec::new();
    match content {
        Some(Value::String(s)) => text.push_str(s),
        Some(Value::Array(items)) => {
            for item in items {
                match item.get("type").and_then(|v| v.as_str()) {
                    Some("text") => {
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(item.get("text").and_then(|v| v.as_str()).unwrap_or(""));
                    }
                    Some("image") => {
                        if let Some(source) = item.get("source") {
                            let data = source.get("data").and_then(|v| v.as_str()).unwrap_or("");
                            let media = source
                                .get("media_type")
                                .and_then(|v| v.as_str())
                                .unwrap_or("image/png");
                            let format = media.split('/').nth(1).unwrap_or("png");
                            images.push(json!({"format": format, "source": {"bytes": data}}));
                        }
                    }
                    Some("tool_result") => {
                        let id = item
                            .get("tool_use_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let is_error = item
                            .get("is_error")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        let result_text = flatten_content_to_text(item.get("content"));
                        tool_results.push(json!({
                            "toolUseId": id,
                            "content": [{"text": result_text}],
                            "status": if is_error { "error" } else { "success" },
                            "isError": is_error
                        }));
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    (text, images, tool_results)
}

fn parse_assistant_content(content: Option<&Value>) -> (String, Vec<Value>) {
    let mut text = String::new();
    let mut tool_uses = Vec::new();
    match content {
        Some(Value::String(s)) => text.push_str(s),
        Some(Value::Array(items)) => {
            for item in items {
                match item.get("type").and_then(|v| v.as_str()) {
                    Some("text") => {
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(item.get("text").and_then(|v| v.as_str()).unwrap_or(""));
                    }
                    Some("tool_use") => {
                        tool_uses.push(json!({
                            "toolUseId": item.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                            "name": item.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                            "input": item.get("input").cloned().unwrap_or_else(|| json!({}))
                        }));
                    }
                    Some("thinking") => {}
                    _ => {}
                }
            }
        }
        _ => {}
    }
    (text, tool_uses)
}

fn flatten_content_to_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|v| {
                if v.get("type").and_then(|t| t.as_str()) == Some("text") {
                    v.get("text").and_then(|t| t.as_str()).map(str::to_string)
                } else {
                    v.as_str().map(str::to_string)
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Some(v) => v.to_string(),
        None => String::new(),
    }
}

#[derive(Debug, Clone)]
struct KiroFrame {
    headers: HashMap<String, String>,
    payload: Vec<u8>,
}

fn parse_frames(buffer: &mut BytesMut) -> Vec<KiroFrame> {
    let mut frames = Vec::new();
    loop {
        if buffer.len() < 12 {
            break;
        }
        let total_length =
            u32::from_be_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]) as usize;
        let header_length =
            u32::from_be_bytes([buffer[4], buffer[5], buffer[6], buffer[7]]) as usize;
        if total_length < 16 || total_length > 16 * 1024 * 1024 {
            buffer.advance(1);
            continue;
        }
        if buffer.len() < total_length {
            break;
        }
        let frame = buffer.split_to(total_length);
        let headers_start = 12;
        let headers_end = headers_start + header_length;
        if headers_end > frame.len().saturating_sub(4) {
            continue;
        }
        let headers = parse_event_headers(&frame[headers_start..headers_end]);
        let payload = frame[headers_end..frame.len() - 4].to_vec();
        frames.push(KiroFrame { headers, payload });
    }
    frames
}

fn parse_event_headers(mut bytes: &[u8]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    while !bytes.is_empty() {
        let name_len = bytes[0] as usize;
        bytes = &bytes[1..];
        if bytes.len() < name_len + 1 {
            break;
        }
        let name = String::from_utf8_lossy(&bytes[..name_len]).to_string();
        bytes = &bytes[name_len..];
        let value_type = bytes[0];
        bytes = &bytes[1..];
        let value = match value_type {
            7 => {
                if bytes.len() < 2 {
                    break;
                }
                let len = u16::from_be_bytes([bytes[0], bytes[1]]) as usize;
                bytes = &bytes[2..];
                if bytes.len() < len {
                    break;
                }
                let value = String::from_utf8_lossy(&bytes[..len]).to_string();
                bytes = &bytes[len..];
                value
            }
            6 => {
                if bytes.len() < 8 {
                    break;
                }
                let value = i64::from_be_bytes([
                    bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
                ])
                .to_string();
                bytes = &bytes[8..];
                value
            }
            _ => break,
        };
        out.insert(name, value);
    }
    out
}

fn frame_event_type(frame: &KiroFrame) -> Option<&str> {
    frame.headers.get(":event-type").map(String::as_str)
}

fn frame_message_type(frame: &KiroFrame) -> Option<&str> {
    frame.headers.get(":message-type").map(String::as_str)
}

#[derive(Default)]
struct SseBuilder {
    message_id: String,
    model: String,
    text_started: bool,
    text_stopped: bool,
    next_index: i32,
    tool_indices: HashMap<String, i32>,
    output_tokens: i32,
}

impl SseBuilder {
    fn new(model: String) -> Self {
        Self {
            message_id: format!("msg_{}", uuid::Uuid::new_v4().to_string().replace('-', "")),
            model,
            ..Default::default()
        }
    }

    fn initial(&self) -> Bytes {
        sse(
            "message_start",
            json!({
                "type": "message_start",
                "message": {
                    "id": self.message_id,
                    "type": "message",
                    "role": "assistant",
                    "model": self.model,
                    "content": [],
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": { "input_tokens": 0, "output_tokens": 0 }
                }
            }),
        )
    }

    fn assistant_delta(&mut self, text: &str) -> Vec<Bytes> {
        if text.is_empty() {
            return Vec::new();
        }
        self.output_tokens += estimate_tokens(text);
        let mut out = Vec::new();
        if !self.text_started {
            self.text_started = true;
            out.push(sse(
                "content_block_start",
                json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            ));
            self.next_index = self.next_index.max(1);
        }
        out.push(sse(
            "content_block_delta",
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":text}}),
        ));
        out
    }

    fn tool_delta(&mut self, payload: &Value) -> Vec<Bytes> {
        let id = payload
            .get("toolUseId")
            .and_then(|v| v.as_str())
            .unwrap_or("toolu_kiro");
        let name = payload
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("tool");
        let input = payload.get("input").and_then(|v| v.as_str()).unwrap_or("");
        let stop = payload
            .get("stop")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let mut out = Vec::new();
        if self.text_started && !self.text_stopped {
            self.text_stopped = true;
            out.push(sse(
                "content_block_stop",
                json!({"type":"content_block_stop","index":0}),
            ));
        }

        let index = if let Some(index) = self.tool_indices.get(id).copied() {
            index
        } else {
            let index = self.next_index;
            self.next_index += 1;
            self.tool_indices.insert(id.to_string(), index);
            out.push(sse(
                "content_block_start",
                json!({"type":"content_block_start","index":index,"content_block":{"type":"tool_use","id":id,"name":name,"input":{}}}),
            ));
            index
        };

        if !input.is_empty() {
            out.push(sse(
                "content_block_delta",
                json!({"type":"content_block_delta","index":index,"delta":{"type":"input_json_delta","partial_json":input}}),
            ));
        }
        if stop {
            out.push(sse(
                "content_block_stop",
                json!({"type":"content_block_stop","index":index}),
            ));
        }
        out
    }

    fn final_events(&mut self) -> Vec<Bytes> {
        let mut out = Vec::new();
        if self.text_started && !self.text_stopped {
            self.text_stopped = true;
            out.push(sse(
                "content_block_stop",
                json!({"type":"content_block_stop","index":0}),
            ));
        }
        let stop_reason = if self.tool_indices.is_empty() {
            "end_turn"
        } else {
            "tool_use"
        };
        out.push(sse(
            "message_delta",
            json!({"type":"message_delta","delta":{"stop_reason":stop_reason,"stop_sequence":null},"usage":{"input_tokens":0,"output_tokens":self.output_tokens}}),
        ));
        out.push(sse("message_stop", json!({"type":"message_stop"})));
        out
    }
}

fn kiro_event_stream_to_anthropic_sse(
    stream: impl Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
    model: String,
) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send {
    async_stream::stream! {
        let mut buffer = BytesMut::new();
        let mut builder = SseBuilder::new(model);
        yield Ok(builder.initial());
        tokio::pin!(stream);
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| std::io::Error::other(e.to_string()))?;
            buffer.extend_from_slice(&chunk);
            for frame in parse_frames(&mut buffer) {
                for bytes in process_frame_to_sse(&mut builder, &frame) {
                    yield Ok(bytes);
                }
            }
        }
        for bytes in builder.final_events() {
            yield Ok(bytes);
        }
    }
}

fn process_frame_to_sse(builder: &mut SseBuilder, frame: &KiroFrame) -> Vec<Bytes> {
    match frame_message_type(frame) {
        Some("error") | Some("exception") => {
            let text = String::from_utf8_lossy(&frame.payload).to_string();
            builder.assistant_delta(&format!("\n[Kiro error] {text}"))
        }
        _ => match frame_event_type(frame) {
            Some("assistantResponseEvent") => {
                let payload: Value = serde_json::from_slice(&frame.payload).unwrap_or(Value::Null);
                let text = payload
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                builder.assistant_delta(text)
            }
            Some("toolUseEvent") => {
                let payload: Value = serde_json::from_slice(&frame.payload).unwrap_or(Value::Null);
                builder.tool_delta(&payload)
            }
            _ => Vec::new(),
        },
    }
}

fn kiro_event_bytes_to_anthropic_json(bytes: &[u8], model: &str) -> Value {
    let mut buffer = BytesMut::from(bytes);
    let mut text = String::new();
    let mut tools: HashMap<String, (String, String)> = HashMap::new();
    for frame in parse_frames(&mut buffer) {
        match frame_event_type(&frame) {
            Some("assistantResponseEvent") => {
                let payload: Value = serde_json::from_slice(&frame.payload).unwrap_or(Value::Null);
                if let Some(chunk) = payload.get("content").and_then(|v| v.as_str()) {
                    text.push_str(chunk);
                }
            }
            Some("toolUseEvent") => {
                let payload: Value = serde_json::from_slice(&frame.payload).unwrap_or(Value::Null);
                let id = payload
                    .get("toolUseId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("toolu_kiro")
                    .to_string();
                let name = payload
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("tool")
                    .to_string();
                let input = payload
                    .get("input")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let entry = tools.entry(id).or_insert((name, String::new()));
                entry.1.push_str(&input);
            }
            _ => {}
        }
    }

    let mut content = Vec::new();
    if !text.is_empty() {
        content.push(json!({"type":"text","text":text}));
    }
    for (id, (name, input)) in tools {
        let parsed_input = serde_json::from_str::<Value>(&input).unwrap_or_else(|_| json!({}));
        content.push(json!({"type":"tool_use","id":id,"name":name,"input":parsed_input}));
    }
    let stop_reason = if content
        .iter()
        .any(|v| v.get("type").and_then(|t| t.as_str()) == Some("tool_use"))
    {
        "tool_use"
    } else {
        "end_turn"
    };
    json!({
        "id": format!("msg_{}", uuid::Uuid::new_v4().to_string().replace('-', "")),
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": content,
        "stop_reason": stop_reason,
        "stop_sequence": null,
        "usage": {"input_tokens": 0, "output_tokens": estimate_tokens(&text)}
    })
}

fn sse(event: &str, data: Value) -> Bytes {
    Bytes::from(format!(
        "event: {event}\ndata: {}\n\n",
        serde_json::to_string(&data).unwrap_or_default()
    ))
}

fn estimate_tokens(text: &str) -> i32 {
    ((text.chars().count() as f64) / 4.0).ceil() as i32
}

#[allow(dead_code)]
fn default_profile_arn_for_builder_id() -> &'static str {
    BUILDER_ID_PROFILE_ARN
}
