//! 流式健康检查命令

use crate::app_config::AppType;
use crate::commands::claude_oauth::ClaudeOAuthState;
use crate::commands::codex_oauth::CodexOAuthState;
use crate::commands::copilot::CopilotAuthState;
use crate::commands::gemini_oauth::GeminiOAuthState;
use crate::error::AppError;
use crate::services::stream_check::{
    HealthStatus, StreamCheckConfig, StreamCheckResult, StreamCheckService,
};
use crate::store::AppState;
use std::collections::HashSet;
use tauri::State;

/// 流式健康检查（单个供应商）
#[tauri::command]
pub async fn stream_check_provider(
    state: State<'_, AppState>,
    copilot_state: State<'_, CopilotAuthState>,
    codex_oauth_state: State<'_, CodexOAuthState>,
    claude_oauth_state: State<'_, ClaudeOAuthState>,
    gemini_oauth_state: State<'_, GeminiOAuthState>,
    app_type: AppType,
    provider_id: String,
) -> Result<StreamCheckResult, AppError> {
    let config = state.db.get_stream_check_config()?;

    let providers = state.db.get_all_providers(app_type.as_str())?;
    let provider = providers
        .get(&provider_id)
        .ok_or_else(|| AppError::Message(format!("供应商 {provider_id} 不存在")))?;

    let auth_override = resolve_copilot_auth_override(provider, &copilot_state)
        .await?
        .or(resolve_codex_oauth_auth_override(&app_type, provider, &codex_oauth_state).await?)
        .or(resolve_claude_oauth_auth_override(provider, &claude_oauth_state).await?)
        .or(resolve_gemini_oauth_auth_override(&app_type, provider, &gemini_oauth_state).await?);
    let base_url_override = resolve_codex_oauth_base_url_override(&app_type, provider)
        .or(resolve_copilot_base_url_override(provider, &copilot_state).await?)
        .or(resolve_claude_oauth_base_url_override(provider))
        .or(resolve_gemini_oauth_base_url_override(&app_type, provider));
    let claude_api_format_override = resolve_claude_api_format_override(
        &app_type,
        provider,
        &config,
        &copilot_state,
        auth_override.as_ref(),
    )
    .await?;
    let result = StreamCheckService::check_with_retry(
        &app_type,
        provider,
        &config,
        auth_override,
        base_url_override,
        claude_api_format_override,
    )
    .await?;

    // 记录日志
    let _ =
        state
            .db
            .save_stream_check_log(&provider_id, &provider.name, app_type.as_str(), &result);

    Ok(result)
}

/// 批量流式健康检查
#[tauri::command]
pub async fn stream_check_all_providers(
    state: State<'_, AppState>,
    copilot_state: State<'_, CopilotAuthState>,
    codex_oauth_state: State<'_, CodexOAuthState>,
    claude_oauth_state: State<'_, ClaudeOAuthState>,
    gemini_oauth_state: State<'_, GeminiOAuthState>,
    app_type: AppType,
    proxy_targets_only: bool,
) -> Result<Vec<(String, StreamCheckResult)>, AppError> {
    let config = state.db.get_stream_check_config()?;
    let providers = state.db.get_all_providers(app_type.as_str())?;

    let mut results = Vec::new();
    let allowed_ids: Option<HashSet<String>> = if proxy_targets_only {
        let mut ids = HashSet::new();
        if let Ok(Some(current_id)) = state.db.get_current_provider(app_type.as_str()) {
            ids.insert(current_id);
        }
        if let Ok(queue) = state.db.get_failover_queue(app_type.as_str()) {
            for item in queue {
                ids.insert(item.provider_id);
            }
        }
        Some(ids)
    } else {
        None
    };

    for (id, provider) in providers {
        if let Some(ids) = &allowed_ids {
            if !ids.contains(&id) {
                continue;
            }
        }

        let auth_override = resolve_copilot_auth_override(&provider, &copilot_state)
            .await?
            .or(resolve_codex_oauth_auth_override(&app_type, &provider, &codex_oauth_state).await?)
            .or(resolve_claude_oauth_auth_override(&provider, &claude_oauth_state).await?)
            .or(
                resolve_gemini_oauth_auth_override(&app_type, &provider, &gemini_oauth_state)
                    .await?,
            );
        let base_url_override = resolve_codex_oauth_base_url_override(&app_type, &provider)
            .or(resolve_copilot_base_url_override(&provider, &copilot_state).await?)
            .or(resolve_claude_oauth_base_url_override(&provider))
            .or(resolve_gemini_oauth_base_url_override(&app_type, &provider));
        let claude_api_format_override = resolve_claude_api_format_override(
            &app_type,
            &provider,
            &config,
            &copilot_state,
            auth_override.as_ref(),
        )
        .await
        .unwrap_or_else(|e| {
            log::warn!(
                "[StreamCheck] Failed to resolve Claude API format override for {}: {}",
                provider.id,
                e
            );
            None
        });
        let result = StreamCheckService::check_with_retry(
            &app_type,
            &provider,
            &config,
            auth_override,
            base_url_override,
            claude_api_format_override,
        )
        .await
        .unwrap_or_else(|e| {
            let (http_status, message) = match &e {
                crate::error::AppError::HttpStatus { status, .. } => (
                    Some(*status),
                    StreamCheckService::classify_http_status(*status).to_string(),
                ),
                _ => (None, e.to_string()),
            };
            StreamCheckResult {
                status: HealthStatus::Failed,
                success: false,
                message,
                response_time_ms: None,
                http_status,
                model_used: String::new(),
                tested_at: chrono::Utc::now().timestamp(),
                retry_count: 0,
                error_category: None,
            }
        });

        let _ = state
            .db
            .save_stream_check_log(&id, &provider.name, app_type.as_str(), &result);

        results.push((id, result));
    }

    Ok(results)
}

/// 获取流式检查配置
#[tauri::command]
pub fn get_stream_check_config(state: State<'_, AppState>) -> Result<StreamCheckConfig, AppError> {
    state.db.get_stream_check_config()
}

/// 保存流式检查配置
#[tauri::command]
pub fn save_stream_check_config(
    state: State<'_, AppState>,
    config: StreamCheckConfig,
) -> Result<(), AppError> {
    state.db.save_stream_check_config(&config)
}

async fn resolve_copilot_auth_override(
    provider: &crate::provider::Provider,
    copilot_state: &State<'_, CopilotAuthState>,
) -> Result<Option<crate::proxy::providers::AuthInfo>, AppError> {
    let is_copilot = is_copilot_provider(provider);

    if !is_copilot {
        return Ok(None);
    }

    let auth_manager = copilot_state.0.read().await;
    let account_id = provider
        .meta
        .as_ref()
        .and_then(|meta| meta.managed_account_id_for("github_copilot"));

    let token = match account_id.as_deref() {
        Some(id) => auth_manager
            .get_valid_token_for_account(id)
            .await
            .map_err(|e| AppError::Message(format!("GitHub Copilot 认证失败: {e}")))?,
        None => auth_manager
            .get_valid_token()
            .await
            .map_err(|e| AppError::Message(format!("GitHub Copilot 认证失败: {e}")))?,
    };

    Ok(Some(crate::proxy::providers::AuthInfo::new(
        token,
        crate::proxy::providers::AuthStrategy::GitHubCopilot,
    )))
}

async fn resolve_copilot_base_url_override(
    provider: &crate::provider::Provider,
    copilot_state: &State<'_, CopilotAuthState>,
) -> Result<Option<String>, AppError> {
    let is_copilot = is_copilot_provider(provider);
    let is_full_url = provider
        .meta
        .as_ref()
        .and_then(|meta| meta.is_full_url)
        .unwrap_or(false);

    if !is_copilot || is_full_url {
        return Ok(None);
    }

    let auth_manager = copilot_state.0.read().await;
    let account_id = provider
        .meta
        .as_ref()
        .and_then(|meta| meta.managed_account_id_for("github_copilot"));

    let endpoint = match account_id.as_deref() {
        Some(id) => auth_manager.get_api_endpoint(id).await,
        None => auth_manager.get_default_api_endpoint().await,
    };

    Ok(Some(endpoint))
}

fn is_copilot_provider(provider: &crate::provider::Provider) -> bool {
    provider
        .meta
        .as_ref()
        .and_then(|meta| meta.provider_type.as_deref())
        == Some("github_copilot")
        || provider
            .settings_config
            .pointer("/env/ANTHROPIC_BASE_URL")
            .and_then(|value| value.as_str())
            .map(|url| url.contains("githubcopilot.com"))
            .unwrap_or(false)
}

async fn resolve_codex_oauth_auth_override(
    app_type: &AppType,
    provider: &crate::provider::Provider,
    codex_oauth_state: &State<'_, CodexOAuthState>,
) -> Result<Option<crate::proxy::providers::AuthInfo>, AppError> {
    if !uses_codex_oauth_auth(app_type, provider) {
        return Ok(None);
    }

    let auth_manager = codex_oauth_state.0.read().await;
    let account_id = provider
        .meta
        .as_ref()
        .and_then(|meta| meta.managed_account_id_for("codex_oauth"));

    let (token, resolved_account_id) = match account_id.as_deref() {
        Some(id) => (
            auth_manager
                .get_valid_token_for_account(id)
                .await
                .map_err(|e| AppError::Message(format!("Codex OAuth 认证失败: {e}")))?,
            Some(id.to_string()),
        ),
        None => {
            let token = auth_manager
                .get_valid_token()
                .await
                .map_err(|e| AppError::Message(format!("Codex OAuth 认证失败: {e}")))?;
            (token, auth_manager.default_account_id().await)
        }
    };

    Ok(Some(
        crate::proxy::providers::AuthInfo::new(
            token,
            crate::proxy::providers::AuthStrategy::CodexOAuth,
        )
        .with_managed_account_id(resolved_account_id),
    ))
}

fn uses_codex_oauth_auth(app_type: &AppType, provider: &crate::provider::Provider) -> bool {
    match app_type {
        AppType::Codex => provider.is_codex_official_with_managed_auth(),
        AppType::Claude => provider.is_codex_oauth_provider(),
        _ => false,
    }
}

fn resolve_codex_oauth_base_url_override(
    app_type: &AppType,
    provider: &crate::provider::Provider,
) -> Option<String> {
    provider
        .stream_check_base_url_override(app_type)
        .map(str::to_string)
}

async fn resolve_claude_oauth_auth_override(
    provider: &crate::provider::Provider,
    claude_oauth_state: &State<'_, ClaudeOAuthState>,
) -> Result<Option<crate::proxy::providers::AuthInfo>, AppError> {
    if !provider.is_claude_oauth_provider() {
        return Ok(None);
    }

    let auth_manager = claude_oauth_state.0.read().await;
    let account_id = provider
        .meta
        .as_ref()
        .and_then(|meta| meta.managed_account_id_for("claude_oauth"));

    let token = match account_id.as_deref() {
        Some(id) => auth_manager
            .get_valid_token_for_account(id)
            .await
            .map_err(|e| AppError::Message(format!("Claude OAuth 认证失败: {e}")))?,
        None => auth_manager
            .get_valid_token()
            .await
            .map_err(|e| AppError::Message(format!("Claude OAuth 认证失败: {e}")))?,
    };

    Ok(Some(crate::proxy::providers::AuthInfo::new(
        token,
        crate::proxy::providers::AuthStrategy::ClaudeOAuth,
    )))
}

#[cfg(test)]
fn is_claude_oauth_provider(provider: &crate::provider::Provider) -> bool {
    provider.is_claude_oauth_provider()
}

fn resolve_claude_oauth_base_url_override(provider: &crate::provider::Provider) -> Option<String> {
    provider
        .stream_check_base_url_override(&AppType::Claude)
        .map(str::to_string)
}

async fn resolve_gemini_oauth_auth_override(
    app_type: &AppType,
    provider: &crate::provider::Provider,
    gemini_oauth_state: &State<'_, GeminiOAuthState>,
) -> Result<Option<crate::proxy::providers::AuthInfo>, AppError> {
    if !matches!(app_type, AppType::Gemini)
        || !(provider.is_google_gemini_oauth_provider()
            || provider.is_google_gemini_official_with_managed_auth())
    {
        return Ok(None);
    }

    let auth_manager = gemini_oauth_state.0.read().await;
    let account_id = provider
        .meta
        .as_ref()
        .and_then(|meta| meta.managed_account_id_for("google_gemini_oauth"));

    let (token, resolved_account_id) = match account_id.as_deref() {
        Some(id) => (
            auth_manager
                .get_valid_token_for_account(id)
                .await
                .map_err(|e| AppError::Message(format!("Google Gemini OAuth 认证失败: {e}")))?,
            Some(id.to_string()),
        ),
        None => {
            let resolved = auth_manager.default_account_id().await;
            let Some(id) = resolved.clone() else {
                return Err(AppError::Message(
                    "Google Gemini OAuth 认证失败: 未找到可用账号".to_string(),
                ));
            };
            (
                auth_manager
                    .get_valid_token_for_account(&id)
                    .await
                    .map_err(|e| AppError::Message(format!("Google Gemini OAuth 认证失败: {e}")))?,
                Some(id),
            )
        }
    };

    Ok(Some(
        crate::proxy::providers::AuthInfo::with_access_token(token.clone(), token)
            .with_managed_account_id(resolved_account_id),
    ))
}

fn resolve_gemini_oauth_base_url_override(
    app_type: &AppType,
    provider: &crate::provider::Provider,
) -> Option<String> {
    if !matches!(app_type, AppType::Gemini) {
        return None;
    }

    provider
        .stream_check_base_url_override(app_type)
        .map(str::to_string)
}

async fn resolve_claude_api_format_override(
    app_type: &AppType,
    provider: &crate::provider::Provider,
    config: &StreamCheckConfig,
    copilot_state: &State<'_, CopilotAuthState>,
    auth_override: Option<&crate::proxy::providers::AuthInfo>,
) -> Result<Option<String>, AppError> {
    if *app_type != AppType::Claude {
        return Ok(None);
    }

    let is_copilot = auth_override
        .map(|auth| auth.strategy == crate::proxy::providers::AuthStrategy::GitHubCopilot)
        .unwrap_or(false);
    if !is_copilot {
        return Ok(None);
    }

    let model_id = StreamCheckService::resolve_effective_test_model(app_type, provider, config);
    let auth_manager = copilot_state.0.read().await;
    let account_id = provider
        .meta
        .as_ref()
        .and_then(|meta| meta.managed_account_id_for("github_copilot"));

    let vendor_result = match account_id.as_deref() {
        Some(id) => {
            auth_manager
                .get_model_vendor_for_account(id, &model_id)
                .await
        }
        None => auth_manager.get_model_vendor(&model_id).await,
    };

    let api_format = match vendor_result {
        Ok(Some(vendor)) if vendor.eq_ignore_ascii_case("openai") => "openai_responses",
        Ok(Some(_)) | Ok(None) => "openai_chat",
        Err(err) => {
            log::warn!(
                "[StreamCheck] Failed to resolve Copilot model vendor for {model_id}: {err}. Falling back to chat/completions"
            );
            "openai_chat"
        }
    };

    Ok(Some(api_format.to_string()))
}

#[cfg(test)]
mod tests {
    use super::{
        is_claude_oauth_provider, is_copilot_provider, resolve_claude_oauth_base_url_override,
        resolve_codex_oauth_base_url_override, uses_codex_oauth_auth,
    };
    use crate::app_config::AppType;
    use crate::provider::{AuthBinding, AuthBindingSource, Provider, ProviderMeta};
    use serde_json::json;

    #[test]
    fn copilot_provider_detection_accepts_provider_type_or_base_url() {
        let typed_provider = Provider {
            id: "p1".to_string(),
            name: "typed".to_string(),
            settings_config: json!({}),
            website_url: None,
            category: None,
            created_at: None,
            sort_index: None,
            notes: None,
            meta: Some(ProviderMeta {
                provider_type: Some("github_copilot".to_string()),
                ..Default::default()
            }),
            icon: None,
            icon_color: None,
            in_failover_queue: false,
        };
        assert!(is_copilot_provider(&typed_provider));

        let url_provider = Provider {
            id: "p2".to_string(),
            name: "url".to_string(),
            settings_config: json!({
                "env": {
                    "ANTHROPIC_BASE_URL": "https://api.githubcopilot.com"
                }
            }),
            website_url: None,
            category: None,
            created_at: None,
            sort_index: None,
            notes: None,
            meta: None,
            icon: None,
            icon_color: None,
            in_failover_queue: false,
        };
        assert!(is_copilot_provider(&url_provider));
    }

    #[test]
    fn copilot_full_url_metadata_is_available_for_override_guard() {
        let provider = Provider {
            id: "p3".to_string(),
            name: "relay".to_string(),
            settings_config: json!({}),
            website_url: None,
            category: None,
            created_at: None,
            sort_index: None,
            notes: None,
            meta: Some(ProviderMeta {
                provider_type: Some("github_copilot".to_string()),
                is_full_url: Some(true),
                ..Default::default()
            }),
            icon: None,
            icon_color: None,
            in_failover_queue: false,
        };

        assert!(is_copilot_provider(&provider));
        assert_eq!(
            provider.meta.as_ref().and_then(|meta| meta.is_full_url),
            Some(true)
        );
    }

    #[test]
    fn claude_oauth_base_url_override_uses_official_endpoint() {
        let provider = Provider {
            id: "p4".to_string(),
            name: "Claude OAuth".to_string(),
            settings_config: json!({
                "env": {
                    "ANTHROPIC_BASE_URL": "http://127.0.0.1:21852"
                }
            }),
            website_url: None,
            category: Some("official".to_string()),
            created_at: None,
            sort_index: None,
            notes: None,
            meta: Some(ProviderMeta {
                provider_type: Some("claude_oauth".to_string()),
                ..Default::default()
            }),
            icon: None,
            icon_color: None,
            in_failover_queue: false,
        };

        assert!(is_claude_oauth_provider(&provider));
        assert_eq!(
            resolve_claude_oauth_base_url_override(&provider).as_deref(),
            Some("https://api.anthropic.com")
        );
    }

    #[test]
    fn codex_official_managed_auth_base_url_override_uses_chatgpt_codex_endpoint() {
        let provider = Provider {
            id: "p5".to_string(),
            name: "OpenAI Official".to_string(),
            settings_config: json!({}),
            website_url: None,
            category: Some("official".to_string()),
            created_at: None,
            sort_index: None,
            notes: None,
            meta: Some(ProviderMeta {
                auth_binding: Some(AuthBinding {
                    source: AuthBindingSource::ManagedAccount,
                    auth_provider: Some("codex_oauth".to_string()),
                    account_id: Some("acct-1".to_string()),
                }),
                ..Default::default()
            }),
            icon: None,
            icon_color: None,
            in_failover_queue: false,
        };

        assert_eq!(
            resolve_codex_oauth_base_url_override(&AppType::Codex, &provider).as_deref(),
            Some("https://chatgpt.com/backend-api/codex")
        );
    }

    #[test]
    fn claude_codex_oauth_provider_uses_codex_oauth_auth_override() {
        let provider = Provider {
            id: "p6".to_string(),
            name: "Codex OAuth".to_string(),
            settings_config: json!({}),
            website_url: None,
            category: Some("official".to_string()),
            created_at: None,
            sort_index: None,
            notes: None,
            meta: Some(ProviderMeta {
                provider_type: Some("codex_oauth".to_string()),
                auth_binding: Some(AuthBinding {
                    source: AuthBindingSource::ManagedAccount,
                    auth_provider: Some("codex_oauth".to_string()),
                    account_id: Some("acct-1".to_string()),
                }),
                ..Default::default()
            }),
            icon: None,
            icon_color: None,
            in_failover_queue: false,
        };

        assert!(uses_codex_oauth_auth(&AppType::Claude, &provider));
    }
}
