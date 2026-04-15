//! Claude OAuth Tauri Commands
//!
//! 提供 Anthropic Claude 官方订阅 OAuth 认证相关的 Tauri 命令。
//!
//! 认证命令通过通用 `auth_*` 命令（参见 `commands::auth`）暴露给前端，
//! 此处定义 State wrapper 以及 Claude OAuth 专属的命令。

use crate::proxy::providers::claude_oauth_auth::ClaudeOAuthManager;
use crate::proxy::providers::claude_oauth_auth::ClaudeOAuthStatus;
use crate::services::subscription::{CredentialStatus, SubscriptionQuota};
use std::sync::Arc;
use tauri::State;
use tokio::sync::RwLock;

/// Claude OAuth 认证状态
pub struct ClaudeOAuthState(pub Arc<RwLock<ClaudeOAuthManager>>);

/// 获取 Claude OAuth 认证状态
///
/// 注意：启动登录流程和轮询结果均通过通用 auth_* 命令完成，
/// 此处仅保留 Claude OAuth 专属命令（状态查询、额度查询）。
#[tauri::command(rename_all = "camelCase")]
pub async fn claude_oauth_get_status(
    state: State<'_, ClaudeOAuthState>,
) -> Result<ClaudeOAuthStatus, String> {
    let manager = state.0.read().await;
    Ok(manager.get_status().await)
}

/// 查询 Claude OAuth 订阅额度
#[tauri::command(rename_all = "camelCase")]
pub async fn get_claude_oauth_quota(
    account_id: Option<String>,
    state: State<'_, ClaudeOAuthState>,
) -> Result<SubscriptionQuota, String> {
    let manager = state.0.read().await;

    let resolved = match account_id {
        Some(id) => Some(id),
        None => manager.default_account_id().await,
    };
    let Some(_id) = resolved else {
        return Ok(SubscriptionQuota::not_found("claude_oauth"));
    };

    // 获取（必要时自动刷新）access_token
    let token = match manager.get_valid_token_for_account(&_id).await {
        Ok(t) => t,
        Err(e) => {
            return Ok(SubscriptionQuota::error(
                "claude_oauth",
                CredentialStatus::Expired,
                format!("Claude OAuth token unavailable: {e}"),
            ));
        }
    };

    // 复用现有的 Claude 订阅额度查询
    Ok(crate::services::subscription::query_claude_quota_with_token(&token, "claude_oauth").await)
}
