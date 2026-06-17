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
    let Some(resolved_account_id) =
        resolve_account_id_for_auth_provider(&auth_provider, account_id, &managers).await
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
    let Some(resolved_account_id) =
        resolve_account_id_for_auth_provider(&auth_provider, account_id, &managers).await
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

fn share_runtime_apps_for_auth_provider(auth_provider: &str) -> Vec<AppType> {
    match auth_provider {
        "claude_oauth" | "github_copilot" | "kiro_oauth" => vec![AppType::Claude],
        "codex_oauth" => vec![AppType::Codex],
        "google_gemini_oauth" => vec![AppType::Gemini],
        "antigravity_oauth" => vec![AppType::Claude, AppType::Gemini],
        "cursor_oauth" => vec![AppType::Claude, AppType::Codex],
        _ => Vec::new(),
    }
}
