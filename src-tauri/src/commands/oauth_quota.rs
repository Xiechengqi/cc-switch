use std::sync::Arc;

use tauri::State;

use crate::commands::{ClaudeOAuthState, CodexOAuthState, CopilotAuthState};
use crate::services::oauth_quota::{
    resolve_account_id_for_auth_provider, CachedOauthQuota, OauthQuotaManagers, OauthQuotaService,
};

pub struct OauthQuotaState(pub Arc<OauthQuotaService>);

#[tauri::command(rename_all = "camelCase")]
pub async fn get_cached_oauth_quota(
    auth_provider: String,
    account_id: Option<String>,
    state: State<'_, OauthQuotaState>,
    codex_state: State<'_, CodexOAuthState>,
    claude_state: State<'_, ClaudeOAuthState>,
    copilot_state: State<'_, CopilotAuthState>,
) -> Result<Option<CachedOauthQuota>, String> {
    let managers = OauthQuotaManagers::from_states(&codex_state, &claude_state, &copilot_state);
    let Some(resolved_account_id) =
        resolve_account_id_for_auth_provider(&auth_provider, account_id, &managers).await
    else {
        return Ok(None);
    };
    Ok(state.0.get(&auth_provider, &resolved_account_id).await)
}
