//! Control-plane endpoint: the cc-switch-router server calls this over the
//! reverse tunnel to apply share-settings changes synchronously.
//!
//! The client stays authoritative: it applies the patch to its own local
//! config (via [`crate::tunnel::sync::apply_share_settings_patch`]) and reports
//! back the resulting [`ShareTunnelMetadata`] descriptor. The server only
//! persists what we return, after verifying it satisfies the patch.
//!
//! Auth is an HMAC-SHA256 over `METHOD\nPATH\n<body>\n<timestamp_ms>\n<nonce>`
//! using the per-installation `control_secret` issued at registration. Requests
//! arriving here are always from the tunnel (public `/_ctl/*` traffic is
//! rejected at the router edge), but we still authenticate every call so a
//! compromised co-tenant on the tunnel cannot drive our config.

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use hmac::{Hmac, Mac};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::Sha256;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Mutex;
use tauri::Manager;

use super::server::ProxyState;
use crate::app_config::AppType;
use crate::commands::{
    AntigravityOAuthState, ClaudeOAuthState, CodexOAuthState, CopilotAuthState, CursorOAuthState,
    GeminiOAuthState, KiroOAuthState, OauthQuotaState,
};
use crate::provider::Provider;
use crate::services::oauth_quota::{resolve_account_id_for_auth_provider, OauthQuotaManagers};
use crate::store::AppState;
use crate::tunnel::sync::{
    apply_share_settings_patch, share_metadata_from_record, ShareSettingsPatch,
};

type HmacSha256 = Hmac<Sha256>;

const APPLY_SHARE_SETTINGS_PATH: &str = "/_ctl/apply_share_settings";
const REFRESH_SHARE_USAGE_PATH: &str = "/_ctl/refresh_share_usage";
/// Reject requests whose timestamp is outside this window (replay / clock skew).
const MAX_SKEW_MS: i64 = 5 * 60 * 1000;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApplyShareSettingsBody {
    share_id: String,
    patch: ShareSettingsPatch,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RefreshShareUsageBody {
    share_id: String,
    app: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RefreshShareUsageItem {
    app: String,
    provider_id: Option<String>,
    provider_name: Option<String>,
    auth_provider: Option<String>,
    account_id: Option<String>,
    refreshed: bool,
    error: Option<String>,
}

/// Remembers recently-seen nonces so a captured request cannot be replayed
/// inside the skew window. Entries are pruned by timestamp on each insert, so
/// the map stays bounded by the request rate over `MAX_SKEW_MS`.
static SEEN_NONCES: Lazy<Mutex<HashMap<String, i64>>> = Lazy::new(|| Mutex::new(HashMap::new()));

fn err(status: StatusCode, code: &str) -> Response {
    (status, Json(json!({ "ok": false, "error": code }))).into_response()
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

fn expected_signature(
    path: &str,
    secret: &str,
    body: &[u8],
    timestamp_ms: &str,
    nonce: &str,
) -> Vec<u8> {
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts keys of any size");
    mac.update(b"POST\n");
    mac.update(path.as_bytes());
    mac.update(b"\n");
    mac.update(body);
    mac.update(b"\n");
    mac.update(timestamp_ms.as_bytes());
    mac.update(b"\n");
    mac.update(nonce.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

/// Records a nonce as used, pruning stale entries. Returns false if the nonce
/// was already seen within the window (replay).
fn register_nonce(nonce: &str, now_ms: i64) -> bool {
    let mut seen = match SEEN_NONCES.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    seen.retain(|_, ts| (now_ms - *ts).abs() <= MAX_SKEW_MS);
    if seen.contains_key(nonce) {
        return false;
    }
    seen.insert(nonce.to_string(), now_ms);
    true
}

fn verify_control_request(path: &str, headers: &HeaderMap, body: &[u8]) -> Result<(), Response> {
    let Some(secret) = crate::tunnel::identity::load_control_secret() else {
        return Err(err(StatusCode::UNAUTHORIZED, "control_secret_unavailable"));
    };
    let (Some(timestamp_raw), Some(nonce), Some(signature_b64)) = (
        header(&headers, "x-ctl-timestamp-ms"),
        header(&headers, "x-ctl-nonce"),
        header(&headers, "x-ctl-signature"),
    ) else {
        return Err(err(StatusCode::UNAUTHORIZED, "missing_control_headers"));
    };
    let Ok(timestamp_ms) = timestamp_raw.parse::<i64>() else {
        return Err(err(StatusCode::UNAUTHORIZED, "bad_timestamp"));
    };
    let now_ms = chrono::Utc::now().timestamp_millis();
    if (now_ms - timestamp_ms).abs() > MAX_SKEW_MS {
        return Err(err(StatusCode::UNAUTHORIZED, "stale_timestamp"));
    }

    let Ok(provided_sig) =
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, signature_b64)
    else {
        return Err(err(StatusCode::UNAUTHORIZED, "bad_signature"));
    };
    let expected = expected_signature(path, &secret, body, timestamp_raw, nonce);
    // ct_eq via constant-time comparison: hmac's MacResult is not exposed here,
    // so compare the raw bytes with a length-checked constant-time fold.
    if provided_sig.len() != expected.len()
        || provided_sig
            .iter()
            .zip(expected.iter())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            != 0
    {
        return Err(err(StatusCode::UNAUTHORIZED, "bad_signature"));
    }
    if !register_nonce(nonce, now_ms) {
        return Err(err(StatusCode::UNAUTHORIZED, "replay"));
    }
    Ok(())
}

pub async fn apply_share_settings(
    State(state): State<ProxyState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(response) = verify_control_request(APPLY_SHARE_SETTINGS_PATH, &headers, &body) {
        return response;
    }

    let parsed: ApplyShareSettingsBody = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "ok": false, "error": format!("bad_body: {e}") })),
            )
                .into_response();
        }
    };

    if let Err(e) = apply_share_settings_patch(&state.db, &parsed.share_id, parsed.patch) {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "ok": false, "error": format!("apply_failed: {e}") })),
        )
            .into_response();
    }

    let updated = match state.db.get_share_by_id(&parsed.share_id) {
        Ok(Some(share)) => share,
        Ok(None) => return err(StatusCode::NOT_FOUND, "share_not_found"),
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "ok": false, "error": format!("read_failed: {e}") })),
            )
                .into_response();
        }
    };

    let descriptor = share_metadata_from_record(&updated);
    (
        StatusCode::OK,
        Json(json!({ "ok": true, "share": descriptor })),
    )
        .into_response()
}

pub async fn refresh_share_usage(
    State(state): State<ProxyState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(response) = verify_control_request(REFRESH_SHARE_USAGE_PATH, &headers, &body) {
        return response;
    }
    let parsed: RefreshShareUsageBody = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "ok": false, "error": format!("bad_body: {e}") })),
            )
                .into_response();
        }
    };
    match refresh_share_usage_inner(&state, parsed).await {
        Ok(items) => (
            StatusCode::OK,
            Json(json!({ "ok": true, "refreshed": items })),
        )
            .into_response(),
        Err((status, message)) => {
            (status, Json(json!({ "ok": false, "error": message }))).into_response()
        }
    }
}

async fn refresh_share_usage_inner(
    state: &ProxyState,
    body: RefreshShareUsageBody,
) -> Result<Vec<RefreshShareUsageItem>, (StatusCode, String)> {
    let app_handle = state.app_handle.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "app_handle_unavailable".to_string(),
        )
    })?;
    let app_state = app_handle.try_state::<AppState>().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "app_state_unavailable".to_string(),
        )
    })?;
    let quota_state = app_handle.try_state::<OauthQuotaState>().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "oauth_quota_state_unavailable".to_string(),
        )
    })?;
    let codex = app_handle.try_state::<CodexOAuthState>().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "codex_oauth_state_unavailable".to_string(),
        )
    })?;
    let claude = app_handle.try_state::<ClaudeOAuthState>().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "claude_oauth_state_unavailable".to_string(),
        )
    })?;
    let gemini = app_handle.try_state::<GeminiOAuthState>().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "gemini_oauth_state_unavailable".to_string(),
        )
    })?;
    let copilot = app_handle.try_state::<CopilotAuthState>().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "copilot_auth_state_unavailable".to_string(),
        )
    })?;
    let kiro = app_handle.try_state::<KiroOAuthState>().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "kiro_oauth_state_unavailable".to_string(),
        )
    })?;
    let antigravity = app_handle
        .try_state::<AntigravityOAuthState>()
        .ok_or_else(|| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "antigravity_oauth_state_unavailable".to_string(),
            )
        })?;
    let cursor = app_handle.try_state::<CursorOAuthState>().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "cursor_oauth_state_unavailable".to_string(),
        )
    })?;
    let managers = OauthQuotaManagers::from_states(
        &codex,
        &claude,
        &gemini,
        &copilot,
        &kiro,
        &antigravity,
        &cursor,
    );
    let share = app_state
        .db
        .get_share_by_id(&body.share_id)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("read_share_failed: {e}"),
            )
        })?
        .ok_or_else(|| (StatusCode::NOT_FOUND, "share_not_found".to_string()))?;

    let apps = if let Some(app) = body
        .app
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        vec![app.to_ascii_lowercase()]
    } else {
        let mut apps: Vec<String> = share
            .bindings
            .keys()
            .map(|app| app.to_ascii_lowercase())
            .collect();
        apps.sort();
        apps.dedup();
        apps
    };

    let mut items = Vec::new();
    for app_name in apps {
        let app_type = match AppType::from_str(&app_name) {
            Ok(value @ (AppType::Claude | AppType::Codex | AppType::Gemini)) => value,
            _ => {
                items.push(RefreshShareUsageItem {
                    app: app_name,
                    provider_id: None,
                    provider_name: None,
                    auth_provider: None,
                    account_id: None,
                    refreshed: false,
                    error: Some("unsupported_app".to_string()),
                });
                continue;
            }
        };
        let Some(provider_id) = share.bindings.get(app_type.as_str()).cloned() else {
            items.push(RefreshShareUsageItem {
                app: app_type.as_str().to_string(),
                provider_id: None,
                provider_name: None,
                auth_provider: None,
                account_id: None,
                refreshed: false,
                error: Some("share_app_not_bound".to_string()),
            });
            continue;
        };
        let provider = match app_state
            .db
            .get_provider_by_id(&provider_id, app_type.as_str())
        {
            Ok(Some(provider)) => provider,
            Ok(None) => {
                items.push(RefreshShareUsageItem {
                    app: app_type.as_str().to_string(),
                    provider_id: Some(provider_id),
                    provider_name: None,
                    auth_provider: None,
                    account_id: None,
                    refreshed: false,
                    error: Some("provider_not_found".to_string()),
                });
                continue;
            }
            Err(err) => {
                items.push(RefreshShareUsageItem {
                    app: app_type.as_str().to_string(),
                    provider_id: Some(provider_id),
                    provider_name: None,
                    auth_provider: None,
                    account_id: None,
                    refreshed: false,
                    error: Some(format!("read_provider_failed: {err}")),
                });
                continue;
            }
        };
        let provider_name = provider.name.clone();
        let Some(auth_provider) = quota_auth_provider(&app_type, &provider) else {
            items.push(RefreshShareUsageItem {
                app: app_type.as_str().to_string(),
                provider_id: Some(provider_id),
                provider_name: Some(provider_name),
                auth_provider: None,
                account_id: None,
                refreshed: false,
                error: Some("provider_has_no_refreshable_quota".to_string()),
            });
            continue;
        };
        let cursor_api_key = if auth_provider == "cursor_apikey" {
            match crate::proxy::providers::cursor_apikey::cursor_api_key_from_provider(&provider) {
                Ok(api_key) => Some(api_key),
                Err(err) => {
                    items.push(RefreshShareUsageItem {
                        app: app_type.as_str().to_string(),
                        provider_id: Some(provider_id),
                        provider_name: Some(provider_name),
                        auth_provider: Some(auth_provider),
                        account_id: None,
                        refreshed: false,
                        error: Some(format!("cursor_apikey_missing: {err}")),
                    });
                    continue;
                }
            }
        } else {
            None
        };
        let account_id = if let Some(api_key) = cursor_api_key.as_deref() {
            Some(crate::proxy::providers::cursor_apikey::account_id_for_api_key(api_key))
        } else {
            let account_id = provider
                .meta
                .as_ref()
                .and_then(|meta| meta.managed_account_id_for(&auth_provider))
                .filter(|id| !id.trim().is_empty());
            resolve_account_id_for_auth_provider(&auth_provider, account_id, &managers).await
        };
        let Some(account_id) = account_id else {
            items.push(RefreshShareUsageItem {
                app: app_type.as_str().to_string(),
                provider_id: Some(provider_id),
                provider_name: Some(provider_name),
                auth_provider: Some(auth_provider),
                account_id: None,
                refreshed: false,
                error: Some("account_not_found".to_string()),
            });
            continue;
        };
        let provider_type = provider
            .meta
            .as_ref()
            .and_then(|meta| meta.provider_type.as_deref());
        let result = quota_state
            .0
            .force_refresh(
                Some(app_handle),
                &managers,
                &auth_provider,
                &account_id,
                provider_type,
                cursor_api_key,
            )
            .await;
        match result {
            Ok(_) => {
                crate::tunnel::sync::schedule_share_runtime_refresh_after_provider_switch(
                    app_state.db.clone(),
                    app_type.clone(),
                );
                items.push(RefreshShareUsageItem {
                    app: app_type.as_str().to_string(),
                    provider_id: Some(provider_id),
                    provider_name: Some(provider_name),
                    auth_provider: Some(auth_provider),
                    account_id: Some(account_id),
                    refreshed: true,
                    error: None,
                });
            }
            Err(err) => items.push(RefreshShareUsageItem {
                app: app_type.as_str().to_string(),
                provider_id: Some(provider_id),
                provider_name: Some(provider_name),
                auth_provider: Some(auth_provider),
                account_id: Some(account_id),
                refreshed: false,
                error: Some(err),
            }),
        }
    }
    Ok(items)
}

fn quota_auth_provider(app_type: &AppType, provider: &Provider) -> Option<String> {
    let provider_type = provider
        .meta
        .as_ref()
        .and_then(|meta| meta.provider_type.as_deref());
    if matches!(app_type, AppType::Claude) && provider_type == Some("claude_oauth") {
        return Some("claude_oauth".to_string());
    }
    if matches!(app_type, AppType::Claude) && provider_type == Some("kiro_oauth") {
        return Some("kiro_oauth".to_string());
    }
    if matches!(app_type, AppType::Claude)
        && (provider_type == Some("github_copilot")
            || provider
                .meta
                .as_ref()
                .and_then(|meta| meta.usage_script.as_ref())
                .and_then(|script| script.template_type.as_deref())
                == Some("github_copilot"))
    {
        return Some("github_copilot".to_string());
    }
    if matches!(app_type, AppType::Codex)
        && (provider_type == Some("codex_oauth") || provider.is_codex_official_with_managed_auth())
    {
        return Some("codex_oauth".to_string());
    }
    if matches!(app_type, AppType::Gemini)
        && (provider_type == Some("google_gemini_oauth")
            || provider.is_google_gemini_official_with_managed_auth())
    {
        return Some("google_gemini_oauth".to_string());
    }
    if matches!(app_type, AppType::Claude | AppType::Gemini)
        && matches!(provider_type, Some("antigravity_oauth" | "agy_oauth"))
    {
        return Some("antigravity_oauth".to_string());
    }
    if matches!(app_type, AppType::Claude | AppType::Codex) && provider_type == Some("cursor_oauth")
    {
        return Some("cursor_oauth".to_string());
    }
    if matches!(app_type, AppType::Claude | AppType::Codex)
        && provider_type == Some("cursor_apikey")
    {
        return Some("cursor_apikey".to_string());
    }
    None
}
