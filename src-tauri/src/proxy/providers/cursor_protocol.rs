//! Cursor private API protocol helpers.
//!
//! The upstream endpoint is HTTP/2 + Connect-RPC protobuf. This module keeps
//! the Cursor-specific transport, protobuf framing, streaming decode, and
//! outward SSE shape in one place so Claude and Codex providers can share it.

use super::cursor_oauth_auth::CursorAccountData;
use crate::provider::Provider;
use crate::proxy::{hyper_client::ProxyResponse, ProxyError};
use async_stream::stream;
use bytes::{Bytes, BytesMut};
use flate2::read::GzDecoder;
use futures::{Stream, StreamExt};
use http::header::HeaderName;
use http::StatusCode;
use http_body_util::Full;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{Read, Result as IoResult};

const DEFAULT_API_BASE_URL: &str = "https://api2.cursor.sh";
const CHAT_PATH: &str = "/aiserver.v1.ChatService/StreamUnifiedChatWithTools";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorResponseFormat {
    AnthropicMessages,
    OpenAiResponses,
    OpenAiChatCompletions,
}

#[derive(Debug, Clone)]
pub struct CursorRequestContext {
    pub account: CursorAccountData,
    pub access_token: String,
    pub body: Value,
    pub conversation_id: Option<String>,
}

pub async fn send_cursor_request(ctx: &CursorRequestContext) -> Result<ProxyResponse, ProxyError> {
    let url = format!("{DEFAULT_API_BASE_URL}{CHAT_PATH}");
    let encoded = encode_cursor_chat_request(&ctx.body, ctx.conversation_id.as_deref());
    let content_length = encoded.len().to_string();

    // Cursor's Connect-RPC endpoint returns AWS ALB 464 over HTTP/1.1 for this
    // protobuf stream. Use a hyper client that advertises h2 via ALPN and then
    // refuses HTTP/1.1, instead of h2 prior knowledge which fails with this ALB.
    let https = hyper_rustls::HttpsConnectorBuilder::new()
        .with_webpki_roots()
        .https_only()
        .enable_http2()
        .build();
    let mut builder = Client::builder(TokioExecutor::new());
    builder.http2_only(true);
    builder.http2_adaptive_window(true);
    let client: Client<_, Full<Bytes>> = builder.build(https);

    let uri = url
        .parse::<http::Uri>()
        .map_err(|e| ProxyError::ForwardFailed(format!("解析 Cursor URL 失败: {e}")))?;
    let mut req = http::Request::builder()
        .method(http::Method::POST)
        .uri(uri)
        .body(Full::new(encoded))
        .map_err(|e| ProxyError::ForwardFailed(format!("创建 Cursor 请求失败: {e}")))?;
    for (key, value) in build_cursor_headers(&ctx.account, &ctx.access_token) {
        let name = HeaderName::from_bytes(key.as_bytes())
            .map_err(|e| ProxyError::ForwardFailed(format!("Cursor 请求头名称无效: {e}")))?;
        let value = http::HeaderValue::from_str(&value)
            .map_err(|e| ProxyError::ForwardFailed(format!("Cursor 请求头值无效: {e}")))?;
        req.headers_mut().insert(name, value);
    }
    req.headers_mut().insert(
        http::header::CONTENT_LENGTH,
        http::HeaderValue::from_str(&content_length)
            .map_err(|e| ProxyError::ForwardFailed(format!("Cursor Content-Length 无效: {e}")))?,
    );

    client
        .request(req)
        .await
        .map(ProxyResponse::Hyper)
        .map_err(|e| ProxyError::ForwardFailed(format!("Cursor HTTP/2 请求失败: {e}")))
}

pub fn response_to_sse_stream(
    response: ProxyResponse,
    model: String,
    format: CursorResponseFormat,
) -> impl Stream<Item = IoResult<Bytes>> + Send + 'static {
    stream! {
        let mut decoder = CursorStreamingDecoder::default();
        let mut upstream = response.bytes_stream();
        let mut aggregated_text = String::new();
        let mut aggregated_reasoning = String::new();
        let mut writer = SseWriter::new(model, format);

        for event in writer.start_events() {
            yield Ok(Bytes::from(event));
        }

        while let Some(chunk) = upstream.next().await {
            match chunk {
                Ok(bytes) => {
                    for delta in decoder.feed(&bytes) {
                        aggregated_text.push_str(&delta.text_delta);
                        aggregated_reasoning.push_str(&delta.reasoning_delta);
                        for event in writer.delta_events(&delta) {
                            yield Ok(Bytes::from(event));
                        }
                    }
                }
                Err(err) => {
                    for event in writer.error_events(&format!("Cursor 响应流读取失败: {err}")) {
                        yield Ok(Bytes::from(event));
                    }
                    return;
                }
            }
        }

        let drained = decoder.drain();
        if !drained.reasoning_delta.is_empty() {
            aggregated_reasoning.push_str(&drained.reasoning_delta);
            let delta = CursorDelta {
                text_delta: String::new(),
                reasoning_delta: drained.reasoning_delta,
            };
            for event in writer.delta_events(&delta) {
                yield Ok(Bytes::from(event));
            }
        }

        let error = decoder.finish();
        for event in writer.done_events(&aggregated_text, &aggregated_reasoning, error.as_deref()) {
            yield Ok(Bytes::from(event));
        }
    }
}

pub async fn response_error_body(response: ProxyResponse) -> Option<String> {
    response
        .bytes()
        .await
        .ok()
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

pub async fn response_to_json(
    response: ProxyResponse,
    model: &str,
    format: CursorResponseFormat,
) -> Result<(StatusCode, Bytes), ProxyError> {
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|e| ProxyError::ForwardFailed(format!("读取 Cursor 响应失败: {e}")))?;
    if !status.is_success() {
        return Ok((
            StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
            bytes,
        ));
    }
    let decoded = decode_cursor_response(&bytes);
    if let Some(error) = decoded
        .error
        .filter(|_| decoded.text.is_empty() && decoded.reasoning.is_empty())
    {
        return Ok((
            StatusCode::BAD_GATEWAY,
            Bytes::from(
                serde_json::to_vec(&json!({
                    "error": {
                        "message": format!("cursor upstream rejected the request: {error}"),
                        "type": "upstream_error",
                        "provider": "cursor"
                    }
                }))
                .unwrap_or_default(),
            ),
        ));
    }
    let body = match format {
        CursorResponseFormat::OpenAiResponses => {
            build_openai_response_json(model, &decoded.text, &decoded.reasoning)
        }
        CursorResponseFormat::OpenAiChatCompletions => {
            build_chat_completion_json(model, &decoded.text, &decoded.reasoning)
        }
        CursorResponseFormat::AnthropicMessages => {
            build_anthropic_message_json(model, &decoded.text, &decoded.reasoning)
        }
    };
    Ok((
        StatusCode::OK,
        Bytes::from(serde_json::to_vec(&body).unwrap_or_default()),
    ))
}

pub fn requested_model(body: &Value) -> String {
    body.get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("cursor-default")
        .to_string()
}

pub(crate) fn prepare_cursor_codex_body(provider: &Provider, body: &Value) -> (Value, String) {
    let response_model = requested_model(body);
    let (mapped_body, original_model, mapped_model) =
        crate::proxy::model_mapper::apply_model_mapping(body.clone(), provider);
    if let (Some(original), Some(mapped)) = (original_model.as_deref(), mapped_model.as_deref()) {
        log::debug!("[Cursor] Codex 模型映射: {original} -> {mapped}");
    }
    (mapped_body, response_model)
}

fn encode_cursor_chat_request(body: &Value, conversation_id: Option<&str>) -> Bytes {
    let model = normalise_model(
        body.get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("default"),
    );
    let messages = messages_from_body(body);
    let conversation_id = conversation_id
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let mut request = Vec::new();
    let mut entries = Vec::new();

    for msg in &messages {
        if msg.role == "system" {
            continue;
        }
        let role = if msg.role == "assistant" { 2 } else { 1 };
        let id = uuid::Uuid::new_v4().to_string();
        entries.push((id.clone(), role));
        encode_bytes_field_vec(
            &mut request,
            1,
            &encode_chat_message(&msg.content, role, &id, 2),
        );
    }

    encode_varint_field_vec(&mut request, 2, 1);
    encode_bytes_field_vec(&mut request, 3, &[]);
    encode_varint_field_vec(&mut request, 4, 1);
    encode_bytes_field_vec(&mut request, 5, &encode_model_msg(&model));
    encode_bytes_field_vec(&mut request, 8, b"");
    encode_varint_field_vec(&mut request, 13, 1);
    encode_bytes_field_vec(&mut request, 15, &encode_cursor_setting());
    encode_varint_field_vec(&mut request, 19, 1);
    encode_bytes_field_vec(&mut request, 23, conversation_id.as_bytes());
    encode_bytes_field_vec(&mut request, 26, &encode_metadata());
    encode_varint_field_vec(&mut request, 27, 1);
    for (id, role) in entries {
        encode_bytes_field_vec(&mut request, 30, &encode_message_id(&id, role));
    }
    encode_varint_field_vec(&mut request, 35, 0);
    encode_varint_field_vec(&mut request, 38, 0);
    encode_varint_field_vec(&mut request, 46, 2);
    encode_bytes_field_vec(&mut request, 47, b"");
    encode_varint_field_vec(&mut request, 48, 0);
    encode_varint_field_vec(&mut request, 49, 0);
    encode_varint_field_vec(&mut request, 51, 0);
    encode_varint_field_vec(&mut request, 53, 1);
    encode_bytes_field_vec(&mut request, 54, b"agent");

    let mut wrapped = Vec::new();
    encode_bytes_field_vec(&mut wrapped, 1, &request);
    Bytes::from(connect_frame(&wrapped))
}

#[derive(Debug)]
struct ChatMessageInput {
    role: String,
    content: String,
}

fn messages_from_body(body: &Value) -> Vec<ChatMessageInput> {
    let mut out = Vec::new();
    if let Some(system) = body.get("system") {
        push_text(&mut out, "system", system);
    }
    if let Some(input) = body.get("input") {
        if let Some(s) = input.as_str() {
            push_text(&mut out, "user", &Value::String(s.to_string()));
        } else if let Some(items) = input.as_array() {
            for item in items {
                if let Some(s) = item.as_str() {
                    push_text(&mut out, "user", &Value::String(s.to_string()));
                } else {
                    let role = item.get("role").and_then(|v| v.as_str()).unwrap_or("user");
                    push_text(&mut out, role, item.get("content").unwrap_or(item));
                }
            }
        }
    }
    if let Some(messages) = body.get("messages").and_then(|v| v.as_array()) {
        for msg in messages {
            let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("user");
            push_text(&mut out, role, msg.get("content").unwrap_or(&Value::Null));
        }
    }
    if out.is_empty() {
        out.push(ChatMessageInput {
            role: "user".to_string(),
            content: String::new(),
        });
    }
    out
}

fn push_text(out: &mut Vec<ChatMessageInput>, role: &str, value: &Value) {
    let text = text_from_content(value);
    if !text.trim().is_empty() {
        out.push(ChatMessageInput {
            role: role.to_string(),
            content: text,
        });
    }
}

fn text_from_content(value: &Value) -> String {
    if let Some(s) = value.as_str() {
        return s.to_string();
    }
    if let Some(arr) = value.as_array() {
        return arr
            .iter()
            .filter_map(|part| {
                part.get("text")
                    .or_else(|| part.get("input_text"))
                    .and_then(|v| v.as_str())
            })
            .collect::<Vec<_>>()
            .join("\n");
    }
    if let Some(obj) = value.as_object() {
        return obj
            .get("text")
            .or_else(|| obj.get("input_text"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
    }
    String::new()
}

fn normalise_model(model: &str) -> String {
    let stripped = model
        .trim()
        .strip_prefix("cursor:")
        .or_else(|| model.trim().strip_prefix("cursor/"))
        .or_else(|| model.trim().strip_prefix("cursor-"))
        .or_else(|| model.trim().strip_prefix("cr/"))
        .unwrap_or(model.trim());
    if stripped.is_empty() {
        return "default".to_string();
    }
    let map = public_model_map();
    map.get(&stripped.to_ascii_lowercase())
        .cloned()
        .unwrap_or_else(|| stripped.to_string())
}

fn public_model_map() -> HashMap<String, String> {
    [
        ("claude-3-5-haiku-20241022", "claude-4.5-haiku"),
        ("claude-3-5-haiku-latest", "claude-4.5-haiku"),
        ("claude-3-5-sonnet-20241022", "claude-4.5-sonnet"),
        ("claude-3-5-sonnet-latest", "claude-4.5-sonnet"),
        ("claude-3-7-sonnet-20250219", "claude-4-sonnet"),
        ("claude-3-7-sonnet-latest", "claude-4-sonnet"),
        ("claude-haiku-4-5", "claude-4.5-haiku"),
        ("claude-haiku-latest", "claude-4.5-haiku"),
        ("claude-sonnet-4-5", "claude-4.5-sonnet"),
        ("claude-sonnet-4-5-latest", "claude-4.5-sonnet"),
        ("claude-sonnet-4-6", "claude-4.6-sonnet-medium"),
        ("claude-sonnet-4-7", "claude-4.6-sonnet-medium"),
        ("claude-sonnet-latest", "claude-4.6-sonnet-medium"),
        ("claude-opus-4-1", "claude-4.5-opus-high"),
        ("claude-opus-4-5", "claude-4.5-opus-high"),
        ("claude-opus-4-6", "claude-4.6-opus-high"),
        ("claude-opus-4-7", "claude-opus-4-7-medium"),
        ("claude-opus-latest", "claude-opus-4-7-medium"),
        ("haiku", "claude-4.5-haiku"),
        ("sonnet", "claude-4.6-sonnet-medium"),
        ("opus", "claude-opus-4-7-medium"),
        ("gpt-5", "gpt-5.5-medium"),
        ("gpt-5-mini", "gpt-5-mini"),
        ("gpt-5.5", "gpt-5.5-medium"),
        ("gpt-5-codex", "gpt-5.3-codex"),
        ("gpt-5.3-codex", "gpt-5.3-codex"),
        ("o3", "gpt-5.5-medium"),
        ("o4-mini", "gpt-5.4-mini-medium"),
        ("o4-high", "gpt-5.4-high"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

fn connect_frame(payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(5 + payload.len());
    frame.push(0);
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(payload);
    frame
}

fn encode_chat_message(content: &str, role: u64, message_id: &str, chat_mode_enum: u64) -> Vec<u8> {
    let mut out = Vec::new();
    encode_bytes_field_vec(&mut out, 1, content.as_bytes());
    encode_varint_field_vec(&mut out, 2, role);
    encode_bytes_field_vec(&mut out, 13, message_id.as_bytes());
    encode_varint_field_vec(&mut out, 47, chat_mode_enum);
    out
}

fn encode_message_id(message_id: &str, role: u64) -> Vec<u8> {
    let mut out = Vec::new();
    encode_bytes_field_vec(&mut out, 1, message_id.as_bytes());
    encode_varint_field_vec(&mut out, 3, role);
    out
}

fn encode_model_msg(model_name: &str) -> Vec<u8> {
    let mut out = Vec::new();
    encode_bytes_field_vec(&mut out, 1, model_name.as_bytes());
    encode_bytes_field_vec(&mut out, 4, &[]);
    out
}

fn encode_cursor_setting() -> Vec<u8> {
    let mut out = Vec::new();
    encode_bytes_field_vec(&mut out, 1, b"cursor\\aisettings");
    out
}

fn encode_metadata() -> Vec<u8> {
    let mut out = Vec::new();
    encode_bytes_field_vec(&mut out, 1, std::env::consts::OS.as_bytes());
    encode_bytes_field_vec(&mut out, 2, std::env::consts::ARCH.as_bytes());
    encode_bytes_field_vec(&mut out, 5, chrono::Utc::now().to_rfc3339().as_bytes());
    out
}

fn encode_varint_field_vec(out: &mut Vec<u8>, field: u64, value: u64) {
    encode_varint(out, (field << 3) | 0);
    encode_varint(out, value);
}

fn encode_bytes_field_vec(out: &mut Vec<u8>, field: u64, bytes: &[u8]) {
    encode_varint(out, (field << 3) | 2);
    encode_varint(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

fn encode_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push(((value & 0x7f) as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn build_cursor_headers(account: &CursorAccountData, token: &str) -> Vec<(String, String)> {
    let mut headers = vec![
        ("Authorization".to_string(), format!("Bearer {token}")),
        (
            "Content-Type".to_string(),
            "application/connect+proto".to_string(),
        ),
        (
            "Accept".to_string(),
            "application/connect+proto".to_string(),
        ),
        ("Accept-Encoding".to_string(), "gzip".to_string()),
        ("Connect-Protocol-Version".to_string(), "1".to_string()),
        ("User-Agent".to_string(), "connect-es/1.6.1".to_string()),
        (
            "x-amzn-trace-id".to_string(),
            uuid::Uuid::new_v4().to_string(),
        ),
    ];
    headers.extend(cursor_identity_headers(account, token));
    headers
}

/// Identity headers Cursor's API endpoints expect (checksum + client metadata).
///
/// Shared between the protobuf chat path and the JSON REST calls (usage query,
/// `/v0/me`). Excludes transport-specific headers like Content-Type/Accept so
/// each caller can set its own.
pub fn cursor_identity_headers(account: &CursorAccountData, token: &str) -> Vec<(String, String)> {
    let machine_id = account.machine_id();
    vec![
        ("x-client-key".to_string(), sha256_hex(token)),
        (
            "x-cursor-checksum".to_string(),
            build_cursor_checksum(token, machine_id),
        ),
        (
            "x-cursor-client-version".to_string(),
            account.client_version().to_string(),
        ),
        ("x-cursor-client-type".to_string(), "ide".to_string()),
        ("x-cursor-client-os".to_string(), cursor_os().to_string()),
        (
            "x-cursor-client-arch".to_string(),
            std::env::consts::ARCH.to_string(),
        ),
        (
            "x-cursor-client-device-type".to_string(),
            "desktop".to_string(),
        ),
        (
            "x-cursor-config-version".to_string(),
            account.config_version(),
        ),
        ("x-cursor-timezone".to_string(), "UTC".to_string()),
        ("x-ghost-mode".to_string(), "true".to_string()),
        ("x-session-id".to_string(), uuid::Uuid::new_v4().to_string()),
        ("x-request-id".to_string(), uuid::Uuid::new_v4().to_string()),
    ]
}

fn cursor_os() -> &'static str {
    match std::env::consts::OS {
        "macos" => "macos",
        "windows" => "windows",
        _ => "linux",
    }
}

fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    hex::encode(hasher.finalize())
}

const URL_SAFE_BASE64: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn jyh_encode(bytes: &[u8]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        let a = bytes[i];
        let b = if i + 1 < bytes.len() { bytes[i + 1] } else { 0 };
        let c = if i + 2 < bytes.len() { bytes[i + 2] } else { 0 };
        out.push(URL_SAFE_BASE64[(a >> 2) as usize] as char);
        out.push(URL_SAFE_BASE64[(((a & 3) << 4) | (b >> 4)) as usize] as char);
        if i + 1 < bytes.len() {
            out.push(URL_SAFE_BASE64[(((b & 15) << 2) | (c >> 6)) as usize] as char);
        }
        if i + 2 < bytes.len() {
            out.push(URL_SAFE_BASE64[(c & 63) as usize] as char);
        }
        i += 3;
    }
    out
}

fn build_cursor_checksum(token: &str, machine_id: &str) -> String {
    let stable_machine_id = if machine_id.is_empty() {
        sha256_hex(&format!("{token}machineId"))
    } else {
        machine_id.to_string()
    };
    let timestamp = (chrono::Utc::now().timestamp_millis() / 1_000_000) as u64;
    let mut buf = [
        ((timestamp >> 40) & 0xff) as u8,
        ((timestamp >> 32) & 0xff) as u8,
        ((timestamp >> 24) & 0xff) as u8,
        ((timestamp >> 16) & 0xff) as u8,
        ((timestamp >> 8) & 0xff) as u8,
        (timestamp & 0xff) as u8,
    ];
    let mut prev = 165u8;
    for (i, b) in buf.iter_mut().enumerate() {
        *b = ((*b ^ prev).wrapping_add((i % 256) as u8)) & 0xff;
        prev = *b;
    }
    format!("{}{}", jyh_encode(&buf), stable_machine_id)
}

#[derive(Default)]
struct CursorStreamingDecoder {
    buffer: BytesMut,
    pending_reasoning: String,
    resolved: ReasoningResolution,
    error: Option<String>,
}

#[derive(Default, PartialEq, Eq)]
enum ReasoningResolution {
    #[default]
    None,
    Split,
    Reasoning,
}

#[derive(Debug, Clone)]
pub struct CursorDelta {
    pub text_delta: String,
    pub reasoning_delta: String,
}

impl CursorStreamingDecoder {
    fn feed(&mut self, chunk: &[u8]) -> Vec<CursorDelta> {
        self.buffer.extend_from_slice(chunk);
        let mut out = Vec::new();
        let mut pos = 0usize;
        while pos + 5 <= self.buffer.len() {
            let frame_type = self.buffer[pos];
            let len = u32::from_be_bytes([
                self.buffer[pos + 1],
                self.buffer[pos + 2],
                self.buffer[pos + 3],
                self.buffer[pos + 4],
            ]) as usize;
            if pos + 5 + len > self.buffer.len() {
                break;
            }
            let mut payload = self.buffer[pos + 5..pos + 5 + len].to_vec();
            pos += 5 + len;
            if frame_type == 1 || frame_type == 3 {
                if let Ok(inflated) = gunzip(&payload) {
                    payload = inflated;
                }
            }
            if frame_type == 0 || frame_type == 1 {
                let parts = extract_from_payload(&payload);
                let mut text_delta = parts.text;
                let mut reasoning_delta = String::new();
                if !parts.reasoning.is_empty() {
                    match self.resolved {
                        ReasoningResolution::Split => text_delta.push_str(&parts.reasoning),
                        ReasoningResolution::Reasoning => {
                            reasoning_delta.push_str(&parts.reasoning)
                        }
                        ReasoningResolution::None => {
                            self.pending_reasoning.push_str(&parts.reasoning);
                            if let Some(split) = self.maybe_split() {
                                reasoning_delta.push_str(&split.reasoning_delta);
                                text_delta = format!("{}{}", split.text_delta, text_delta);
                            }
                        }
                    }
                }
                if !text_delta.is_empty() && self.resolved == ReasoningResolution::None {
                    if let Some(flushed) = self.flush_reasoning() {
                        reasoning_delta = format!("{}{}", flushed.reasoning_delta, reasoning_delta);
                    }
                }
                if !text_delta.is_empty() || !reasoning_delta.is_empty() {
                    out.push(CursorDelta {
                        text_delta,
                        reasoning_delta,
                    });
                }
            } else if frame_type == 2 || frame_type == 3 {
                if let Some(err) = extract_json_error(&payload) {
                    self.error = Some(err);
                }
            }
        }
        let _ = self.buffer.split_to(pos);
        out
    }

    fn maybe_split(&mut self) -> Option<CursorDelta> {
        if self.resolved != ReasoningResolution::None || self.pending_reasoning.is_empty() {
            return None;
        }
        let lower = self.pending_reasoning.to_ascii_lowercase();
        let idx = lower.find("</think>")?;
        let end = idx + "</think>".len();
        let reasoning = self.pending_reasoning[..idx].to_string();
        let text = self.pending_reasoning[end..].trim_start().to_string();
        self.pending_reasoning.clear();
        self.resolved = ReasoningResolution::Split;
        Some(CursorDelta {
            text_delta: text,
            reasoning_delta: reasoning,
        })
    }

    fn flush_reasoning(&mut self) -> Option<CursorDelta> {
        if self.pending_reasoning.is_empty() {
            return None;
        }
        let out = CursorDelta {
            text_delta: String::new(),
            reasoning_delta: std::mem::take(&mut self.pending_reasoning),
        };
        self.resolved = ReasoningResolution::Reasoning;
        Some(out)
    }

    fn drain(&mut self) -> CursorDelta {
        self.flush_reasoning().unwrap_or(CursorDelta {
            text_delta: String::new(),
            reasoning_delta: String::new(),
        })
    }

    fn finish(self) -> Option<String> {
        self.error
    }
}

#[derive(Default)]
struct DecodeResult {
    text: String,
    reasoning: String,
    error: Option<String>,
}

fn decode_cursor_response(data: &[u8]) -> DecodeResult {
    let mut result = DecodeResult::default();
    for (frame_type, mut payload) in read_connect_frames(data) {
        if frame_type == 1 || frame_type == 3 {
            if let Ok(inflated) = gunzip(&payload) {
                payload = inflated;
            }
        }
        if frame_type == 0 || frame_type == 1 {
            let parts = extract_from_payload(&payload);
            result.text.push_str(&parts.text);
            result.reasoning.push_str(&parts.reasoning);
        } else if frame_type == 2 || frame_type == 3 {
            if let Some(err) = extract_json_error(&payload) {
                result.error = Some(err);
            }
        }
    }
    if result.text.is_empty() {
        if let Some(idx) = result.reasoning.to_ascii_lowercase().find("</think>") {
            let end = idx + "</think>".len();
            let text = result.reasoning[end..].trim_start().to_string();
            let reasoning = result.reasoning[..idx].trim().to_string();
            result.text = text;
            result.reasoning = reasoning;
        }
    }
    result.text = result
        .text
        .trim_start()
        .trim_end_matches([' ', '\t'])
        .to_string();
    result.reasoning = result.reasoning.trim().to_string();
    result
}

fn read_connect_frames(data: &[u8]) -> Vec<(u8, Vec<u8>)> {
    let mut frames = Vec::new();
    let mut pos = 0usize;
    while pos + 5 <= data.len() {
        let frame_type = data[pos];
        let len = u32::from_be_bytes([data[pos + 1], data[pos + 2], data[pos + 3], data[pos + 4]])
            as usize;
        pos += 5;
        if pos + len > data.len() {
            break;
        }
        frames.push((frame_type, data[pos..pos + len].to_vec()));
        pos += len;
    }
    frames
}

#[derive(Debug)]
struct ExtractParts {
    text: String,
    reasoning: String,
}

fn extract_from_payload(payload: &[u8]) -> ExtractParts {
    let fields = parse_fields(payload);
    let mut text = String::new();
    let mut reasoning = String::new();
    for field in fields {
        let Some(bytes) = field.bytes else {
            continue;
        };
        if field.wire_type != 2 {
            continue;
        }
        if field.field == 25 {
            reasoning.push_str(&extract_inner_text(&bytes, 0));
        } else if field.field == 1 {
            let direct = String::from_utf8_lossy(&bytes).to_string();
            if is_utf_printable(&direct) && !is_uuid_like(direct.trim()) {
                text.push_str(&direct);
            }
        } else if (field.field == 2 || bytes.len() > 1) && looks_like_proto_start(bytes[0]) {
            let sub = extract_from_payload(&bytes);
            text.push_str(&sub.text);
            reasoning.push_str(&sub.reasoning);
        }
    }
    ExtractParts { text, reasoning }
}

fn extract_inner_text(payload: &[u8], depth: usize) -> String {
    if depth > 4 {
        return String::new();
    }
    let fields = parse_fields(payload);
    if let Some(candidate) = fields
        .iter()
        .find(|f| f.field == 1 && f.wire_type == 2 && f.bytes.is_some())
    {
        let text =
            String::from_utf8_lossy(candidate.bytes.as_deref().unwrap_or_default()).to_string();
        if is_utf_printable(&text) && !is_uuid_like(text.trim()) {
            return text;
        }
    }
    let mut acc = String::new();
    for field in fields {
        if field.wire_type == 2 {
            if let Some(bytes) = field.bytes {
                if bytes.len() > 1 && looks_like_proto_start(bytes[0]) {
                    acc.push_str(&extract_inner_text(&bytes, depth + 1));
                }
            }
        }
    }
    acc
}

struct ProtoFieldRaw {
    field: u64,
    wire_type: u64,
    bytes: Option<Vec<u8>>,
}

fn parse_fields(data: &[u8]) -> Vec<ProtoFieldRaw> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos < data.len() {
        let Some((tag, after_tag)) = decode_varint(data, pos) else {
            break;
        };
        if after_tag <= pos {
            break;
        }
        pos = after_tag;
        let field = tag >> 3;
        let wire_type = tag & 7;
        if wire_type == 0 {
            let Some((_, p)) = decode_varint(data, pos) else {
                break;
            };
            out.push(ProtoFieldRaw {
                field,
                wire_type,
                bytes: None,
            });
            pos = p;
        } else if wire_type == 2 {
            let Some((len, after_len)) = decode_varint(data, pos) else {
                break;
            };
            pos = after_len;
            let len = len as usize;
            if pos + len > data.len() {
                break;
            }
            out.push(ProtoFieldRaw {
                field,
                wire_type,
                bytes: Some(data[pos..pos + len].to_vec()),
            });
            pos += len;
        } else if wire_type == 1 {
            pos = pos.saturating_add(8);
        } else if wire_type == 5 {
            pos = pos.saturating_add(4);
        } else {
            break;
        }
    }
    out
}

fn decode_varint(data: &[u8], mut pos: usize) -> Option<(u64, usize)> {
    let mut value = 0u64;
    let mut shift = 0u32;
    while pos < data.len() {
        let b = data[pos];
        pos += 1;
        value |= ((b & 0x7f) as u64) << shift;
        if (b & 0x80) == 0 {
            return Some((value, pos));
        }
        shift += 7;
        if shift >= 64 {
            return None;
        }
    }
    None
}

fn looks_like_proto_start(byte: u8) -> bool {
    let wire = byte & 0x07;
    byte != 0 && (wire == 0 || wire == 1 || wire == 2 || wire == 5)
}

fn is_uuid_like(text: &str) -> bool {
    text.len() >= 32 && text.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

fn is_utf_printable(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|c| matches!(c, '\t' | '\n' | '\r') || !c.is_control())
}

fn gunzip(data: &[u8]) -> IoResult<Vec<u8>> {
    let mut decoder = GzDecoder::new(data);
    let mut out = Vec::new();
    decoder.read_to_end(&mut out)?;
    Ok(out)
}

fn extract_json_error(payload: &[u8]) -> Option<String> {
    let parsed: Value = serde_json::from_slice(payload).ok()?;
    let error = parsed.get("error")?;
    let code = error.get("code").and_then(|v| v.as_str());
    let message = error.get("message").and_then(|v| v.as_str());
    let debug = error
        .pointer("/details/0/debug/error")
        .and_then(|v| v.as_str());
    let detail = error
        .pointer("/details/0/debug/details/detail")
        .and_then(|v| v.as_str());
    let parts: Vec<&str> = [code, debug, message, detail]
        .into_iter()
        .flatten()
        .collect();
    (!parts.is_empty()).then(|| parts.join(" - "))
}

struct SseWriter {
    model: String,
    format: CursorResponseFormat,
    id: String,
    created: i64,
    anthropic_next_index: usize,
    anthropic_thinking_open: Option<usize>,
    anthropic_text_open: Option<usize>,
    openai_role_sent: bool,
}

impl SseWriter {
    fn new(model: String, format: CursorResponseFormat) -> Self {
        let prefix = match format {
            CursorResponseFormat::AnthropicMessages => "msg",
            CursorResponseFormat::OpenAiResponses => "resp",
            CursorResponseFormat::OpenAiChatCompletions => "chatcmpl",
        };
        Self {
            model,
            format,
            id: format!(
                "{prefix}_{}",
                uuid::Uuid::new_v4().to_string().replace('-', "")
            ),
            created: chrono::Utc::now().timestamp(),
            anthropic_next_index: 0,
            anthropic_thinking_open: None,
            anthropic_text_open: None,
            openai_role_sent: false,
        }
    }

    fn start_events(&mut self) -> Vec<String> {
        match self.format {
            CursorResponseFormat::AnthropicMessages => vec![anthropic_event(
                "message_start",
                json!({
                    "type": "message_start",
                    "message": {
                        "id": self.id,
                        "type": "message",
                        "role": "assistant",
                        "content": [],
                        "model": self.model,
                        "stop_reason": null,
                        "stop_sequence": null,
                        "usage": {"input_tokens": 0, "output_tokens": 0}
                    }
                }),
            )],
            CursorResponseFormat::OpenAiResponses => vec![event(
                "response.created",
                json!({
                    "type": "response.created",
                    "response": {"id": self.id, "created_at": self.created, "model": self.model}
                }),
            )],
            CursorResponseFormat::OpenAiChatCompletions => Vec::new(),
        }
    }

    fn delta_events(&mut self, delta: &CursorDelta) -> Vec<String> {
        match self.format {
            CursorResponseFormat::AnthropicMessages => self.anthropic_delta_events(delta),
            CursorResponseFormat::OpenAiResponses => {
                let mut out = Vec::new();
                if !delta.reasoning_delta.is_empty() {
                    out.push(event(
                        "response.reasoning_summary_text.delta",
                        json!({"type": "response.reasoning_summary_text.delta", "delta": delta.reasoning_delta}),
                    ));
                }
                if !delta.text_delta.is_empty() {
                    out.push(event(
                        "response.output_text.delta",
                        json!({"type": "response.output_text.delta", "delta": delta.text_delta}),
                    ));
                }
                out
            }
            CursorResponseFormat::OpenAiChatCompletions => {
                let mut out = Vec::new();
                if !self.openai_role_sent {
                    out.push(chat_chunk(
                        &self.id,
                        self.created,
                        &self.model,
                        json!({"role": "assistant", "content": ""}),
                        None,
                    ));
                    self.openai_role_sent = true;
                }
                if !delta.reasoning_delta.is_empty() {
                    out.push(chat_chunk(
                        &self.id,
                        self.created,
                        &self.model,
                        json!({"reasoning_content": delta.reasoning_delta}),
                        None,
                    ));
                }
                if !delta.text_delta.is_empty() {
                    out.push(chat_chunk(
                        &self.id,
                        self.created,
                        &self.model,
                        json!({"content": delta.text_delta}),
                        None,
                    ));
                }
                out
            }
        }
    }

    fn anthropic_delta_events(&mut self, delta: &CursorDelta) -> Vec<String> {
        let mut out = Vec::new();
        if !delta.reasoning_delta.is_empty() && self.anthropic_text_open.is_none() {
            let idx = *self.anthropic_thinking_open.get_or_insert_with(|| {
                let idx = self.anthropic_next_index;
                self.anthropic_next_index += 1;
                out.push(anthropic_event(
                    "content_block_start",
                    json!({"type": "content_block_start", "index": idx, "content_block": {"type": "thinking", "thinking": ""}}),
                ));
                idx
            });
            out.push(anthropic_event(
                "content_block_delta",
                json!({"type": "content_block_delta", "index": idx, "delta": {"type": "thinking_delta", "thinking": delta.reasoning_delta}}),
            ));
        }
        if !delta.text_delta.is_empty() {
            if let Some(idx) = self.anthropic_thinking_open.take() {
                out.push(anthropic_event(
                    "content_block_stop",
                    json!({"type": "content_block_stop", "index": idx}),
                ));
            }
            let idx = *self.anthropic_text_open.get_or_insert_with(|| {
                let idx = self.anthropic_next_index;
                self.anthropic_next_index += 1;
                out.push(anthropic_event(
                    "content_block_start",
                    json!({"type": "content_block_start", "index": idx, "content_block": {"type": "text", "text": ""}}),
                ));
                idx
            });
            out.push(anthropic_event(
                "content_block_delta",
                json!({"type": "content_block_delta", "index": idx, "delta": {"type": "text_delta", "text": delta.text_delta}}),
            ));
        }
        out
    }

    fn done_events(
        &mut self,
        aggregated_text: &str,
        aggregated_reasoning: &str,
        error: Option<&str>,
    ) -> Vec<String> {
        match self.format {
            CursorResponseFormat::AnthropicMessages => {
                let mut out = Vec::new();
                if let Some(idx) = self.anthropic_thinking_open.take() {
                    out.push(anthropic_event(
                        "content_block_stop",
                        json!({"type": "content_block_stop", "index": idx}),
                    ));
                }
                if let Some(idx) = self.anthropic_text_open.take() {
                    out.push(anthropic_event(
                        "content_block_stop",
                        json!({"type": "content_block_stop", "index": idx}),
                    ));
                }
                if let Some(error) =
                    error.filter(|_| aggregated_text.is_empty() && aggregated_reasoning.is_empty())
                {
                    out.push(anthropic_event("error", json!({"type": "error", "error": {"type": "upstream_error", "message": error}})));
                    return out;
                }
                out.push(anthropic_event(
                    "message_delta",
                    json!({"type": "message_delta", "delta": {"stop_reason": "end_turn", "stop_sequence": null}, "usage": {"output_tokens": 0}}),
                ));
                out.push(anthropic_event(
                    "message_stop",
                    json!({"type": "message_stop"}),
                ));
                out
            }
            CursorResponseFormat::OpenAiResponses => {
                if let Some(error) =
                    error.filter(|_| aggregated_text.is_empty() && aggregated_reasoning.is_empty())
                {
                    return vec![
                        event(
                            "response.failed",
                            json!({"type": "response.failed", "response": {"id": self.id, "error": {"message": error}}}),
                        ),
                        "data: [DONE]\n\n".to_string(),
                    ];
                }
                vec![
                    event(
                        "response.completed",
                        json!({"type": "response.completed", "response": build_openai_response_json_with_id(&self.id, self.created, &self.model, aggregated_text, aggregated_reasoning)}),
                    ),
                    "data: [DONE]\n\n".to_string(),
                ]
            }
            CursorResponseFormat::OpenAiChatCompletions => {
                let mut out = Vec::new();
                if !self.openai_role_sent {
                    out.push(chat_chunk(
                        &self.id,
                        self.created,
                        &self.model,
                        json!({"role": "assistant", "content": ""}),
                        None,
                    ));
                    self.openai_role_sent = true;
                }
                if let Some(error) =
                    error.filter(|_| aggregated_text.is_empty() && aggregated_reasoning.is_empty())
                {
                    out.push(format!(
                        "data: {}\n\n",
                        json!({"error": {"message": error, "type": "upstream_error"}})
                    ));
                    out.push("data: [DONE]\n\n".to_string());
                    return out;
                }
                out.push(chat_chunk(
                    &self.id,
                    self.created,
                    &self.model,
                    json!({}),
                    Some("stop"),
                ));
                out.push("data: [DONE]\n\n".to_string());
                out
            }
        }
    }

    fn error_events(&mut self, message: &str) -> Vec<String> {
        match self.format {
            CursorResponseFormat::AnthropicMessages => {
                vec![anthropic_event(
                    "error",
                    json!({"type": "error", "error": {"type": "upstream_error", "message": message}}),
                )]
            }
            CursorResponseFormat::OpenAiResponses => vec![
                event(
                    "response.failed",
                    json!({"type": "response.failed", "response": {"id": self.id, "error": {"message": message}}}),
                ),
                "data: [DONE]\n\n".to_string(),
            ],
            CursorResponseFormat::OpenAiChatCompletions => vec![
                format!(
                    "data: {}\n\n",
                    json!({"error": {"message": message, "type": "upstream_error"}})
                ),
                "data: [DONE]\n\n".to_string(),
            ],
        }
    }
}

fn anthropic_event(event_name: &str, data: Value) -> String {
    event(event_name, data)
}

fn event(event_name: &str, data: Value) -> String {
    format!("event: {event_name}\ndata: {data}\n\n")
}

fn chat_chunk(
    id: &str,
    created: i64,
    model: &str,
    delta: Value,
    finish_reason: Option<&str>,
) -> String {
    format!(
        "data: {}\n\n",
        json!({
            "id": id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "choices": [{"index": 0, "delta": delta, "finish_reason": finish_reason}]
        })
    )
}

fn build_openai_response_json(model: &str, text: &str, reasoning: &str) -> Value {
    build_openai_response_json_with_id(
        &format!("resp_{}", uuid::Uuid::new_v4().to_string().replace('-', "")),
        chrono::Utc::now().timestamp(),
        model,
        text,
        reasoning,
    )
}

fn build_openai_response_json_with_id(
    id: &str,
    created: i64,
    model: &str,
    text: &str,
    reasoning: &str,
) -> Value {
    let mut output = Vec::new();
    if !reasoning.trim().is_empty() {
        output.push(json!({
            "type": "reasoning",
            "summary": [{"type": "summary_text", "text": reasoning}]
        }));
    }
    output.push(json!({
        "type": "message",
        "role": "assistant",
        "content": [{"type": "output_text", "text": text}]
    }));
    json!({
        "id": id,
        "object": "response",
        "created_at": created,
        "status": "completed",
        "model": model,
        "output": output,
        "usage": {
            "input_tokens": 0,
            "output_tokens": 0,
            "total_tokens": 0,
            "input_tokens_details": {"cached_tokens": 0},
            "output_tokens_details": {"reasoning_tokens": 0}
        }
    })
}

fn build_chat_completion_json(model: &str, text: &str, reasoning: &str) -> Value {
    let mut message = json!({"role": "assistant", "content": text});
    if !reasoning.trim().is_empty() {
        message["reasoning_content"] = json!(reasoning);
    }
    json!({
        "id": format!("chatcmpl-{}", uuid::Uuid::new_v4().to_string().replace('-', "")),
        "object": "chat.completion",
        "created": chrono::Utc::now().timestamp(),
        "model": model,
        "choices": [{"index": 0, "message": message, "finish_reason": "stop", "logprobs": null}],
        "usage": {"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0}
    })
}

fn build_anthropic_message_json(model: &str, text: &str, reasoning: &str) -> Value {
    let mut content = Vec::new();
    if !reasoning.trim().is_empty() {
        content.push(json!({"type": "thinking", "thinking": reasoning}));
    }
    if !text.is_empty() {
        content.push(json!({"type": "text", "text": text}));
    }
    json!({
        "id": format!("msg_{}", uuid::Uuid::new_v4().to_string().replace('-', "")),
        "type": "message",
        "role": "assistant",
        "content": content,
        "model": model,
        "stop_reason": "end_turn",
        "stop_sequence": null,
        "usage": {"input_tokens": 0, "output_tokens": 0}
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn create_provider(config: serde_json::Value) -> Provider {
        Provider {
            id: "cursor-test".to_string(),
            name: "Cursor Test".to_string(),
            settings_config: config,
            website_url: None,
            category: Some("codex".to_string()),
            created_at: None,
            sort_index: None,
            notes: None,
            meta: None,
            icon: None,
            icon_color: None,
            in_failover_queue: false,
        }
    }

    #[test]
    fn normalise_model_maps_public_names() {
        assert_eq!(normalise_model("claude-sonnet-4-5"), "claude-4.5-sonnet");
        assert_eq!(normalise_model("cursor/gpt-5.3-codex"), "gpt-5.3-codex");
        assert_eq!(
            normalise_model("custom-cursor-model"),
            "custom-cursor-model"
        );
    }

    #[test]
    fn connect_frame_wraps_payload() {
        let frame = connect_frame(b"abc");
        assert_eq!(frame, vec![0, 0, 0, 0, 3, b'a', b'b', b'c']);
    }

    #[test]
    fn prepare_cursor_codex_body_applies_single_model_mapping() {
        let provider = create_provider(json!({
            "modelMapping": {
                "mode": "single",
                "upstreamModel": "composer-2.5"
            }
        }));
        let body = json!({
            "model": "gpt-5.5",
            "input": "who are you"
        });

        let (mapped_body, response_model) = prepare_cursor_codex_body(&provider, &body);

        assert_eq!(response_model, "gpt-5.5");
        assert_eq!(
            mapped_body.get("model").and_then(|value| value.as_str()),
            Some("composer-2.5")
        );
    }
}
