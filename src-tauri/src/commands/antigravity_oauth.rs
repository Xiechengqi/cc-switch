//! Antigravity OAuth state wrapper.

use crate::proxy::providers::antigravity_oauth_auth::AntigravityOAuthManager;
use crate::services::model_fetch::FetchedModel;
use std::sync::Arc;
use tauri::State;
use tokio::sync::RwLock;

/// Antigravity OAuth 认证状态
pub struct AntigravityOAuthState(pub Arc<RwLock<AntigravityOAuthManager>>);

/// 获取 Antigravity OAuth 可用模型列表。
///
/// 未指定账号或账号不可用时仍返回内置免费模型清单；有账号时追加服务端
/// `fetchAvailableModels` 下发的动态模型。
#[tauri::command(rename_all = "camelCase")]
pub async fn get_antigravity_oauth_models(
    account_id: Option<String>,
    state: State<'_, AntigravityOAuthState>,
) -> Result<Vec<FetchedModel>, String> {
    let manager = state.0.read().await;
    let resolved = match account_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        Some(id) => Some(id.to_string()),
        None => manager.default_account_id().await,
    };

    let Some(id) = resolved else {
        return Ok(crate::services::antigravity_models::static_antigravity_models());
    };

    let token = match manager.get_valid_token_for_account(&id).await {
        Ok(token) => token,
        Err(err) => {
            log::warn!("Antigravity OAuth token unavailable, using static catalog: {err}");
            return Ok(crate::services::antigravity_models::static_antigravity_models());
        }
    };
    let project_id = manager.project_id_for_account(&id).await.ok();
    drop(manager);

    match crate::services::antigravity_models::fetch_antigravity_available_models(
        &token,
        project_id.as_deref(),
    )
    .await
    {
        Ok(dynamic) => {
            Ok(crate::services::antigravity_models::merge_static_and_dynamic_models(dynamic))
        }
        Err(err) => {
            log::warn!("Antigravity model discovery failed, using static catalog: {err}");
            Ok(crate::services::antigravity_models::static_antigravity_models())
        }
    }
}
