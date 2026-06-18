//! Cursor API key provider.
//!
//! This follows the composer-api transport shape: a user Cursor API key is
//! exchanged for Cursor's internal access token, then the existing private
//! Connect-RPC Composer protocol is used for Claude/Codex-compatible requests.

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

use super::cursor_oauth_auth::{CursorAccountData, DEFAULT_CURSOR_CLIENT_VERSION};
use super::cursor_protocol::{
    requested_model, response_to_json, response_to_sse_stream, send_cursor_request,
    CursorRequestContext, CursorResponseFormat,
};

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
) -> Result<ProxyResponse, ProxyError> {
    forward_cursor_apikey(
        provider,
        headers,
        body,
        CursorResponseFormat::AnthropicMessages,
    )
    .await
}

pub async fn forward_cursor_apikey_codex(
    provider: &Provider,
    headers: Option<&HeaderMap>,
    endpoint: &str,
    body: &Value,
) -> Result<ProxyResponse, ProxyError> {
    let format = if endpoint.contains("/chat/completions") {
        CursorResponseFormat::OpenAiChatCompletions
    } else {
        CursorResponseFormat::OpenAiResponses
    };
    forward_cursor_apikey(provider, headers, body, format).await
}

async fn forward_cursor_apikey(
    provider: &Provider,
    headers: Option<&HeaderMap>,
    body: &Value,
    response_format: CursorResponseFormat,
) -> Result<ProxyResponse, ProxyError> {
    let api_key = cursor_api_key_from_provider(provider)?;
    let access_token = get_cursor_access_token(&api_key, false).await?;
    let account = account_for_api_key(&api_key);

    let request_body = normalize_cursor_body(body);
    let conversation_id = conversation_id_from_headers(headers);
    let response = send_cursor_request(&CursorRequestContext {
        account: account.clone(),
        access_token: access_token.clone(),
        body: request_body.clone(),
        conversation_id: conversation_id.clone(),
    })
    .await?;
    let response = if response.status() == reqwest::StatusCode::UNAUTHORIZED {
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
        let body = response.text().await.ok();
        return Err(ProxyError::UpstreamError { status, body });
    }

    let model = requested_model(body);
    let is_stream = body
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if is_stream {
        let stream = response_to_sse_stream(response, model, response_format);
        Ok(ProxyResponse::local_sse(Box::pin(stream)))
    } else {
        let (_, bytes) = response_to_json(response, &model, response_format).await?;
        Ok(ProxyResponse::local_json(StatusCode::OK, bytes))
    }
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
    let key_hash = sha256_hex(api_key);
    if !force_refresh {
        if let Some(cached) = ACCESS_TOKEN_CACHE.lock().await.get(&key_hash).cloned() {
            if cached.is_valid() {
                return Ok(cached.token);
            }
        }
    }

    let token = exchange_cursor_api_key(api_key).await?;
    ACCESS_TOKEN_CACHE.lock().await.insert(
        key_hash,
        CachedAccessToken {
            token: token.clone(),
            expires_at_ms: chrono::Utc::now().timestamp_millis() + ACCESS_TOKEN_CACHE_TTL_MS,
        },
    );
    Ok(token)
}

async fn invalidate_cached_access_token(api_key: &str) {
    let key_hash = sha256_hex(api_key);
    ACCESS_TOKEN_CACHE.lock().await.remove(&key_hash);
}

async fn exchange_cursor_api_key(api_key: &str) -> Result<String, ProxyError> {
    let base_url = cursor_backend_base_url();
    let url = format!(
        "{}{}",
        base_url.trim_end_matches('/'),
        EXCHANGE_USER_API_KEY_PATH
    );
    let client = reqwest::Client::builder()
        .http2_adaptive_window(true)
        .build()
        .map_err(|e| ProxyError::ForwardFailed(format!("创建 Cursor HTTP client 失败: {e}")))?;
    let response = client
        .post(url)
        .bearer_auth(api_key)
        .header("Content-Type", "application/json")
        .json(&json!({}))
        .send()
        .await
        .map_err(|e| ProxyError::ForwardFailed(format!("Cursor API Key 交换失败: {e}")))?;

    if !response.status().is_success() {
        let status = response.status().as_u16();
        let body = response.text().await.ok();
        return Err(ProxyError::UpstreamError { status, body });
    }

    let payload = response
        .json::<CursorApiKeyExchangeResponse>()
        .await
        .map_err(|e| ProxyError::ForwardFailed(format!("解析 Cursor API Key 交换响应失败: {e}")))?;
    payload
        .access_token
        .filter(|token| !token.trim().is_empty())
        .ok_or_else(|| {
            ProxyError::ForwardFailed("Cursor API Key 交换响应缺少 accessToken".to_string())
        })
}

fn cursor_backend_base_url() -> String {
    std::env::var("CC_SWITCH_CURSOR_BACKEND_BASE_URL")
        .ok()
        .or_else(|| std::env::var("CURSOR_BACKEND_BASE_URL").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_CURSOR_BACKEND_BASE_URL.to_string())
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

fn sha256_hex(input: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    hex::encode(hasher.finalize())
}

fn conversation_id_from_headers(headers: Option<&HeaderMap>) -> Option<String> {
    let headers = headers?;
    let key = [
        "x-session-affinity",
        "x-opencode-session-id",
        "x-opencode-session",
    ]
    .iter()
    .find_map(|name| {
        headers
            .get(*name)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|v| !v.is_empty())
    })?;
    Some(stable_uuid_like(key))
}

fn stable_uuid_like(input: &str) -> String {
    let hash = sha256_hex(input);
    format!(
        "{}-{}-{}-{}-{}",
        &hash[0..8],
        &hash[8..12],
        &hash[12..16],
        &hash[16..20],
        &hash[20..32]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(settings_config: Value) -> Provider {
        Provider::with_id(
            "cursor-apikey-test".to_string(),
            "Cursor API Key".to_string(),
            settings_config,
            Some("https://cursor.com".to_string()),
        )
    }

    #[test]
    fn extracts_claude_cursor_api_key() {
        let provider = provider(json!({
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
        let provider = provider(json!({
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
    fn stable_uuid_like_is_deterministic() {
        let first = stable_uuid_like("session-a");
        let second = stable_uuid_like("session-a");
        assert_eq!(first, second);
        assert_eq!(first.len(), 36);
    }

    #[test]
    fn account_for_api_key_uses_stable_identity() {
        let first = account_for_api_key("cursor_key");
        let second = account_for_api_key("cursor_key");
        assert_eq!(
            first.cursor_service_machine_id,
            second.cursor_service_machine_id
        );
        assert_eq!(first.cursor_config_version, second.cursor_config_version);
    }
}
