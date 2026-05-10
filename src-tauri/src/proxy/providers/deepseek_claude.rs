use super::{
    deepseek_account_auth::DeepSeekAccountManager,
    deepseek_client::DeepSeekWebClient,
    deepseek_sse::{parse_sse_data_line, DeepSeekEvent},
};
use crate::{provider::Provider, proxy::hyper_client::ProxyResponse, proxy::ProxyError};
use async_stream::stream;
use bytes::Bytes;
use futures::StreamExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tauri::Manager;

pub async fn forward_deepseek_claude(
    app_handle: Option<&tauri::AppHandle>,
    provider: &Provider,
    body: &Value,
) -> Result<ProxyResponse, ProxyError> {
    let app_handle = app_handle.ok_or_else(|| {
        ProxyError::AuthError("DeepSeek Account is unavailable without app state".to_string())
    })?;
    let state = app_handle.state::<crate::commands::DeepSeekAccountState>();
    let manager = state.0.clone();
    forward_deepseek_claude_with_manager(manager, provider, body).await
}

pub async fn forward_deepseek_claude_with_manager(
    manager: Arc<tokio::sync::RwLock<DeepSeekAccountManager>>,
    provider: &Provider,
    body: &Value,
) -> Result<ProxyResponse, ProxyError> {
    let account_id = if let Some(account_id) = provider
        .meta
        .as_ref()
        .and_then(|meta| meta.managed_account_id_for("deepseek_account"))
    {
        account_id
    } else {
        manager
            .read()
            .await
            .default_account_id()
            .await
            .ok_or_else(|| {
                ProxyError::AuthError(
                    "DeepSeek Account provider is not bound to an account".to_string(),
                )
            })?
    };

    let prompt = build_prompt(body)?;
    let input_tokens = estimate_billable_user_input_tokens(body);
    let response_model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("claude-sonnet-4-5")
        .to_string();
    let deepseek_model = map_model(&response_model);
    let stream_enabled = body.get("stream").and_then(Value::as_bool).unwrap_or(false);

    if stream_enabled {
        let stream = start_stream(
            manager,
            account_id,
            deepseek_model.clone(),
            deepseek_model,
            prompt,
            input_tokens,
        );
        return Ok(ProxyResponse::local_sse(Box::pin(stream)));
    }

    let text = collect_text(manager, &account_id, &deepseek_model, &prompt).await?;
    let output_tokens = estimate_tokens(&text);
    let body = serde_json::to_vec(&json!({
        "id": format!("msg_{}", uuid::Uuid::new_v4().simple()),
        "type": "message",
        "role": "assistant",
        "content": [{"type": "text", "text": text}],
        "model": deepseek_model,
        "stop_reason": "end_turn",
        "stop_sequence": null,
        "usage": {"input_tokens": input_tokens, "output_tokens": output_tokens}
    }))
    .map_err(|e| ProxyError::Internal(e.to_string()))?;
    Ok(ProxyResponse::local_json(
        http::StatusCode::OK,
        Bytes::from(body),
    ))
}

fn start_stream(
    manager: Arc<tokio::sync::RwLock<DeepSeekAccountManager>>,
    account_id: String,
    deepseek_model: String,
    actual_model: String,
    prompt: String,
    input_tokens: u32,
) -> impl futures::Stream<Item = Result<Bytes, std::io::Error>> + Send {
    stream! {
        yield Ok(sse_event("message_start", &json!({
            "type": "message_start",
            "message": {
                "id": format!("msg_{}", uuid::Uuid::new_v4().simple()),
                "type": "message",
                "role": "assistant",
                "content": [],
                "model": actual_model,
                "stop_reason": null,
                "stop_sequence": null,
                "usage": {"input_tokens": input_tokens, "output_tokens": 0}
            }
        })));
        yield Ok(sse_event("content_block_start", &json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "text", "text": ""}
        })));

        match start_deepseek_response(manager, &account_id, &deepseek_model, &prompt).await {
            Ok(resp) => {
                let status = resp.status();
                if !status.is_success() {
                    let body = resp.text().await.unwrap_or_default();
                    yield Ok(sse_event("error", &json!({
                        "type": "error",
                        "error": {
                            "type": "api_error",
                            "message": format!("DeepSeek returned HTTP {status}: {body}")
                        }
                    })));
                    return;
                }
                let mut bytes_stream = resp.bytes_stream();
                let mut buffer = String::new();
                let mut output_text = String::new();
                let mut done = false;
                while let Some(item) = bytes_stream.next().await {
                    if done {
                        break;
                    }
                    match item {
                        Ok(bytes) => {
                            buffer.push_str(&String::from_utf8_lossy(&bytes));
                            while let Some(pos) = buffer.find('\n') {
                                let line = buffer[..pos].trim_end_matches('\r').to_string();
                                buffer = buffer[pos + 1..].to_string();
                                if !line.trim_start().starts_with("data:") {
                                    continue;
                                }
                                match parse_sse_data_line(&line) {
                                    DeepSeekEvent::Text(text) if !text.is_empty() => {
                                        output_text.push_str(&text);
                                        yield Ok(sse_event("content_block_delta", &json!({
                                            "type": "content_block_delta",
                                            "index": 0,
                                            "delta": {"type": "text_delta", "text": text}
                                        })));
                                    }
                                    DeepSeekEvent::Done => {
                                        done = true;
                                        break;
                                    }
                                    _ => {}
                                }
                            }
                        }
                        Err(err) => {
                            yield Ok(sse_event("error", &json!({
                                "type": "error",
                                "error": {"type": "api_error", "message": err.to_string()}
                            })));
                            break;
                        }
                    }
                }
                let output_tokens = estimate_tokens(&output_text);
                yield Ok(sse_event("content_block_stop", &json!({"type": "content_block_stop", "index": 0})));
                yield Ok(sse_event("message_delta", &json!({
                    "type": "message_delta",
                    "delta": {"stop_reason": "end_turn", "stop_sequence": null},
                    "usage": {"output_tokens": output_tokens}
                })));
                yield Ok(sse_event("message_stop", &json!({"type": "message_stop"})));
                return;
            }
            Err(err) => {
                yield Ok(sse_event("error", &json!({
                    "type": "error",
                    "error": {"type": "api_error", "message": err.to_string()}
                })));
            }
        }

        yield Ok(sse_event("content_block_stop", &json!({"type": "content_block_stop", "index": 0})));
        yield Ok(sse_event("message_delta", &json!({
            "type": "message_delta",
            "delta": {"stop_reason": "end_turn", "stop_sequence": null},
            "usage": {"output_tokens": 0}
        })));
        yield Ok(sse_event("message_stop", &json!({"type": "message_stop"})));
    }
}

async fn collect_text(
    manager: Arc<tokio::sync::RwLock<DeepSeekAccountManager>>,
    account_id: &str,
    model: &str,
    prompt: &str,
) -> Result<String, ProxyError> {
    let resp = start_deepseek_response(manager, account_id, model, prompt).await?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(ProxyError::UpstreamError {
            status: status.as_u16(),
            body: Some(body),
        });
    }
    let mut out = String::new();
    for line in body
        .lines()
        .filter(|line| line.trim_start().starts_with("data:"))
    {
        match parse_sse_data_line(line) {
            DeepSeekEvent::Text(text) => out.push_str(&text),
            DeepSeekEvent::Done => break,
            DeepSeekEvent::Ignored => {}
        }
    }
    Ok(out)
}

async fn start_deepseek_response(
    manager: Arc<tokio::sync::RwLock<DeepSeekAccountManager>>,
    account_id: &str,
    model: &str,
    prompt: &str,
) -> Result<reqwest::Response, ProxyError> {
    let client = DeepSeekWebClient::new();
    let token = manager
        .read()
        .await
        .get_valid_token_for_account(account_id)
        .await
        .map_err(|e| ProxyError::AuthError(e.to_string()))?;
    let resp = start_with_token(&client, &token, model, prompt).await?;
    if matches!(resp.status().as_u16(), 401 | 403) {
        manager
            .read()
            .await
            .invalidate_cached_token(account_id)
            .await;
        let token = manager
            .read()
            .await
            .get_valid_token_for_account(account_id)
            .await
            .map_err(|e| ProxyError::AuthError(e.to_string()))?;
        return start_with_token(&client, &token, model, prompt).await;
    }
    Ok(resp)
}

async fn start_with_token(
    client: &DeepSeekWebClient,
    token: &str,
    model: &str,
    prompt: &str,
) -> Result<reqwest::Response, ProxyError> {
    let session_id = client
        .create_session(token)
        .await
        .map_err(|e| ProxyError::ForwardFailed(e.to_string()))?;
    let pow = client
        .create_pow_header(token)
        .await
        .map_err(|e| ProxyError::ForwardFailed(e.to_string()))?;
    client
        .completion(token, &session_id, &pow, model, prompt)
        .await
        .map_err(|e| ProxyError::ForwardFailed(e.to_string()))
}

fn sse_event(event: &str, data: &Value) -> Bytes {
    Bytes::from(format!("event: {event}\ndata: {data}\n\n"))
}

fn map_model(model: &str) -> String {
    match model {
        "claude-sonnet-4-5" | "claude-sonnet-4-6" | "claude-sonnet-4-7" | "claude-3-5-sonnet" => {
            "deepseek-v4-flash".to_string()
        }
        "claude-opus-4-5" | "claude-opus-4-6" | "claude-opus-4-7" | "claude-3-opus" => {
            "deepseek-v4-pro".to_string()
        }
        m if m.starts_with("deepseek-") => m.to_string(),
        _ => "deepseek-v4-flash".to_string(),
    }
}

fn build_prompt(body: &Value) -> Result<String, ProxyError> {
    let mut parts = Vec::new();
    if let Some(system) = body.get("system") {
        let text = text_from_content(system);
        if !text.trim().is_empty() {
            parts.push(format!("<system>\n{}\n</system>", text.trim()));
        }
    }
    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| ProxyError::InvalidRequest("messages must be an array".to_string()))?;
    for message in messages {
        let role = match message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user")
        {
            "assistant" => "Assistant",
            "user" => "User",
            other => other,
        };
        let text = text_from_content(message.get("content").unwrap_or(&Value::Null));
        if !text.trim().is_empty() {
            parts.push(format!("{role}: {}", text.trim()));
        }
    }
    let prompt = parts.join("\n\n");
    if prompt.trim().is_empty() {
        return Err(ProxyError::InvalidRequest(
            "text prompt is empty".to_string(),
        ));
    }
    Ok(prompt)
}

fn text_from_content(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|item| {
                (item.get("type").and_then(Value::as_str) == Some("text"))
                    .then(|| item.get("text").and_then(Value::as_str).map(str::to_string))
                    .flatten()
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn estimate_billable_user_input_tokens(body: &Value) -> u32 {
    let Some(messages) = body.get("messages").and_then(Value::as_array) else {
        return 0;
    };

    messages
        .iter()
        .rev()
        .filter(|message| message.get("role").and_then(Value::as_str) == Some("user"))
        .map(|message| text_from_content(message.get("content").unwrap_or(&Value::Null)))
        .find(|text| !text.trim().is_empty())
        .map(|text| estimate_tokens(&text))
        .unwrap_or(0)
}

fn estimate_tokens(text: &str) -> u32 {
    let chars = text.chars().filter(|c| !c.is_whitespace()).count() as u32;
    if chars == 0 {
        0
    } else {
        chars.div_ceil(4).max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::usage::parser::TokenUsage;
    use serde_json::json;

    #[test]
    fn deepseek_claude_non_stream_usage_is_estimated() {
        let body = json!({
            "system": "Be concise.",
            "messages": [{"role": "user", "content": "hello world"}]
        });
        let prompt = build_prompt(&body).unwrap();

        assert!(estimate_tokens(&prompt) > 0);
        assert_eq!(estimate_billable_user_input_tokens(&body), 3);
        assert_eq!(estimate_tokens("hello world"), 3);
    }

    #[test]
    fn deepseek_claude_input_usage_counts_only_latest_user_text() {
        let body = json!({
            "system": "Large cached system prompt that should not be billed as new input.",
            "messages": [
                {"role": "user", "content": "old cached user message that is long"},
                {"role": "assistant", "content": "old assistant answer"},
                {
                    "role": "user",
                    "content": [
                        {"type": "tool_result", "tool_use_id": "toolu_1", "content": "tool output should not count"},
                        {"type": "text", "text": "new question", "cache_control": {"type": "ephemeral"}}
                    ]
                }
            ]
        });

        assert_eq!(
            estimate_billable_user_input_tokens(&body),
            estimate_tokens("new question")
        );
    }

    #[test]
    fn generated_claude_stream_usage_is_parseable() {
        let events = vec![
            json!({
                "type": "message_start",
                "message": {
                    "id": "msg_test",
                    "model": "claude-opus-4-7",
                    "usage": {"input_tokens": estimate_tokens("User: hello world"), "output_tokens": 0}
                }
            }),
            json!({
                "type": "message_delta",
                "delta": {"stop_reason": "end_turn", "stop_sequence": null},
                "usage": {"output_tokens": estimate_tokens("hello from deepseek")}
            }),
        ];

        let usage = TokenUsage::from_claude_stream_events(&events).unwrap();
        assert!(usage.input_tokens > 0);
        assert!(usage.output_tokens > 0);
        assert_eq!(usage.model, Some("claude-opus-4-7".to_string()));
        assert_eq!(usage.message_id, Some("msg_test".to_string()));
    }

    #[test]
    fn claude_4_7_models_map_to_expected_deepseek_tiers() {
        assert_eq!(map_model("claude-opus-4-7"), "deepseek-v4-pro");
        assert_eq!(map_model("claude-sonnet-4-7"), "deepseek-v4-flash");
    }
}
