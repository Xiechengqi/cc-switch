//! Kiro OAuth provider for Claude-compatible proxy requests.

use crate::commands::KiroOAuthState;
use crate::provider::Provider;
use crate::proxy::hyper_client::ProxyResponse;
use crate::proxy::ProxyError;
use bytes::{Buf, Bytes, BytesMut};
use futures::{Stream, StreamExt};
use http::StatusCode;
use once_cell::sync::Lazy;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::Mutex,
};
use tauri::Manager;

use super::kiro_oauth_auth::{machine_id_from_refresh_token, KiroAccountData};

const DEFAULT_KIRO_VERSION: &str = "2.3.0";
const DEFAULT_SYSTEM_VERSION: &str = "macos";
const DEFAULT_NODE_VERSION: &str = "22.22.0";
const BUILDER_ID_PROFILE_ARN: &str =
    "arn:aws:codewhisperer:us-east-1:638616132270:profile/AAAACCCCXXXX";
const TOOL_NAME_MAX_LEN: usize = 63;
const WRITE_TOOL_DESCRIPTION_SUFFIX: &str = "- IMPORTANT: If the content to write exceeds 150 lines, you MUST only write the first 50 lines using this tool, then use `Edit` tool to append the remaining content in chunks of no more than 50 lines each. If needed, leave a unique placeholder to help append content. Do NOT attempt to write all content at once.";
const EDIT_TOOL_DESCRIPTION_SUFFIX: &str = "- IMPORTANT: If the `new_string` content exceeds 50 lines, you MUST split it into multiple Edit calls, each replacing no more than 50 lines at a time. If used to append content, leave a unique placeholder to help append content. On the final chunk, do NOT include the placeholder.";
const SYSTEM_CHUNKED_POLICY: &str = "When the Write or Edit tool has content size limits, always comply silently. Never suggest bypassing these limits via alternative tools. Never ask the user whether to switch approaches. Complete all chunked operations without commentary.";
const ACCOUNT_THROTTLE_COOLDOWN_SECS: i64 = 30 * 60;
const QUOTA_EXHAUSTED_COOLDOWN_SECS: i64 = 24 * 60 * 60;
const PROMPT_CACHE_CAPACITY: usize = 4096;
const PROMPT_CACHE_DEFAULT_TTL_SECS: i64 = 5 * 60;
const PROMPT_CACHE_MAX_TTL_SECS: i64 = 60 * 60;

static KIRO_PROMPT_CACHE: Lazy<KiroPromptCache> = Lazy::new(|| {
    KiroPromptCache::new(Some(
        crate::config::get_app_config_dir().join("kiro_prompt_cache.json"),
    ))
});

struct KiroRequestBuild {
    body: Value,
    tool_name_map: HashMap<String, String>,
}

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
    let manager = { state.0.read().await.clone() };
    let bound_account_id = provider
        .meta
        .as_ref()
        .and_then(|m| m.managed_account_id_for("kiro_oauth"));
    let allow_account_failover = bound_account_id.is_none();
    let mut attempted_account_ids = HashSet::new();

    let (response, request) = loop {
        let resolved_account = match bound_account_id.as_deref() {
            Some(id) => manager.get_account(id).await,
            None => {
                manager
                    .get_available_account_excluding(&attempted_account_ids)
                    .await
            }
        }
        .ok_or_else(|| ProxyError::AuthError("未找到可用 Kiro OAuth 账号".to_string()))?;
        attempted_account_ids.insert(resolved_account.account_id.clone());

        let token = manager
            .get_valid_token_for_account(&resolved_account.account_id)
            .await
            .map_err(|e| ProxyError::AuthError(format!("Kiro OAuth 认证失败: {e}")))?;

        let request = anthropic_to_kiro_request(body, &resolved_account)?;
        let response = send_kiro_request(&resolved_account, &token, request.body.clone()).await?;

        let response = if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            manager
                .invalidate_cached_token(&resolved_account.account_id)
                .await;
            let token = manager
                .get_valid_token_for_account(&resolved_account.account_id)
                .await
                .map_err(|e| ProxyError::AuthError(format!("Kiro OAuth 认证失败: {e}")))?;
            send_kiro_request(&resolved_account, &token, request.body.clone()).await?
        } else {
            response
        };

        if response.status().is_success() {
            break (response, request);
        }

        let status = response.status();
        let status_code = status.as_u16();
        let response_body = response.text().await.ok();
        let response_text = response_body.as_deref().unwrap_or("");
        if is_quota_exhausted(response_text) || is_account_throttled(status, response_text) {
            let cooldown_secs = if is_quota_exhausted(response_text) {
                QUOTA_EXHAUSTED_COOLDOWN_SECS
            } else {
                ACCOUNT_THROTTLE_COOLDOWN_SECS
            };
            manager
                .mark_account_temporarily_unavailable(&resolved_account.account_id, cooldown_secs)
                .await;
            if allow_account_failover
                && manager
                    .get_available_account_excluding(&attempted_account_ids)
                    .await
                    .is_some()
            {
                continue;
            }
        }

        return Err(ProxyError::UpstreamError {
            status: status_code,
            body: response_body,
        });
    };

    let model = body
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("claude-sonnet-4-5");
    let is_stream = body
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let prompt_cache_usage = compute_kiro_prompt_cache_usage(body);

    if is_stream {
        let stream = kiro_event_stream_to_anthropic_sse(
            response.bytes_stream(),
            model.to_string(),
            request.tool_name_map,
            prompt_cache_usage,
        );
        Ok(ProxyResponse::local_sse(Box::pin(stream)))
    } else {
        let bytes = response
            .bytes()
            .await
            .map_err(|e| ProxyError::ForwardFailed(format!("读取 Kiro 响应失败: {e}")))?;
        let message = kiro_event_bytes_to_anthropic_json(
            &bytes,
            model,
            &request.tool_name_map,
            prompt_cache_usage,
        );
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

fn anthropic_to_kiro_request(
    body: &Value,
    account: &KiroAccountData,
) -> Result<KiroRequestBuild, ProxyError> {
    let model = body
        .get("model")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ProxyError::InvalidRequest("missing model".to_string()))?;
    let model_id = map_model(model)
        .ok_or_else(|| ProxyError::InvalidRequest(format!("Kiro OAuth 不支持该模型: {model}")))?;
    let raw_messages = body
        .get("messages")
        .and_then(|v| v.as_array())
        .ok_or_else(|| ProxyError::InvalidRequest("missing messages".to_string()))?;
    if raw_messages.is_empty() {
        return Err(ProxyError::InvalidRequest("messages is empty".to_string()));
    }

    let last_user_idx = raw_messages
        .iter()
        .rposition(|m| m.get("role").and_then(|v| v.as_str()) == Some("user"))
        .ok_or_else(|| ProxyError::InvalidRequest("missing user message".to_string()))?;
    let messages = &raw_messages[..=last_user_idx];

    let mut tool_name_map = HashMap::new();
    let mut tools = convert_tools(body.get("tools"), &mut tool_name_map);
    let (content, images, tool_results) =
        parse_user_content(messages[last_user_idx].get("content"));
    let mut history = build_history(body, messages, model_id, &mut tool_name_map);
    let (validated_tool_results, orphaned_tool_use_ids) =
        validate_tool_pairing(&history, &tool_results);
    remove_orphaned_tool_uses(&mut history, &orphaned_tool_use_ids);
    add_missing_history_tools(&mut tools, &history);

    let current_message = json!({
        "userInputMessage": {
            "userInputMessageContext": {
                "envState": env_state(),
                "toolResults": validated_tool_results,
                "tools": tools
            },
            "content": content,
            "modelId": model_id,
            "images": images,
            "origin": "AI_EDITOR"
        }
    });

    let profile_arn = account
        .profile_arn
        .clone()
        .unwrap_or_else(|| BUILDER_ID_PROFILE_ARN.to_string());
    Ok(KiroRequestBuild {
        body: json!({
        "conversationState": {
            "agentTaskType": "vibe",
            "chatTriggerType": "MANUAL",
            "currentMessage": current_message,
            "conversationId": conversation_id(body),
            "agentContinuationId": uuid::Uuid::new_v4().to_string(),
            "history": history
        },
        "profileArn": profile_arn
        }),
        tool_name_map,
    })
}

fn map_model(model: &str) -> Option<&'static str> {
    let m = model.to_ascii_lowercase();
    if m.contains("sonnet") {
        if m.contains("4-8") || m.contains("4.8") {
            Some("claude-sonnet-4.8")
        } else if m.contains("4-6") || m.contains("4.6") {
            Some("claude-sonnet-4.6")
        } else if m.contains("4-5") || m.contains("4.5") {
            Some("claude-sonnet-4.5")
        } else {
            Some("claude-sonnet-4.5")
        }
    } else if m.contains("opus") {
        if m.contains("4-8") || m.contains("4.8") {
            Some("claude-opus-4.8")
        } else if m.contains("4-7") || m.contains("4.7") {
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

fn convert_tools(tools: Option<&Value>, tool_name_map: &mut HashMap<String, String>) -> Vec<Value> {
    tools
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|tool| {
                    let name = tool.get("name")?.as_str()?;
                    let mapped_name = map_tool_name(name, tool_name_map);
                    let mut description = tool
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    match name {
                        "Write" => {
                            description.push('\n');
                            description.push_str(WRITE_TOOL_DESCRIPTION_SUFFIX);
                        }
                        "Edit" => {
                            description.push('\n');
                            description.push_str(EDIT_TOOL_DESCRIPTION_SUFFIX);
                        }
                        _ => {}
                    }
                    if description.trim().is_empty() {
                        description = name.to_string();
                    }
                    description = truncate_chars(description, 10_000);
                    let schema = tool
                        .get("input_schema")
                        .cloned()
                        .unwrap_or_else(|| json!({"type":"object","properties":{}}));
                    Some(json!({
                        "toolSpecification": {
                            "name": mapped_name,
                            "description": description,
                            "inputSchema": { "json": normalize_schema(schema) }
                        }
                    }))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn normalize_schema(schema: Value) -> Value {
    let Value::Object(mut obj) = schema else {
        return json!({
            "type": "object",
            "properties": {},
            "required": [],
            "additionalProperties": true
        });
    };

    obj.remove("$schema");
    if !obj
        .get("type")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty())
    {
        obj.insert("type".to_string(), json!("object"));
    }

    let properties = match obj.remove("properties") {
        Some(Value::Object(props)) => Value::Object(
            props
                .into_iter()
                .map(|(k, v)| (k, normalize_property_schema(v)))
                .collect(),
        ),
        _ => json!({}),
    };
    obj.insert("properties".to_string(), properties);

    let required = match obj.remove("required") {
        Some(Value::Array(arr)) => Value::Array(
            arr.into_iter()
                .filter_map(|v| v.as_str().map(|s| Value::String(s.to_string())))
                .collect(),
        ),
        _ => json!([]),
    };
    obj.insert("required".to_string(), required);

    if !matches!(
        obj.get("additionalProperties"),
        Some(Value::Bool(_)) | Some(Value::Object(_))
    ) {
        obj.insert("additionalProperties".to_string(), json!(true));
    }

    Value::Object(obj)
}

fn normalize_property_schema(schema: Value) -> Value {
    let Value::Object(mut obj) = schema else {
        return schema;
    };

    obj.remove("$schema");
    if obj
        .get("exclusiveMinimum")
        .and_then(|v| v.as_f64())
        .is_some()
    {
        obj.remove("exclusiveMinimum");
    }
    if obj
        .get("exclusiveMaximum")
        .and_then(|v| v.as_f64())
        .is_some()
    {
        obj.remove("exclusiveMaximum");
    }
    for key in ["maximum", "minimum"] {
        if let Some(v) = obj.get(key).and_then(|v| v.as_f64()) {
            if !(-2_147_483_648.0..=2_147_483_647.0).contains(&v) {
                obj.remove(key);
            }
        }
    }
    if let Some(Value::Object(props)) = obj.remove("properties") {
        obj.insert(
            "properties".to_string(),
            Value::Object(
                props
                    .into_iter()
                    .map(|(k, v)| (k, normalize_property_schema(v)))
                    .collect(),
            ),
        );
    }
    if let Some(items) = obj.remove("items") {
        obj.insert("items".to_string(), normalize_property_schema(items));
    }
    Value::Object(obj)
}

fn truncate_chars(value: String, max_chars: usize) -> String {
    match value.char_indices().nth(max_chars) {
        Some((idx, _)) => value[..idx].to_string(),
        None => value,
    }
}

fn shorten_tool_name(name: &str) -> String {
    let digest = Sha256::digest(name.as_bytes());
    let hash_hex = format!("{digest:x}");
    let hash_suffix = &hash_hex[..8];
    let prefix_max = TOOL_NAME_MAX_LEN - 1 - 8;
    let prefix = match name.char_indices().nth(prefix_max) {
        Some((idx, _)) => &name[..idx],
        None => name,
    };
    format!("{prefix}_{hash_suffix}")
}

fn map_tool_name(name: &str, tool_name_map: &mut HashMap<String, String>) -> String {
    if name.chars().count() <= TOOL_NAME_MAX_LEN {
        return name.to_string();
    }
    let short = shorten_tool_name(name);
    tool_name_map.insert(short.clone(), name.to_string());
    short
}

fn original_tool_name(name: &str, tool_name_map: &HashMap<String, String>) -> String {
    tool_name_map
        .get(name)
        .cloned()
        .unwrap_or_else(|| name.to_string())
}

fn build_history(
    body: &Value,
    messages: &[Value],
    model_id: &str,
    tool_name_map: &mut HashMap<String, String>,
) -> Vec<Value> {
    let mut history = Vec::new();
    let prefix = thinking_prefix(body);

    if let Some(system) = system_text(body).filter(|s| !s.is_empty()) {
        let system = format!("{system}\n{SYSTEM_CHUNKED_POLICY}");
        let final_system = match prefix.as_deref() {
            Some(prefix) if !has_thinking_tags(&system) => format!("{prefix}\n{system}"),
            _ => system,
        };
        history.push(history_user_message(
            final_system,
            model_id,
            Vec::new(),
            Vec::new(),
        ));
        history.push(history_assistant_message(
            "I will follow these instructions.".to_string(),
            Vec::new(),
        ));
    } else if let Some(prefix) = prefix {
        history.push(history_user_message(
            prefix,
            model_id,
            Vec::new(),
            Vec::new(),
        ));
        history.push(history_assistant_message(
            "I will follow these instructions.".to_string(),
            Vec::new(),
        ));
    }

    let history_end = messages.len().saturating_sub(1);
    let mut user_buffer: Vec<&Value> = Vec::new();
    let mut assistant_buffer: Vec<&Value> = Vec::new();

    for msg in &messages[..history_end] {
        match msg.get("role").and_then(|v| v.as_str()) {
            Some("user") => {
                if !assistant_buffer.is_empty() {
                    history.push(merge_assistant_messages(&assistant_buffer, tool_name_map));
                    assistant_buffer.clear();
                }
                user_buffer.push(msg);
            }
            Some("assistant") => {
                if !user_buffer.is_empty() {
                    history.push(merge_user_messages(&user_buffer, model_id));
                    user_buffer.clear();
                }
                assistant_buffer.push(msg);
            }
            _ => {}
        }
    }

    if !assistant_buffer.is_empty() {
        history.push(merge_assistant_messages(&assistant_buffer, tool_name_map));
    }
    if !user_buffer.is_empty() {
        history.push(merge_user_messages(&user_buffer, model_id));
        history.push(history_assistant_message("OK".to_string(), Vec::new()));
    }

    history
}

fn system_text(body: &Value) -> Option<String> {
    match body.get("system") {
        Some(Value::String(text)) => Some(text.clone()),
        Some(Value::Array(items)) => {
            let parts = items
                .iter()
                .filter_map(|item| match item {
                    Value::String(text) => Some(text.clone()),
                    Value::Object(_)
                        if item.get("type").and_then(|v| v.as_str()) == Some("text") =>
                    {
                        item.get("text")
                            .and_then(|v| v.as_str())
                            .map(str::to_string)
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            Some(parts.join("\n"))
        }
        _ => None,
    }
}

fn thinking_prefix(body: &Value) -> Option<String> {
    let thinking = body.get("thinking")?;
    let thinking_type = thinking
        .get("type")
        .or_else(|| thinking.get("thinking_type"))
        .and_then(|v| v.as_str())?;
    match thinking_type {
        "enabled" => {
            let budget = thinking
                .get("budget_tokens")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            Some(format!(
                "<thinking_mode>enabled</thinking_mode><max_thinking_length>{budget}</max_thinking_length>"
            ))
        }
        "adaptive" => {
            let effort = body
                .get("output_config")
                .and_then(|v| v.get("effort"))
                .and_then(|v| v.as_str())
                .unwrap_or("high");
            Some(format!(
                "<thinking_mode>adaptive</thinking_mode><thinking_effort>{effort}</thinking_effort>"
            ))
        }
        _ => None,
    }
}

fn has_thinking_tags(content: &str) -> bool {
    content.contains("<thinking_mode>") || content.contains("<max_thinking_length>")
}

fn merge_user_messages(messages: &[&Value], model_id: &str) -> Value {
    let mut content_parts = Vec::new();
    let mut images = Vec::new();
    let mut tool_results = Vec::new();
    for msg in messages {
        let (content, msg_images, msg_tool_results) = parse_user_content(msg.get("content"));
        if !content.is_empty() {
            content_parts.push(content);
        }
        images.extend(msg_images);
        tool_results.extend(msg_tool_results);
    }
    history_user_message(content_parts.join("\n"), model_id, images, tool_results)
}

fn merge_assistant_messages(
    messages: &[&Value],
    tool_name_map: &mut HashMap<String, String>,
) -> Value {
    let mut content_parts = Vec::new();
    let mut tool_uses = Vec::new();
    for msg in messages {
        let (content, msg_tool_uses) = parse_assistant_content(msg.get("content"), tool_name_map);
        if !content.trim().is_empty() {
            content_parts.push(content);
        }
        tool_uses.extend(msg_tool_uses);
    }
    let content = if content_parts.is_empty() && !tool_uses.is_empty() {
        " ".to_string()
    } else {
        content_parts.join("\n\n")
    };
    history_assistant_message(content, tool_uses)
}

fn history_user_message(
    content: String,
    model_id: &str,
    images: Vec<Value>,
    tool_results: Vec<Value>,
) -> Value {
    json!({
        "userInputMessage": {
            "userInputMessageContext": {
                "envState": env_state(),
                "toolResults": tool_results
            },
            "content": content,
            "modelId": model_id,
            "images": images,
            "origin": "AI_EDITOR"
        }
    })
}

fn history_assistant_message(content: String, tool_uses: Vec<Value>) -> Value {
    let mut message = json!({
        "assistantResponseMessage": {
            "content": content
        }
    });
    if !tool_uses.is_empty() {
        message["assistantResponseMessage"]["toolUses"] = Value::Array(tool_uses);
    }
    message
}

fn validate_tool_pairing(
    history: &[Value],
    tool_results: &[Value],
) -> (Vec<Value>, HashSet<String>) {
    let mut all_tool_use_ids = HashSet::new();
    let mut history_tool_result_ids = HashSet::new();

    for msg in history {
        if let Some(tool_uses) = msg
            .pointer("/assistantResponseMessage/toolUses")
            .and_then(Value::as_array)
        {
            for tool_use in tool_uses {
                if let Some(id) = tool_use.get("toolUseId").and_then(Value::as_str) {
                    all_tool_use_ids.insert(id.to_string());
                }
            }
        }
        if let Some(results) = msg
            .pointer("/userInputMessage/userInputMessageContext/toolResults")
            .and_then(Value::as_array)
        {
            for result in results {
                if let Some(id) = result.get("toolUseId").and_then(Value::as_str) {
                    history_tool_result_ids.insert(id.to_string());
                }
            }
        }
    }

    let mut unpaired: HashSet<String> = all_tool_use_ids
        .difference(&history_tool_result_ids)
        .cloned()
        .collect();
    let mut filtered = Vec::new();
    for result in tool_results {
        let Some(id) = result.get("toolUseId").and_then(Value::as_str) else {
            continue;
        };
        if unpaired.remove(id) {
            filtered.push(result.clone());
        }
    }
    (filtered, unpaired)
}

fn remove_orphaned_tool_uses(history: &mut [Value], orphaned_ids: &HashSet<String>) {
    if orphaned_ids.is_empty() {
        return;
    }
    for msg in history {
        let Some(tool_uses) = msg
            .pointer_mut("/assistantResponseMessage/toolUses")
            .and_then(Value::as_array_mut)
        else {
            continue;
        };
        tool_uses.retain(|tool_use| {
            tool_use
                .get("toolUseId")
                .and_then(Value::as_str)
                .map(|id| !orphaned_ids.contains(id))
                .unwrap_or(true)
        });
        if tool_uses.is_empty() {
            if let Some(obj) = msg
                .get_mut("assistantResponseMessage")
                .and_then(Value::as_object_mut)
            {
                obj.remove("toolUses");
            }
        }
    }
}

fn add_missing_history_tools(tools: &mut Vec<Value>, history: &[Value]) {
    let mut existing_names: HashSet<String> = tools
        .iter()
        .filter_map(|tool| {
            tool.pointer("/toolSpecification/name")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect();
    let mut missing = Vec::new();

    for msg in history {
        let Some(tool_uses) = msg
            .pointer("/assistantResponseMessage/toolUses")
            .and_then(Value::as_array)
        else {
            continue;
        };
        for tool_use in tool_uses {
            let Some(name) = tool_use.get("name").and_then(Value::as_str) else {
                continue;
            };
            if existing_names.insert(name.to_string()) {
                missing.push(json!({
                    "toolSpecification": {
                        "name": name,
                        "description": name,
                        "inputSchema": {
                            "json": {
                                "type": "object",
                                "properties": {},
                                "required": [],
                                "additionalProperties": true
                            }
                        }
                    }
                }));
            }
        }
    }

    tools.extend(missing);
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

fn parse_assistant_content(
    content: Option<&Value>,
    tool_name_map: &mut HashMap<String, String>,
) -> (String, Vec<Value>) {
    let mut text = String::new();
    let mut thinking = String::new();
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
                        let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        let mapped_name = map_tool_name(name, tool_name_map);
                        tool_uses.push(json!({
                            "toolUseId": item.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                            "name": mapped_name,
                            "input": item.get("input").cloned().unwrap_or_else(|| json!({}))
                        }));
                    }
                    Some("thinking") => {
                        if let Some(value) = item.get("thinking").and_then(|v| v.as_str()) {
                            thinking.push_str(value);
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    let content = if !thinking.is_empty() && !text.is_empty() {
        format!("<thinking>{thinking}</thinking>\n\n{text}")
    } else if !thinking.is_empty() {
        format!("<thinking>{thinking}</thinking>")
    } else if text.is_empty() && !tool_uses.is_empty() {
        " ".to_string()
    } else {
        text
    };
    (content, tool_uses)
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
    tool_name_map: HashMap<String, String>,
    text_index: Option<i32>,
    text_stopped: bool,
    thinking_index: Option<i32>,
    thinking_stopped: bool,
    next_index: i32,
    tool_indices: HashMap<String, i32>,
    output_tokens: i32,
    usage: KiroUsageAccumulator,
}

impl SseBuilder {
    fn new(model: String, tool_name_map: HashMap<String, String>) -> Self {
        Self {
            message_id: format!("msg_{}", uuid::Uuid::new_v4().to_string().replace('-', "")),
            model,
            tool_name_map,
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
        if self.thinking_index.is_some() && !self.thinking_stopped {
            self.thinking_stopped = true;
            if let Some(index) = self.thinking_index {
                out.push(sse(
                    "content_block_stop",
                    json!({"type":"content_block_stop","index":index}),
                ));
            }
        }
        if self.text_stopped {
            self.text_index = None;
            self.text_stopped = false;
        }
        let index = if let Some(index) = self.text_index {
            index
        } else {
            let index = self.next_index;
            self.next_index += 1;
            self.text_index = Some(index);
            out.push(sse(
                "content_block_start",
                json!({"type":"content_block_start","index":index,"content_block":{"type":"text","text":""}}),
            ));
            index
        };
        out.push(sse(
            "content_block_delta",
            json!({"type":"content_block_delta","index":index,"delta":{"type":"text_delta","text":text}}),
        ));
        out
    }

    fn thinking_delta(&mut self, text: &str) -> Vec<Bytes> {
        if text.is_empty() {
            return Vec::new();
        }
        self.output_tokens += estimate_tokens(text);
        let mut out = Vec::new();
        if self.text_index.is_some() && !self.text_stopped {
            self.text_stopped = true;
            if let Some(index) = self.text_index {
                out.push(sse(
                    "content_block_stop",
                    json!({"type":"content_block_stop","index":index}),
                ));
            }
        }
        if self.thinking_stopped {
            self.thinking_index = None;
            self.thinking_stopped = false;
        }
        let index = if let Some(index) = self.thinking_index {
            index
        } else {
            let index = self.next_index;
            self.next_index += 1;
            self.thinking_index = Some(index);
            out.push(sse(
                "content_block_start",
                json!({"type":"content_block_start","index":index,"content_block":{"type":"thinking","thinking":""}}),
            ));
            index
        };
        out.push(sse(
            "content_block_delta",
            json!({"type":"content_block_delta","index":index,"delta":{"type":"thinking_delta","thinking":text}}),
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
        let name = original_tool_name(name, &self.tool_name_map);
        let input = payload.get("input").and_then(|v| v.as_str()).unwrap_or("");
        let stop = payload
            .get("stop")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let mut out = Vec::new();
        if self.thinking_index.is_some() && !self.thinking_stopped {
            self.thinking_stopped = true;
            if let Some(index) = self.thinking_index {
                out.push(sse(
                    "content_block_stop",
                    json!({"type":"content_block_stop","index":index}),
                ));
            }
        }
        if self.text_index.is_some() && !self.text_stopped {
            self.text_stopped = true;
            let index = self.text_index.unwrap_or(0);
            out.push(sse(
                "content_block_stop",
                json!({"type":"content_block_stop","index":index}),
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
        if self.thinking_index.is_some() && !self.thinking_stopped {
            self.thinking_stopped = true;
            if let Some(index) = self.thinking_index {
                out.push(sse(
                    "content_block_stop",
                    json!({"type":"content_block_stop","index":index}),
                ));
            }
        }
        if self.text_index.is_some() && !self.text_stopped {
            self.text_stopped = true;
            let index = self.text_index.unwrap_or(0);
            out.push(sse(
                "content_block_stop",
                json!({"type":"content_block_stop","index":index}),
            ));
        }
        let stop_reason = if self.tool_indices.is_empty() {
            "end_turn"
        } else {
            "tool_use"
        };
        let usage = self.usage.final_usage(self.output_tokens);
        out.push(sse(
            "message_delta",
            json!({
                "type":"message_delta",
                "delta":{"stop_reason":stop_reason,"stop_sequence":null},
                "usage":{
                    "input_tokens":usage.input_tokens,
                    "output_tokens":usage.output_tokens,
                    "cache_read_input_tokens":usage.cache_read_tokens,
                    "cache_creation_input_tokens":usage.cache_creation_tokens
                }
            }),
        ));
        out.push(sse("message_stop", json!({"type":"message_stop"})));
        out
    }

    fn usage_event(&mut self, event_type: &str, payload: &Value) {
        self.usage.apply_event(event_type, payload, &self.model);
    }

    fn set_prompt_cache_usage(&mut self, usage: KiroPromptCacheUsage) {
        self.usage.set_prompt_cache_usage(usage);
    }
}

fn kiro_event_stream_to_anthropic_sse(
    stream: impl Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
    model: String,
    tool_name_map: HashMap<String, String>,
    prompt_cache_usage: KiroPromptCacheUsage,
) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send {
    async_stream::stream! {
        let mut buffer = BytesMut::new();
        let mut builder = SseBuilder::new(model, tool_name_map);
        builder.set_prompt_cache_usage(prompt_cache_usage);
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
            Some("codeEvent") => {
                let payload: Value = serde_json::from_slice(&frame.payload).unwrap_or(Value::Null);
                let text = payload
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                builder.assistant_delta(text)
            }
            Some("reasoningContentEvent") => {
                let payload: Value = serde_json::from_slice(&frame.payload).unwrap_or(Value::Null);
                builder.thinking_delta(reasoning_text(&payload).unwrap_or(""))
            }
            Some("toolUseEvent") => {
                let payload: Value = serde_json::from_slice(&frame.payload).unwrap_or(Value::Null);
                builder.tool_delta(&payload)
            }
            Some("contextUsageEvent") | Some("metricsEvent") | Some("meteringEvent") => {
                let event_type = frame_event_type(frame).unwrap_or_default();
                let payload: Value = serde_json::from_slice(&frame.payload).unwrap_or(Value::Null);
                builder.usage_event(event_type, &payload);
                Vec::new()
            }
            _ => Vec::new(),
        },
    }
}

fn kiro_event_bytes_to_anthropic_json(
    bytes: &[u8],
    model: &str,
    tool_name_map: &HashMap<String, String>,
    prompt_cache_usage: KiroPromptCacheUsage,
) -> Value {
    let mut buffer = BytesMut::from(bytes);
    let mut text = String::new();
    let mut thinking = String::new();
    let mut tools: HashMap<String, (String, String)> = HashMap::new();
    let mut usage = KiroUsageAccumulator::default();
    usage.set_prompt_cache_usage(prompt_cache_usage);
    for frame in parse_frames(&mut buffer) {
        match frame_event_type(&frame) {
            Some("assistantResponseEvent") => {
                let payload: Value = serde_json::from_slice(&frame.payload).unwrap_or(Value::Null);
                if let Some(chunk) = payload.get("content").and_then(|v| v.as_str()) {
                    text.push_str(chunk);
                }
            }
            Some("codeEvent") => {
                let payload: Value = serde_json::from_slice(&frame.payload).unwrap_or(Value::Null);
                if let Some(chunk) = payload.get("content").and_then(|v| v.as_str()) {
                    text.push_str(chunk);
                }
            }
            Some("reasoningContentEvent") => {
                let payload: Value = serde_json::from_slice(&frame.payload).unwrap_or(Value::Null);
                if let Some(chunk) = reasoning_text(&payload) {
                    thinking.push_str(chunk);
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
                let name = original_tool_name(&name, tool_name_map);
                let input = payload
                    .get("input")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let entry = tools.entry(id).or_insert((name, String::new()));
                entry.1.push_str(&input);
            }
            Some("contextUsageEvent") | Some("metricsEvent") | Some("meteringEvent") => {
                let event_type = frame_event_type(&frame).unwrap_or_default();
                let payload: Value = serde_json::from_slice(&frame.payload).unwrap_or(Value::Null);
                usage.apply_event(event_type, &payload, model);
            }
            _ => {}
        }
    }

    let mut content = Vec::new();
    if !thinking.is_empty() {
        content.push(json!({"type":"thinking","thinking":thinking}));
    }
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
    let fallback_output_tokens = estimate_tokens(&format!("{thinking}{text}"));
    let usage = usage.final_usage(fallback_output_tokens);
    json!({
        "id": format!("msg_{}", uuid::Uuid::new_v4().to_string().replace('-', "")),
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": content,
        "stop_reason": stop_reason,
        "stop_sequence": null,
        "usage": {
            "input_tokens": usage.input_tokens,
            "output_tokens": usage.output_tokens,
            "cache_read_input_tokens": usage.cache_read_tokens,
            "cache_creation_input_tokens": usage.cache_creation_tokens
        }
    })
}

#[derive(Debug, Clone, Copy, Default)]
struct KiroPromptCacheUsage {
    cache_read_tokens: i32,
    cache_creation_tokens: i32,
}

#[derive(Debug, Clone)]
struct KiroPromptCacheSegment {
    hash: u64,
    cumulative_tokens: u32,
    ttl_secs: i64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct KiroPromptCacheEntry {
    tokens: u32,
    expires_at: i64,
    last_hit_at: i64,
}

struct KiroPromptCache {
    entries: Mutex<HashMap<u64, KiroPromptCacheEntry>>,
    persist_path: Option<PathBuf>,
}

impl KiroPromptCache {
    fn new(persist_path: Option<PathBuf>) -> Self {
        let entries = persist_path
            .as_ref()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| {
                serde_json::from_slice::<HashMap<u64, KiroPromptCacheEntry>>(&bytes).ok()
            })
            .map(|entries| {
                let now = unix_timestamp_secs();
                entries
                    .into_iter()
                    .filter(|(_, entry)| entry.expires_at > now)
                    .collect()
            })
            .unwrap_or_default();
        Self {
            entries: Mutex::new(entries),
            persist_path,
        }
    }

    fn compute_usage(&self, segments: &[KiroPromptCacheSegment]) -> KiroPromptCacheUsage {
        if segments.is_empty() {
            return KiroPromptCacheUsage::default();
        }

        let now = unix_timestamp_secs();
        let mut entries = self.entries.lock().unwrap_or_else(|err| err.into_inner());
        entries.retain(|_, entry| entry.expires_at > now);

        let deepest_hit = segments
            .iter()
            .enumerate()
            .rev()
            .find_map(|(idx, segment)| {
                entries.get_mut(&segment.hash).and_then(|entry| {
                    if entry.expires_at > now {
                        entry.last_hit_at = now;
                        Some(idx)
                    } else {
                        None
                    }
                })
            });

        let total = segments
            .last()
            .map(|segment| segment.cumulative_tokens)
            .unwrap_or(0);
        let (cache_creation_tokens, cache_read_tokens) = match deepest_hit {
            Some(idx) => (
                total.saturating_sub(segments[idx].cumulative_tokens),
                segments[idx].cumulative_tokens,
            ),
            None => (total, 0),
        };

        for segment in segments {
            entries.insert(
                segment.hash,
                KiroPromptCacheEntry {
                    tokens: segment.cumulative_tokens,
                    expires_at: now + segment.ttl_secs.clamp(60, PROMPT_CACHE_MAX_TTL_SECS),
                    last_hit_at: now,
                },
            );
        }
        if entries.len() > PROMPT_CACHE_CAPACITY {
            let drop_count = entries.len() - PROMPT_CACHE_CAPACITY;
            let mut victims = entries
                .iter()
                .map(|(hash, entry)| (*hash, entry.last_hit_at))
                .collect::<Vec<_>>();
            victims.sort_by_key(|(_, last_hit_at)| *last_hit_at);
            for (hash, _) in victims.into_iter().take(drop_count) {
                entries.remove(&hash);
            }
        }

        let snapshot = entries.clone();
        drop(entries);
        self.flush_snapshot(snapshot);

        KiroPromptCacheUsage {
            cache_read_tokens: cache_read_tokens as i32,
            cache_creation_tokens: cache_creation_tokens as i32,
        }
    }

    fn flush_snapshot(&self, snapshot: HashMap<u64, KiroPromptCacheEntry>) {
        let Some(path) = self.persist_path.as_ref() else {
            return;
        };
        if let Some(parent) = path.parent() {
            if let Err(err) = std::fs::create_dir_all(parent) {
                log::warn!("Kiro PromptCache 创建目录失败 {}: {err}", parent.display());
                return;
            }
        }
        match serde_json::to_vec(&snapshot) {
            Ok(bytes) => {
                if let Err(err) = std::fs::write(path, bytes) {
                    log::warn!("Kiro PromptCache 写入失败 {}: {err}", path.display());
                }
            }
            Err(err) => log::warn!("Kiro PromptCache 序列化失败: {err}"),
        }
    }
}

fn compute_kiro_prompt_cache_usage(body: &Value) -> KiroPromptCacheUsage {
    compute_kiro_prompt_cache_usage_with_cache(body, &KIRO_PROMPT_CACHE)
}

fn compute_kiro_prompt_cache_usage_with_cache(
    body: &Value,
    cache: &KiroPromptCache,
) -> KiroPromptCacheUsage {
    let segments = extract_kiro_prompt_cache_segments(body);
    cache.compute_usage(&segments)
}

fn extract_kiro_prompt_cache_segments(body: &Value) -> Vec<KiroPromptCacheSegment> {
    let mut hasher = Sha256::new();
    let mut cumulative_tokens = 0u32;
    let mut segments = Vec::new();

    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        for tool in tools {
            feed_prompt_cache_value(&mut hasher, tool, &mut cumulative_tokens);
            if let Some(cache_control) = tool.get("cache_control") {
                commit_prompt_cache_segment(
                    &hasher,
                    cumulative_tokens,
                    cache_control,
                    &mut segments,
                );
            }
        }
    }

    match body.get("system") {
        Some(Value::String(system)) => {
            feed_prompt_cache_text(&mut hasher, system, &mut cumulative_tokens)
        }
        Some(Value::Array(items)) => {
            for item in items {
                feed_prompt_cache_value(&mut hasher, item, &mut cumulative_tokens);
                if let Some(cache_control) = item.get("cache_control") {
                    commit_prompt_cache_segment(
                        &hasher,
                        cumulative_tokens,
                        cache_control,
                        &mut segments,
                    );
                }
            }
        }
        _ => {}
    }

    if let Some(messages) = body.get("messages").and_then(Value::as_array) {
        for message in messages {
            if let Some(role) = message.get("role").and_then(Value::as_str) {
                feed_prompt_cache_text(&mut hasher, role, &mut cumulative_tokens);
            }
            match message.get("content") {
                Some(Value::String(text)) => {
                    feed_prompt_cache_text(&mut hasher, text, &mut cumulative_tokens);
                }
                Some(Value::Array(blocks)) => {
                    for block in blocks {
                        feed_prompt_cache_value(&mut hasher, block, &mut cumulative_tokens);
                        if let Some(cache_control) = block.get("cache_control") {
                            commit_prompt_cache_segment(
                                &hasher,
                                cumulative_tokens,
                                cache_control,
                                &mut segments,
                            );
                        }
                    }
                }
                Some(other) => feed_prompt_cache_value(&mut hasher, other, &mut cumulative_tokens),
                None => {}
            }
            if let Some(cache_control) = message.get("cache_control") {
                commit_prompt_cache_segment(
                    &hasher,
                    cumulative_tokens,
                    cache_control,
                    &mut segments,
                );
            }
        }
    }

    segments
}

fn feed_prompt_cache_value(hasher: &mut Sha256, value: &Value, cumulative_tokens: &mut u32) {
    let signature = prompt_cache_signature(value);
    feed_prompt_cache_text(hasher, &signature, cumulative_tokens);
}

fn feed_prompt_cache_text(hasher: &mut Sha256, text: &str, cumulative_tokens: &mut u32) {
    if text.is_empty() {
        return;
    }
    hasher.update(text.as_bytes());
    *cumulative_tokens = cumulative_tokens.saturating_add(estimate_tokens(text).max(0) as u32);
}

fn prompt_cache_signature(value: &Value) -> String {
    serde_json::to_string(&canonical_prompt_cache_value(value)).unwrap_or_default()
}

fn canonical_prompt_cache_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut normalized = serde_json::Map::new();
            let mut keys = map.keys().collect::<Vec<_>>();
            keys.sort();
            for key in keys {
                if key != "cache_control" {
                    if let Some(child) = map.get(key) {
                        normalized.insert(key.clone(), canonical_prompt_cache_value(child));
                    }
                }
            }
            Value::Object(normalized)
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(canonical_prompt_cache_value)
                .collect::<Vec<_>>(),
        ),
        _ => value.clone(),
    }
}

fn commit_prompt_cache_segment(
    hasher: &Sha256,
    cumulative_tokens: u32,
    cache_control: &Value,
    segments: &mut Vec<KiroPromptCacheSegment>,
) {
    if cumulative_tokens == 0 {
        return;
    }
    let digest = hasher.clone().finalize();
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    segments.push(KiroPromptCacheSegment {
        hash: u64::from_be_bytes(bytes),
        cumulative_tokens,
        ttl_secs: parse_prompt_cache_ttl(cache_control),
    });
}

fn parse_prompt_cache_ttl(cache_control: &Value) -> i64 {
    match cache_control.get("ttl").and_then(Value::as_str) {
        Some(ttl) if ttl.eq_ignore_ascii_case("1h") => 60 * 60,
        Some(ttl) if ttl.eq_ignore_ascii_case("5m") => 5 * 60,
        _ => PROMPT_CACHE_DEFAULT_TTL_SECS,
    }
}

fn unix_timestamp_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, Copy, Default)]
struct KiroUsage {
    input_tokens: i32,
    output_tokens: i32,
    cache_read_tokens: i32,
    cache_creation_tokens: i32,
}

#[derive(Debug, Clone, Default)]
struct KiroUsageAccumulator {
    context_input_tokens: Option<i32>,
    metrics_input_tokens: Option<i32>,
    output_tokens: Option<i32>,
    cache_read_tokens: Option<i32>,
    cache_creation_tokens: Option<i32>,
    prompt_cache_read_tokens: i32,
    prompt_cache_creation_tokens: i32,
}

impl KiroUsageAccumulator {
    fn set_prompt_cache_usage(&mut self, usage: KiroPromptCacheUsage) {
        self.prompt_cache_read_tokens = usage.cache_read_tokens;
        self.prompt_cache_creation_tokens = usage.cache_creation_tokens;
    }

    fn apply_event(&mut self, event_type: &str, payload: &Value, model: &str) {
        match event_type {
            "contextUsageEvent" => {
                if let Some(tokens) = context_usage_input_tokens(payload, model) {
                    self.context_input_tokens = Some(tokens);
                }
            }
            "metricsEvent" => self.apply_metrics(payload),
            "meteringEvent" => {}
            _ => {}
        }
    }

    fn apply_metrics(&mut self, payload: &Value) {
        let metrics = payload.get("metricsEvent").unwrap_or(payload);
        if let Some(tokens) = number_field(
            metrics,
            &[
                "inputTokens",
                "input_tokens",
                "promptTokens",
                "prompt_tokens",
            ],
        ) {
            self.metrics_input_tokens = Some(tokens);
        }
        if let Some(tokens) = number_field(
            metrics,
            &[
                "outputTokens",
                "output_tokens",
                "completionTokens",
                "completion_tokens",
            ],
        ) {
            self.output_tokens = Some(tokens);
        }
        if let Some(tokens) = number_field(
            metrics,
            &[
                "cacheReadInputTokens",
                "cache_read_input_tokens",
                "cacheReadTokens",
            ],
        ) {
            self.cache_read_tokens = Some(tokens);
        }
        if let Some(tokens) = number_field(
            metrics,
            &[
                "cacheCreationInputTokens",
                "cache_creation_input_tokens",
                "cacheCreationTokens",
            ],
        ) {
            self.cache_creation_tokens = Some(tokens);
        }
    }

    fn final_usage(&self, fallback_output_tokens: i32) -> KiroUsage {
        let raw_input_tokens = self
            .metrics_input_tokens
            .or(self.context_input_tokens)
            .unwrap_or(0)
            .max(0);
        let cache_read_tokens = self
            .cache_read_tokens
            .unwrap_or(self.prompt_cache_read_tokens)
            .max(0);
        let cache_creation_tokens = self
            .cache_creation_tokens
            .unwrap_or(self.prompt_cache_creation_tokens)
            .max(0);
        KiroUsage {
            input_tokens: raw_input_tokens
                .saturating_sub(cache_read_tokens)
                .saturating_sub(cache_creation_tokens),
            output_tokens: self.output_tokens.unwrap_or(fallback_output_tokens).max(0),
            cache_read_tokens,
            cache_creation_tokens,
        }
    }
}

fn reasoning_text(payload: &Value) -> Option<&str> {
    if let Some(text) = payload.as_str() {
        return Some(text);
    }
    let value = payload.get("reasoningContentEvent").unwrap_or(payload);
    value
        .as_str()
        .or_else(|| value.get("text").and_then(Value::as_str))
        .or_else(|| value.get("content").and_then(Value::as_str))
        .or_else(|| value.get("reasoningContent").and_then(Value::as_str))
}

fn sse(event: &str, data: Value) -> Bytes {
    Bytes::from(format!(
        "event: {event}\ndata: {}\n\n",
        serde_json::to_string(&data).unwrap_or_default()
    ))
}

fn context_usage_input_tokens(payload: &Value, model: &str) -> Option<i32> {
    let value = payload.get("contextUsageEvent").unwrap_or(payload);
    let percentage = number_f64_field(value, &["contextUsagePercentage"])?;
    Some((percentage * context_window_size(model) as f64 / 100.0).floor() as i32)
        .filter(|tokens| *tokens > 0)
}

fn number_field(value: &Value, keys: &[&str]) -> Option<i32> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(number_value))
}

fn number_f64_field(value: &Value, keys: &[&str]) -> Option<f64> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(number_f64_value))
}

fn number_value(value: &Value) -> Option<i32> {
    if let Some(n) = value.as_i64() {
        return i32::try_from(n).ok();
    }
    if let Some(n) = value.as_u64() {
        return i32::try_from(n).ok();
    }
    value.as_f64().and_then(|n| {
        if n.is_finite() && n >= 0.0 && n <= i32::MAX as f64 {
            Some(n as i32)
        } else {
            None
        }
    })
}

fn number_f64_value(value: &Value) -> Option<f64> {
    if let Some(n) = value.as_f64() {
        return n.is_finite().then_some(n);
    }
    if let Some(n) = value.as_i64() {
        return Some(n as f64);
    }
    value.as_u64().map(|n| n as f64)
}

fn context_window_size(model: &str) -> i32 {
    let normalized = model.to_ascii_lowercase();
    if normalized.contains("[1m]") || normalized.contains("-1m") {
        1_000_000
    } else {
        200_000
    }
}

fn estimate_tokens(text: &str) -> i32 {
    ((text.chars().count() as f64) / 4.0).ceil() as i32
}

fn is_quota_exhausted(body: &str) -> bool {
    const REASONS: &[&str] = &["MONTHLY_REQUEST_COUNT", "OVERAGE_REQUEST_LIMIT_EXCEEDED"];
    if !REASONS.iter().any(|reason| body.contains(reason)) {
        return false;
    }
    if let Ok(value) = serde_json::from_str::<Value>(body) {
        let top = value.get("reason").and_then(Value::as_str);
        let nested = value.pointer("/error/reason").and_then(Value::as_str);
        return [top, nested]
            .into_iter()
            .flatten()
            .any(|reason| REASONS.contains(&reason));
    }
    true
}

fn is_account_throttled(status: reqwest::StatusCode, body: &str) -> bool {
    status == reqwest::StatusCode::TOO_MANY_REQUESTS
        && body.contains("suspicious activity")
        && body.contains("temporary limits")
}

#[allow(dead_code)]
fn default_profile_arn_for_builder_id() -> &'static str {
    BUILDER_ID_PROFILE_ARN
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_account() -> KiroAccountData {
        KiroAccountData {
            account_id: "kiro_test".to_string(),
            email: None,
            refresh_token: "refresh".to_string(),
            profile_arn: None,
            auth_region: "us-east-1".to_string(),
            api_region: "us-east-1".to_string(),
            machine_id: None,
            client_id: Some("client".to_string()),
            client_secret: Some("secret".to_string()),
            client_secret_expires_at: None,
            start_url: None,
            auth_method: Some("builder-id".to_string()),
            authenticated_at: 1,
        }
    }

    #[test]
    fn map_model_supports_4_8_aliases() {
        assert_eq!(map_model("claude-sonnet-4-8"), Some("claude-sonnet-4.8"));
        assert_eq!(map_model("claude-opus-4.8"), Some("claude-opus-4.8"));
        assert_eq!(map_model("claude-haiku-4-5"), Some("claude-haiku-4.5"));
    }

    #[test]
    fn conversion_drops_trailing_prefill_and_normalizes_tool_schema() {
        let long_tool_name = format!("tool_{}", "x".repeat(80));
        let body = json!({
            "model": "claude-sonnet-4-8",
            "messages": [
                {"role": "user", "content": "first"},
                {"role": "assistant", "content": "answer"},
                {"role": "user", "content": "second"},
                {"role": "assistant", "content": "prefill"}
            ],
            "tools": [{
                "name": long_tool_name,
                "description": "",
                "input_schema": {
                    "$schema": "http://json-schema.org/draft-07/schema#",
                    "type": "object",
                    "properties": {
                        "count": {
                            "type": "number",
                            "exclusiveMinimum": 1,
                            "maximum": 9999999999999.0
                        }
                    }
                }
            }]
        });

        let request = anthropic_to_kiro_request(&body, &test_account()).unwrap();
        let state = request.body.get("conversationState").unwrap();
        assert_eq!(
            state.pointer("/currentMessage/userInputMessage/content"),
            Some(&json!("second"))
        );
        assert_eq!(
            state.pointer("/currentMessage/userInputMessage/modelId"),
            Some(&json!("claude-sonnet-4.8"))
        );

        let tool = state
            .pointer("/currentMessage/userInputMessage/userInputMessageContext/tools/0/toolSpecification")
            .unwrap();
        let mapped_name = tool.get("name").and_then(Value::as_str).unwrap();
        assert!(mapped_name.chars().count() <= TOOL_NAME_MAX_LEN);
        assert_eq!(
            request.tool_name_map.get(mapped_name),
            Some(&long_tool_name)
        );
        assert_eq!(tool.get("description"), Some(&json!(long_tool_name)));
        let property = tool.pointer("/inputSchema/json/properties/count").unwrap();
        assert!(property.get("exclusiveMinimum").is_none());
        assert!(property.get("maximum").is_none());
    }

    #[test]
    fn conversion_removes_orphaned_history_tool_use() {
        let body = json!({
            "model": "claude-sonnet-4-8",
            "messages": [
                {"role": "user", "content": "run tool"},
                {"role": "assistant", "content": [{
                    "type": "tool_use",
                    "id": "toolu_1",
                    "name": "Read",
                    "input": {"file_path": "Cargo.toml"}
                }]},
                {"role": "user", "content": "continue"}
            ],
            "tools": [{
                "name": "Read",
                "description": "read file",
                "input_schema": {"type": "object", "properties": {}}
            }]
        });

        let request = anthropic_to_kiro_request(&body, &test_account()).unwrap();
        let history = request
            .body
            .pointer("/conversationState/history")
            .and_then(Value::as_array)
            .unwrap();
        assert!(history
            .iter()
            .all(|msg| msg.pointer("/assistantResponseMessage/toolUses").is_none()));
    }

    #[test]
    fn sse_builder_restores_original_tool_name() {
        let mut tool_name_map = HashMap::new();
        tool_name_map.insert(
            "short_name".to_string(),
            "very_long_original_name".to_string(),
        );
        let mut builder = SseBuilder::new("claude-sonnet-4-8".to_string(), tool_name_map);
        let bytes = builder
            .tool_delta(&json!({
                "toolUseId": "toolu_1",
                "name": "short_name",
                "input": "{\"x\":",
                "stop": false
            }))
            .into_iter()
            .map(|b| String::from_utf8(b.to_vec()).unwrap())
            .collect::<Vec<_>>()
            .join("");
        assert!(bytes.contains("very_long_original_name"));
    }

    #[test]
    fn kiro_reasoning_and_code_events_emit_claude_blocks() {
        let mut builder = SseBuilder::new("claude-sonnet-4-8".to_string(), HashMap::new());
        let reasoning_frame = KiroFrame {
            headers: HashMap::from([(
                ":event-type".to_string(),
                "reasoningContentEvent".to_string(),
            )]),
            payload: serde_json::to_vec(&json!({
                "reasoningContentEvent": { "text": "think first" }
            }))
            .unwrap(),
        };
        let code_frame = KiroFrame {
            headers: HashMap::from([(":event-type".to_string(), "codeEvent".to_string())]),
            payload: serde_json::to_vec(&json!({ "content": "visible answer" })).unwrap(),
        };

        let bytes = process_frame_to_sse(&mut builder, &reasoning_frame)
            .into_iter()
            .chain(process_frame_to_sse(&mut builder, &code_frame))
            .map(|b| String::from_utf8(b.to_vec()).unwrap())
            .collect::<Vec<_>>()
            .join("");

        assert!(bytes.contains("\"type\":\"thinking\""));
        assert!(bytes.contains("\"type\":\"thinking_delta\""));
        assert!(bytes.contains("\"thinking\":\"think first\""));
        assert!(bytes.contains("\"type\":\"text_delta\""));
        assert!(bytes.contains("\"text\":\"visible answer\""));
    }

    #[test]
    fn kiro_usage_events_are_emitted_in_final_claude_delta() {
        let mut builder = SseBuilder::new("claude-sonnet-4-8".to_string(), HashMap::new());
        builder.usage_event(
            "contextUsageEvent",
            &json!({ "contextUsagePercentage": 1.5 }),
        );
        builder.usage_event(
            "metricsEvent",
            &json!({
                "metricsEvent": {
                    "outputTokens": 42,
                    "cacheReadInputTokens": 7,
                    "cacheCreationInputTokens": 11
                }
            }),
        );

        let bytes = builder
            .final_events()
            .into_iter()
            .map(|b| String::from_utf8(b.to_vec()).unwrap())
            .collect::<Vec<_>>()
            .join("");

        assert!(bytes.contains("\"input_tokens\":2982"));
        assert!(bytes.contains("\"output_tokens\":42"));
        assert!(bytes.contains("\"cache_read_input_tokens\":7"));
        assert!(bytes.contains("\"cache_creation_input_tokens\":11"));
    }

    #[test]
    fn kiro_prompt_cache_miss_then_hit_from_cache_control() {
        let cache = KiroPromptCache::new(None);
        let body = json!({
            "model": "claude-opus-4-7",
            "system": [
                {
                    "type": "text",
                    "text": "You are a coding agent. ".repeat(200),
                    "cache_control": { "type": "ephemeral", "ttl": "5m" }
                }
            ],
            "messages": [
                {
                    "role": "user",
                    "content": "hello"
                }
            ]
        });

        let first = compute_kiro_prompt_cache_usage_with_cache(&body, &cache);
        assert!(first.cache_creation_tokens > 0);
        assert_eq!(first.cache_read_tokens, 0);

        let second = compute_kiro_prompt_cache_usage_with_cache(&body, &cache);
        assert_eq!(second.cache_creation_tokens, 0);
        assert_eq!(second.cache_read_tokens, first.cache_creation_tokens);
    }

    #[test]
    fn kiro_prompt_cache_signature_ignores_object_key_order() {
        let cache = KiroPromptCache::new(None);
        let first = json!({
            "tools": [
                {
                    "name": "Read",
                    "description": "Read files",
                    "input_schema": {
                        "type": "object",
                        "properties": {
                            "path": { "type": "string", "description": "Path" }
                        }
                    },
                    "cache_control": { "type": "ephemeral" }
                }
            ],
            "messages": [{ "role": "user", "content": "hello" }]
        });
        let second = json!({
            "tools": [
                {
                    "cache_control": { "type": "ephemeral" },
                    "input_schema": {
                        "properties": {
                            "path": { "description": "Path", "type": "string" }
                        },
                        "type": "object"
                    },
                    "description": "Read files",
                    "name": "Read"
                }
            ],
            "messages": [{ "content": "hello", "role": "user" }]
        });

        let miss = compute_kiro_prompt_cache_usage_with_cache(&first, &cache);
        let hit = compute_kiro_prompt_cache_usage_with_cache(&second, &cache);

        assert!(miss.cache_creation_tokens > 0);
        assert_eq!(hit.cache_read_tokens, miss.cache_creation_tokens);
        assert_eq!(hit.cache_creation_tokens, 0);
    }

    #[test]
    fn kiro_prompt_cache_tokens_are_subtracted_from_fresh_input() {
        let mut usage = KiroUsageAccumulator::default();
        usage.set_prompt_cache_usage(KiroPromptCacheUsage {
            cache_read_tokens: 700,
            cache_creation_tokens: 30,
        });
        usage.apply_event(
            "metricsEvent",
            &json!({ "metricsEvent": { "inputTokens": 1_000, "outputTokens": 9 } }),
            "claude-opus-4-7",
        );

        let usage = usage.final_usage(1);
        assert_eq!(usage.input_tokens, 270);
        assert_eq!(usage.output_tokens, 9);
        assert_eq!(usage.cache_read_tokens, 700);
        assert_eq!(usage.cache_creation_tokens, 30);
    }

    #[test]
    fn kiro_metrics_input_overrides_context_usage_when_available() {
        let mut usage = KiroUsageAccumulator::default();
        usage.apply_event(
            "contextUsageEvent",
            &json!({ "contextUsageEvent": { "contextUsagePercentage": 2.0 } }),
            "claude-sonnet-4-8[1m]",
        );
        usage.apply_event(
            "metricsEvent",
            &json!({ "inputTokens": 123, "outputTokens": 9 }),
            "claude-sonnet-4-8[1m]",
        );

        let usage = usage.final_usage(1);
        assert_eq!(usage.input_tokens, 123);
        assert_eq!(usage.output_tokens, 9);
    }

    #[test]
    fn kiro_metrics_input_keeps_priority_when_context_arrives_later() {
        let mut usage = KiroUsageAccumulator::default();
        usage.apply_event(
            "metricsEvent",
            &json!({ "metricsEvent": { "inputTokens": 123, "outputTokens": 9 } }),
            "claude-sonnet-4-8",
        );
        usage.apply_event(
            "contextUsageEvent",
            &json!({ "contextUsagePercentage": 2.0 }),
            "claude-sonnet-4-8",
        );

        let usage = usage.final_usage(1);
        assert_eq!(usage.input_tokens, 123);
        assert_eq!(usage.output_tokens, 9);
    }

    #[test]
    fn detects_kiro_quota_and_account_throttle_errors() {
        assert!(is_quota_exhausted(
            r#"{"error":{"reason":"OVERAGE_REQUEST_LIMIT_EXCEEDED"}}"#
        ));
        assert!(is_account_throttled(
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            "Due to suspicious activity, we are imposing temporary limits"
        ));
    }
}
