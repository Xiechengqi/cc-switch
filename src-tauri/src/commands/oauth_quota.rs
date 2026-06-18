use std::sync::Arc;

use tauri::State;

use crate::app_config::AppType;
use crate::commands::{
    AntigravityOAuthState, ClaudeOAuthState, CodexOAuthState, CopilotAuthState, CursorOAuthState,
    GeminiOAuthState, KiroOAuthState,
};
use crate::services::oauth_quota::{
    resolve_account_id_for_auth_provider, CachedOauthQuota, OauthQuotaManagers, OauthQuotaService,
};
use crate::store::AppState;

pub struct OauthQuotaState(pub Arc<OauthQuotaService>);

#[tauri::command(rename_all = "camelCase")]
#[allow(clippy::too_many_arguments)]
pub async fn get_cached_oauth_quota(
    auth_provider: String,
    account_id: Option<String>,
    app_type: Option<String>,
    provider_id: Option<String>,
    state: State<'_, OauthQuotaState>,
    app_state: State<'_, AppState>,
    codex_state: State<'_, CodexOAuthState>,
    claude_state: State<'_, ClaudeOAuthState>,
    gemini_state: State<'_, GeminiOAuthState>,
    copilot_state: State<'_, CopilotAuthState>,
    kiro_state: State<'_, KiroOAuthState>,
    antigravity_state: State<'_, AntigravityOAuthState>,
    cursor_state: State<'_, CursorOAuthState>,
) -> Result<Option<CachedOauthQuota>, String> {
    let managers = OauthQuotaManagers::from_states(
        &codex_state,
        &claude_state,
        &gemini_state,
        &copilot_state,
        &kiro_state,
        &antigravity_state,
        &cursor_state,
    );
    let Some(resolved_account_id) = resolve_quota_account_id(
        &app_state,
        &auth_provider,
        account_id,
        app_type.as_deref(),
        provider_id.as_deref(),
        &managers,
    )
    .await
    else {
        return Ok(None);
    };
    Ok(state.0.get(&auth_provider, &resolved_account_id).await)
}

#[tauri::command(rename_all = "camelCase")]
#[allow(clippy::too_many_arguments)]
pub async fn refresh_oauth_quota(
    app_handle: tauri::AppHandle,
    auth_provider: String,
    account_id: Option<String>,
    provider_type: Option<String>,
    app_type: Option<String>,
    provider_id: Option<String>,
    app_state: State<'_, AppState>,
    state: State<'_, OauthQuotaState>,
    codex_state: State<'_, CodexOAuthState>,
    claude_state: State<'_, ClaudeOAuthState>,
    gemini_state: State<'_, GeminiOAuthState>,
    copilot_state: State<'_, CopilotAuthState>,
    kiro_state: State<'_, KiroOAuthState>,
    antigravity_state: State<'_, AntigravityOAuthState>,
    cursor_state: State<'_, CursorOAuthState>,
) -> Result<Option<CachedOauthQuota>, String> {
    let managers = OauthQuotaManagers::from_states(
        &codex_state,
        &claude_state,
        &gemini_state,
        &copilot_state,
        &kiro_state,
        &antigravity_state,
        &cursor_state,
    );
    let cursor_api_key = resolve_cursor_apikey_for_quota(
        &app_state,
        &auth_provider,
        app_type.as_deref(),
        provider_id.as_deref(),
    )?;
    let Some(resolved_account_id) = resolve_quota_account_id(
        &app_state,
        &auth_provider,
        account_id,
        app_type.as_deref(),
        provider_id.as_deref(),
        &managers,
    )
    .await
    else {
        return Ok(None);
    };
    let cached = state
        .0
        .force_refresh(
            Some(&app_handle),
            &managers,
            &auth_provider,
            &resolved_account_id,
            provider_type.as_deref(),
            cursor_api_key,
        )
        .await?;
    for app_type in share_runtime_apps_for_auth_provider(&auth_provider) {
        crate::tunnel::sync::schedule_share_runtime_refresh_after_provider_switch(
            app_state.db.clone(),
            app_type,
        );
    }
    Ok(Some(cached))
}

async fn resolve_quota_account_id(
    app_state: &AppState,
    auth_provider: &str,
    account_id: Option<String>,
    app_type: Option<&str>,
    provider_id: Option<&str>,
    managers: &OauthQuotaManagers,
) -> Option<String> {
    if auth_provider == "cursor_apikey" {
        let api_key =
            resolve_cursor_apikey_for_quota(app_state, auth_provider, app_type, provider_id)
                .ok()??;
        return Some(crate::proxy::providers::cursor_apikey::account_id_for_api_key(&api_key));
    }
    resolve_account_id_for_auth_provider(auth_provider, account_id, managers).await
}

fn resolve_cursor_apikey_for_quota(
    app_state: &AppState,
    auth_provider: &str,
    app_type: Option<&str>,
    provider_id: Option<&str>,
) -> Result<Option<String>, String> {
    if auth_provider != "cursor_apikey" {
        return Ok(None);
    }
    let Some(app_type) = app_type else {
        return Ok(None);
    };
    let Some(provider_id) = provider_id else {
        return Ok(None);
    };
    let provider = app_state
        .db
        .get_provider_by_id(provider_id, app_type)
        .map_err(|e| format!("Failed to get provider: {e}"))?
        .ok_or_else(|| format!("Provider not found: {provider_id}"))?;
    crate::proxy::providers::cursor_apikey::cursor_api_key_from_provider(&provider)
        .map(Some)
        .map_err(|e| e.to_string())
}

fn share_runtime_apps_for_auth_provider(auth_provider: &str) -> Vec<AppType> {
    match auth_provider {
        "claude_oauth" | "github_copilot" | "kiro_oauth" => vec![AppType::Claude],
        "codex_oauth" => vec![AppType::Codex],
        "google_gemini_oauth" => vec![AppType::Gemini],
        "antigravity_oauth" => vec![AppType::Claude, AppType::Gemini],
        "cursor_oauth" | "cursor_apikey" => vec![AppType::Claude, AppType::Codex],
        _ => Vec::new(),
    }
}
