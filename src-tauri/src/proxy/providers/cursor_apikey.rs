//! Cursor API key provider.
//!
//! Exchanges a user Cursor API key for Cursor's internal access token, then
//! drives requests over either the legacy `ChatService` (text-only) or the
//! newer `agent.v1.AgentService/Run` (tools + images + Responses state).

use crate::provider::Provider;
use crate::proxy::hyper_client::ProxyResponse;
use crate::proxy::ProxyError;
use axum::http::HeaderMap;
use http::StatusCode;
use once_cell::sync::Lazy;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use tokio::sync::Mutex;

use super::cursor_agent_service::{run_agent, AgentRunOptions};
use super::cursor_oauth_auth::{CursorAccountData, DEFAULT_CURSOR_CLIENT_VERSION};
use super::cursor_protocol::{
    conversation_id_from_headers, prepare_cursor_codex_body, requested_model, response_error_body,
    response_to_json, response_to_sse_stream, send_cursor_request, stable_uuid_like,
    CursorRequestContext, CursorResponseFormat,
};
use super::cursor_request_builder::{build_plan, InboundProtocol, ToolResultBlock};
use super::cursor_router::{select_protocol, select_tool_mode, CursorProtocol, CursorToolMode};
use super::cursor_session::{self, CursorSessionManager};

const DEFAULT_CURSOR_BACKEND_BASE_URL: &str = "https://api2.cursor.sh";
const EXCHANGE_USER_API_KEY_PATH: &str = "/auth/exchange_user_api_key";
const ACCESS_TOKEN_CACHE_TTL_MS: i64 = 30 * 60 * 1000;

static ACCESS_TOKEN_CACHE: Lazy<Mutex<HashMap<String, CachedAccessToken>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

#[derive(Debug, Clone)]
struct CachedAccessToken {
    token: String,
    expires_at_ms: i64,
}

impl CachedAccessToken {
    fn is_valid(&self) -> bool {
        self.expires_at_ms > chrono::Utc::now().timestamp_millis()
    }
}

#[derive(Debug, Deserialize)]
struct CursorApiKeyExchangeResponse {
    #[serde(default, rename = "accessToken", alias = "access_token")]
    access_token: Option<String>,
}

pub async fn forward_cursor_apikey_claude(
    provider: &Provider,
    headers: Option<&HeaderMap>,
    body: &Value,
) -> Result<(ProxyResponse, String), ProxyError> {
    let (mapped_body, response_model, upstream_model) =
        prepare_cursor_apikey_claude_body(provider, body);
    let response = forward_cursor_apikey(
        provider,
        headers,
        &mapped_body,
        response_model,
        CursorResponseFormat::AnthropicMessages,
        InboundProtocol::AnthropicMessages,
        body,
    )
    .await?;
    Ok((response, upstream_model))
}

pub async fn forward_cursor_apikey_codex(
    provider: &Provider,
    headers: Option<&HeaderMap>,
    endpoint: &str,
    body: &Value,
) -> Result<(ProxyResponse, String), ProxyError> {
    let (mapped_body, response_model, upstream_model) = prepare_cursor_codex_body(provider, body);
    let (format, inbound) = if endpoint.contains("/chat/completions") {
        (
            CursorResponseFormat::OpenAiChatCompletions,
            InboundProtocol::OpenAiChat,
        )
    } else {
        (
            CursorResponseFormat::OpenAiResponses,
            InboundProtocol::OpenAiResponses,
        )
    };
    let response = forward_cursor_apikey(
        provider,
        headers,
        &mapped_body,
        response_model,
        format,
        inbound,
        body,
    )
    .await?;
    Ok((response, upstream_model))
}

#[allow(clippy::too_many_arguments)]
async fn forward_cursor_apikey(
    provider: &Provider,
    headers: Option<&HeaderMap>,
    mapped_body: &Value,
    response_model: String,
    response_format: CursorResponseFormat,
    inbound: InboundProtocol,
    original_body: &Value,
) -> Result<ProxyResponse, ProxyError> {
    let api_key = cursor_api_key_from_provider(provider)?;
    let access_token = get_cursor_access_token(&api_key, false).await?;
    let account = account_for_api_key(&api_key);

    let protocol = select_protocol(provider, inbound, mapped_body);
    log::debug!(
        "[CursorApiKey] 选定协议: {:?}（model={}）",
        protocol,
        requested_model(mapped_body)
    );

    match protocol {
        CursorProtocol::AgentService => {
            let mut plan = build_plan(inbound, mapped_body);
            let session_key = derive_session_key(
                headers,
                mapped_body,
                cursor_session::global(),
                &plan.tool_results,
                plan.previous_response_id.as_deref(),
            )
            .await;
            if matches!(select_tool_mode(provider), CursorToolMode::Disabled) {
                plan.tools.clear();
            }
            let is_stream = original_body
                .get("stream")
                .and_then(Value::as_bool)
                .unwrap_or(matches!(inbound, InboundProtocol::OpenAiResponses));
            let try_once = |access_token: String| {
                let account = account.clone();
                let session_key = session_key.clone();
                let plan = plan.clone();
                let response_model = response_model.clone();
                async move {
                    run_agent(AgentRunOptions {
                        account: &account,
                        access_token: &access_token,
                        session_manager: cursor_session::global(),
                        session_key,
                        plan,
                        format: response_format,
                        response_model,
                        stream: is_stream,
                    })
                    .await
                }
            };
            match try_once(access_token.clone()).await {
                Ok(resp) => Ok(resp),
                Err(ProxyError::UpstreamError { status: 401, .. }) => {
                    invalidate_cached_access_token(&api_key).await;
                    let refreshed = get_cursor_access_token(&api_key, true).await?;
                    try_once(refreshed).await
                }
                Err(e) => Err(e),
            }
        }
        CursorProtocol::ChatService => {
            let request_body = normalize_cursor_body(mapped_body);
            let conversation_id = conversation_id_from_headers(headers);
            let response = send_cursor_request(&CursorRequestContext {
                account: account.clone(),
                access_token: access_token.clone(),
                body: request_body.clone(),
                conversation_id: conversation_id.clone(),
            })
            .await?;
            let response = if response.status() == StatusCode::UNAUTHORIZED {
                invalidate_cached_access_token(&api_key).await;
                let access_token = get_cursor_access_token(&api_key, true).await?;
                send_cursor_request(&CursorRequestContext {
                    account,
                    access_token,
                    body: request_body,
                    conversation_id,
                })
                .await?
            } else {
                response
            };

            if !response.status().is_success() {
                let status = response.status().as_u16();
                let body = response_error_body(response).await;
                return Err(ProxyError::UpstreamError { status, body });
            }

            let is_stream = original_body
                .get("stream")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if is_stream {
                let stream = response_to_sse_stream(
                    response,
                    response_model,
                    mapped_body.clone(),
                    response_format,
                );
                Ok(ProxyResponse::local_sse(Box::pin(stream)))
            } else {
                let (_, bytes) =
                    response_to_json(response, &response_model, mapped_body, response_format)
                        .await?;
                Ok(ProxyResponse::local_json(StatusCode::OK, bytes))
            }
        }
    }
}

async fn derive_session_key(
    headers: Option<&HeaderMap>,
    body: &Value,
    session_manager: &CursorSessionManager,
    tool_results: &[ToolResultBlock],
    previous_response_id: Option<&str>,
) -> String {
    if let Some(s) = conversation_id_from_headers(headers) {
        return s;
    }
    for tr in tool_results {
        if let Some(s) = session_manager.resolve_tool_call_id(&tr.tool_call_id).await {
            return s;
        }
    }
    if let Some(prev) = previous_response_id {
        if !prev.is_empty() {
            if let Some(s) = session_manager.resolve_response_id(prev).await {
                return s;
            }
            return stable_uuid_like(prev);
        }
    }
    if let Some(meta) = body.get("metadata").and_then(Value::as_object) {
        if let Some(id) = meta.get("conversation_id").and_then(Value::as_str) {
            if !id.is_empty() {
                return stable_uuid_like(id);
            }
        }
    }
    uuid::Uuid::new_v4().to_string()
}

pub(crate) fn cursor_api_key_from_provider(provider: &Provider) -> Result<String, ProxyError> {
    if let Some(key) = provider
        .settings_config
        .pointer("/env/ANTHROPIC_AUTH_TOKEN")
        .and_then(|v| v.as_str())
        .or_else(|| {
            provider
                .settings_config
                .pointer("/env/ANTHROPIC_API_KEY")
                .and_then(|v| v.as_str())
        })
        .or_else(|| {
            provider
                .settings_config
                .pointer("/env/CURSOR_API_KEY")
                .and_then(|v| v.as_str())
        })
        .map(str::trim)
        .filter(|key| !key.is_empty())
    {
        return Ok(key.to_string());
    }

    let auth = provider.settings_config.get("auth");
    let config_text = provider
        .settings_config
        .get("config")
        .and_then(|v| v.as_str());
    if let Some(key) = crate::codex_config::extract_codex_api_key(auth, config_text)
        .map(|key| key.trim().to_string())
        .filter(|key| !key.is_empty())
    {
        return Ok(key);
    }

    Err(ProxyError::AuthError(
        "Cursor API Key 未配置，请填写 Cursor API Key".to_string(),
    ))
}

pub(crate) async fn get_cursor_access_token(
    api_key: &str,
    force_refresh: bool,
) -> Result<String, ProxyError> {
    let cache_key = access_token_cache_key(api_key);
    if !force_refresh {
        let cache = ACCESS_TOKEN_CACHE.lock().await;
        if let Some(entry) = cache.get(&cache_key) {
            if entry.is_valid() {
                return Ok(entry.token.clone());
            }
        }
    }
    let token = exchange_cursor_api_key(api_key).await?;
    let mut cache = ACCESS_TOKEN_CACHE.lock().await;
    cache.insert(
        cache_key,
        CachedAccessToken {
            token: token.clone(),
            expires_at_ms: chrono::Utc::now().timestamp_millis() + ACCESS_TOKEN_CACHE_TTL_MS,
        },
    );
    Ok(token)
}

async fn invalidate_cached_access_token(api_key: &str) {
    let mut cache = ACCESS_TOKEN_CACHE.lock().await;
    cache.remove(&access_token_cache_key(api_key));
}

async fn exchange_cursor_api_key(api_key: &str) -> Result<String, ProxyError> {
    let url = format!(
        "{}{}",
        cursor_backend_base_url(),
        EXCHANGE_USER_API_KEY_PATH
    );
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| ProxyError::ForwardFailed(format!("构造 exchange client 失败: {e}")))?;
    let resp = client
        .post(&url)
        .header("authorization", format!("Bearer {api_key}"))
        .header("content-type", "application/json")
        .json(&json!({}))
        .send()
        .await
        .map_err(|e| ProxyError::ForwardFailed(format!("exchange_user_api_key 请求失败: {e}")))?;
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        return Err(ProxyError::UpstreamError {
            status,
            body: Some(body),
        });
    }
    let parsed: CursorApiKeyExchangeResponse = resp
        .json()
        .await
        .map_err(|e| ProxyError::ForwardFailed(format!("解析 exchange 响应失败: {e}")))?;
    parsed
        .access_token
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| ProxyError::AuthError("Cursor exchange 响应缺少 access_token".to_string()))
}

fn cursor_backend_base_url() -> String {
    std::env::var("CC_SWITCH_CURSOR_BACKEND_BASE_URL")
        .ok()
        .or_else(|| std::env::var("CURSOR_BACKEND_BASE_URL").ok())
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_CURSOR_BACKEND_BASE_URL.to_string())
}

fn access_token_cache_key(api_key: &str) -> String {
    sha256_hex(api_key)
}

pub(crate) fn account_for_api_key(api_key: &str) -> CursorAccountData {
    let hash = sha256_hex(api_key);
    CursorAccountData {
        account_id: format!("cursor_apikey_{}", &hash[..24]),
        email: None,
        refresh_token: String::new(),
        id_token: None,
        cursor_service_machine_id: Some(hash.clone()),
        cursor_client_version: Some(DEFAULT_CURSOR_CLIENT_VERSION.to_string()),
        cursor_config_version: Some(stable_uuid_like(&format!("cursor-config:{hash}"))),
        cursor_client_id: None,
        authenticated_at: chrono::Utc::now().timestamp(),
    }
}

pub(crate) fn account_id_for_api_key(api_key: &str) -> String {
    let hash = sha256_hex(api_key);
    format!("cursor_apikey_{}", &hash[..24])
}

fn normalize_cursor_body(body: &Value) -> Value {
    let mut next = body.clone();
    if next
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        next["stream"] = json!(true);
    }
    next
}

fn prepare_cursor_apikey_claude_body(provider: &Provider, body: &Value) -> (Value, String, String) {
    let response_model = requested_model(body);
    let (mapped_body, original_model, mapped_model) =
        crate::proxy::model_mapper::apply_model_mapping(body.clone(), provider);
    if let (Some(original), Some(mapped)) = (original_model.as_deref(), mapped_model.as_deref()) {
        log::debug!("[CursorApiKey] Claude 模型映射: {original} -> {mapped}");
    }
    let mapped_body =
        crate::proxy::model_mapper::strip_one_m_suffix_for_upstream_from_body(mapped_body);
    let upstream_model = requested_model(&mapped_body);
    (mapped_body, response_model, upstream_model)
}

fn sha256_hex(input: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(api_key: &str) -> Provider {
        Provider::with_id(
            "test".to_string(),
            "test".to_string(),
            json!({ "env": { "CURSOR_API_KEY": api_key } }),
            None,
        )
    }

    fn provider_with_config(settings_config: Value) -> Provider {
        Provider::with_id(
            "cursor-apikey-test".to_string(),
            "Cursor API Key".to_string(),
            settings_config,
            Some("https://cursor.com".to_string()),
        )
    }

    #[test]
    fn account_id_is_stable() {
        let p1 = account_id_for_api_key("sk_abc");
        let p2 = account_id_for_api_key("sk_abc");
        assert_eq!(p1, p2);
        assert!(p1.starts_with("cursor_apikey_"));
    }

    #[test]
    fn api_key_from_settings() {
        let p = provider("sk_test_123");
        assert_eq!(cursor_api_key_from_provider(&p).unwrap(), "sk_test_123");
    }

    #[test]
    fn extracts_claude_cursor_api_key() {
        let provider = provider_with_config(json!({
            "env": {
                "ANTHROPIC_AUTH_TOKEN": " cursor_key "
            }
        }));
        assert_eq!(
            cursor_api_key_from_provider(&provider).unwrap(),
            "cursor_key"
        );
    }

    #[test]
    fn extracts_codex_cursor_api_key() {
        let provider = provider_with_config(json!({
            "auth": {
                "OPENAI_API_KEY": "cursor_key"
            },
            "config": "model = \"composer-2.5\""
        }));
        assert_eq!(
            cursor_api_key_from_provider(&provider).unwrap(),
            "cursor_key"
        );
    }

    #[test]
    fn access_token_cache_key_does_not_store_raw_key() {
        let raw = "cursor_key";
        let key = access_token_cache_key(raw);
        assert_ne!(key, raw);
        assert_eq!(key.len(), 64);
        assert_eq!(key, access_token_cache_key(raw));
    }
}
