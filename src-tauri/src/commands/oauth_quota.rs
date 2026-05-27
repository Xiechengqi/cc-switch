use std::sync::Arc;

use tauri::State;

use crate::commands::{
    AntigravityOAuthState, ClaudeOAuthState, CodexOAuthState, CopilotAuthState, CursorOAuthState,
    GeminiOAuthState, KiroOAuthState,
};
use crate::services::oauth_quota::{
    resolve_account_id_for_auth_provider, CachedOauthQuota, OauthQuotaManagers, OauthQuotaService,
};

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
    let cached = state
        .0
        .force_refresh(None, &managers, &auth_provider, &resolved_account_id)
        .await?;
    Ok(Some(cached))
}
