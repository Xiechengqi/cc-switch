//! 真实模型测试命令

use crate::app_config::AppType;
use crate::commands::antigravity_oauth::AntigravityOAuthState;
use crate::commands::claude_oauth::ClaudeOAuthState;
use crate::commands::codex_oauth::CodexOAuthState;
use crate::commands::copilot::CopilotAuthState;
use crate::commands::deepseek_account::DeepSeekAccountState;
use crate::commands::gemini_oauth::GeminiOAuthState;
use crate::error::AppError;
use crate::proxy::providers::{AuthInfo, AuthStrategy};
use crate::services::model_test::{
    HealthStatus, ModelTestService, StreamCheckConfig, StreamCheckResult,
};
use crate::store::AppState;
use futures::StreamExt;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::time::Instant;
use tauri::{AppHandle, Manager, State};

/// 真实模型测试（单个供应商）
#[tauri::command]
pub async fn model_test_provider(
    app_handle: AppHandle,
    state: State<'_, AppState>,
    copilot_state: State<'_, CopilotAuthState>,
    codex_oauth_state: State<'_, CodexOAuthState>,
    claude_oauth_state: State<'_, ClaudeOAuthState>,
    gemini_oauth_state: State<'_, GeminiOAuthState>,
    antigravity_oauth_state: State<'_, AntigravityOAuthState>,
    deepseek_account_state: State<'_, DeepSeekAccountState>,
    app_type: AppType,
    provider_id: String,
) -> Result<StreamCheckResult, AppError> {
    let config: StreamCheckConfig = state.db.get_stream_check_config()?.into();

    let providers = state.db.get_all_providers(app_type.as_str())?;
    let provider = providers
        .get(&provider_id)
        .ok_or_else(|| AppError::Message(format!("供应商 {provider_id} 不存在")))?;

    if matches!(app_type, AppType::Claude) && provider.is_deepseek_account_provider() {
        let result =
            check_deepseek_account_provider(provider, &config, deepseek_account_state.0.clone())
                .await;
        let _ = state.db.save_stream_check_log(
            &provider_id,
            &provider.name,
            app_type.as_str(),
            &result,
        );
        return Ok(result);
    }

    if matches!(app_type, AppType::Claude) && provider.is_kiro_oauth_provider() {
        let result = check_kiro_oauth_provider(&app_handle, provider, &config).await;
        let _ = state.db.save_stream_check_log(
            &provider_id,
            &provider.name,
            app_type.as_str(),
            &result,
        );
        return Ok(result);
    }

    if matches!(app_type, AppType::Claude | AppType::Codex)
        && (provider.is_cursor_oauth_provider() || provider.is_cursor_apikey_provider())
    {
        let result = check_cursor_oauth_provider(&app_handle, &app_type, provider, &config).await;
        let _ = state.db.save_stream_check_log(
            &provider_id,
            &provider.name,
            app_type.as_str(),
            &result,
        );
        return Ok(result);
    }

    if matches!(app_type, AppType::Claude | AppType::Gemini)
        && provider.is_antigravity_family_provider()
    {
        let result = check_antigravity_oauth_provider(
            &app_type,
            provider,
            &config,
            antigravity_oauth_state.0.clone(),
        )
        .await;
        let _ = state.db.save_stream_check_log(
            &provider_id,
            &provider.name,
            app_type.as_str(),
            &result,
        );
        return Ok(result);
    }

    let result = run_standard_stream_check_with_managed_auth_retry(
        &app_type,
        provider,
        &config,
        &copilot_state,
        &codex_oauth_state,
        &claude_oauth_state,
        &gemini_oauth_state,
    )
    .await?;

    // 记录日志
    let _ =
        state
            .db
            .save_stream_check_log(&provider_id, &provider.name, app_type.as_str(), &result);
    if result.success {
        let _ = state
            .db
            .reset_provider_health(&provider_id, app_type.as_str())
            .await;
    }

    Ok(result)
}

/// Run the same provider stream check used by the UI "Test model" button from
/// internal HTTP surfaces such as the share-router model health probe.
pub(crate) async fn run_model_test_for_provider(
    db: &crate::database::Database,
    app_handle: Option<&tauri::AppHandle>,
    app_type: &AppType,
    provider: &crate::provider::Provider,
) -> Result<StreamCheckResult, AppError> {
    let config: StreamCheckConfig = db.get_stream_check_config()?.into();

    let Some(app_handle) = app_handle else {
        return ModelTestService::check_with_retry(app_type, provider, &config, None, None, None)
            .await;
    };

    let copilot_state = app_handle.state::<CopilotAuthState>();
    let codex_oauth_state = app_handle.state::<CodexOAuthState>();
    let claude_oauth_state = app_handle.state::<ClaudeOAuthState>();
    let gemini_oauth_state = app_handle.state::<GeminiOAuthState>();
    let antigravity_oauth_state = app_handle.state::<AntigravityOAuthState>();
    let deepseek_account_state = app_handle.state::<DeepSeekAccountState>();

    if matches!(app_type, AppType::Claude) && provider.is_deepseek_account_provider() {
        return Ok(check_deepseek_account_provider(
            provider,
            &config,
            deepseek_account_state.0.clone(),
        )
        .await);
    }

    if matches!(app_type, AppType::Claude) && provider.is_kiro_oauth_provider() {
        return Ok(check_kiro_oauth_provider(app_handle, provider, &config).await);
    }

    if matches!(app_type, AppType::Claude | AppType::Codex)
        && (provider.is_cursor_oauth_provider() || provider.is_cursor_apikey_provider())
    {
        return Ok(check_cursor_oauth_provider(app_handle, app_type, provider, &config).await);
    }

    if matches!(app_type, AppType::Claude | AppType::Gemini)
        && provider.is_antigravity_family_provider()
    {
        return Ok(check_antigravity_oauth_provider(
            app_type,
            provider,
            &config,
            antigravity_oauth_state.0.clone(),
        )
        .await);
    }

    let result = run_standard_stream_check_with_managed_auth_retry(
        app_type,
        provider,
        &config,
        &copilot_state,
        &codex_oauth_state,
        &claude_oauth_state,
        &gemini_oauth_state,
    )
    .await?;

    let _ = db.save_stream_check_log(&provider.id, &provider.name, app_type.as_str(), &result);
    if result.success {
        let _ = db
            .reset_provider_health(&provider.id, app_type.as_str())
            .await;
    }

    Ok(result)
}

/// 批量真实模型测试
#[tauri::command]
pub async fn model_test_all_providers(
    app_handle: AppHandle,
    state: State<'_, AppState>,
    copilot_state: State<'_, CopilotAuthState>,
    codex_oauth_state: State<'_, CodexOAuthState>,
    claude_oauth_state: State<'_, ClaudeOAuthState>,
    gemini_oauth_state: State<'_, GeminiOAuthState>,
    antigravity_oauth_state: State<'_, AntigravityOAuthState>,
    deepseek_account_state: State<'_, DeepSeekAccountState>,
    app_type: AppType,
    proxy_targets_only: bool,
) -> Result<Vec<(String, StreamCheckResult)>, AppError> {
    let config: StreamCheckConfig = state.db.get_stream_check_config()?.into();
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

        if matches!(app_type, AppType::Claude) && provider.is_deepseek_account_provider() {
            let result = check_deepseek_account_provider(
                &provider,
                &config,
                deepseek_account_state.0.clone(),
            )
            .await;
            let _ = state
                .db
                .save_stream_check_log(&id, &provider.name, app_type.as_str(), &result);
            results.push((id, result));
            continue;
        }

        if matches!(app_type, AppType::Claude) && provider.is_kiro_oauth_provider() {
            let result = check_kiro_oauth_provider(&app_handle, &provider, &config).await;
            let _ = state
                .db
                .save_stream_check_log(&id, &provider.name, app_type.as_str(), &result);
            results.push((id, result));
            continue;
        }

        if matches!(app_type, AppType::Claude | AppType::Codex)
            && (provider.is_cursor_oauth_provider() || provider.is_cursor_apikey_provider())
        {
            let result =
                check_cursor_oauth_provider(&app_handle, &app_type, &provider, &config).await;
            let _ = state
                .db
                .save_stream_check_log(&id, &provider.name, app_type.as_str(), &result);
            results.push((id, result));
            continue;
        }

        if matches!(app_type, AppType::Claude | AppType::Gemini)
            && provider.is_antigravity_family_provider()
        {
            let result = check_antigravity_oauth_provider(
                &app_type,
                &provider,
                &config,
                antigravity_oauth_state.0.clone(),
            )
            .await;
            let _ = state
                .db
                .save_stream_check_log(&id, &provider.name, app_type.as_str(), &result);
            results.push((id, result));
            continue;
        }

        let result = run_standard_stream_check_with_managed_auth_retry(
            &app_type,
            &provider,
            &config,
            &copilot_state,
            &codex_oauth_state,
            &claude_oauth_state,
            &gemini_oauth_state,
        )
        .await
        .unwrap_or_else(|e| {
            let (http_status, message) = match &e {
                crate::error::AppError::HttpStatus { status, .. } => (
                    Some(*status),
                    ModelTestService::classify_http_status(*status).to_string(),
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
                input_tokens: 0,
                output_tokens: 0,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
            }
        });
        let _ = state
            .db
            .save_stream_check_log(&id, &provider.name, app_type.as_str(), &result);

        results.push((id, result));
    }

    Ok(results)
}

async fn run_standard_stream_check_with_managed_auth_retry(
    app_type: &AppType,
    provider: &crate::provider::Provider,
    config: &StreamCheckConfig,
    copilot_state: &State<'_, CopilotAuthState>,
    codex_oauth_state: &State<'_, CodexOAuthState>,
    claude_oauth_state: &State<'_, ClaudeOAuthState>,
    gemini_oauth_state: &State<'_, GeminiOAuthState>,
) -> Result<StreamCheckResult, AppError> {
    let (auth_override, base_url_override, claude_api_format_override) =
        resolve_standard_stream_check_inputs(
            app_type,
            provider,
            config,
            copilot_state,
            codex_oauth_state,
            claude_oauth_state,
            gemini_oauth_state,
        )
        .await?;
    let retry_account = oauth_retry_account(provider, auth_override.as_ref());

    let result = ModelTestService::check_with_retry(
        app_type,
        provider,
        config,
        auth_override,
        base_url_override,
        claude_api_format_override,
    )
    .await?;

    if !result.success && result.http_status == Some(401) {
        if let Some((kind, account_id)) = retry_account {
            log::warn!(
                "[StreamCheck] OAuth upstream returned 401 for provider={} kind={kind:?} account={account_id}; invalidating cached access token and retrying once",
                provider.id,
            );
            match kind {
                StreamCheckOAuthKind::Codex => {
                    codex_oauth_state
                        .0
                        .read()
                        .await
                        .invalidate_cached_token(&account_id)
                        .await;
                }
            }

            let (auth_override, base_url_override, claude_api_format_override) =
                resolve_standard_stream_check_inputs(
                    app_type,
                    provider,
                    config,
                    copilot_state,
                    codex_oauth_state,
                    claude_oauth_state,
                    gemini_oauth_state,
                )
                .await?;

            let mut retry_result = ModelTestService::check_with_retry(
                app_type,
                provider,
                config,
                auth_override,
                base_url_override,
                claude_api_format_override,
            )
            .await?;
            retry_result.retry_count = retry_result
                .retry_count
                .saturating_add(result.retry_count)
                .saturating_add(1);
            return Ok(retry_result);
        }
    }

    Ok(result)
}

async fn resolve_standard_stream_check_inputs(
    app_type: &AppType,
    provider: &crate::provider::Provider,
    config: &StreamCheckConfig,
    copilot_state: &State<'_, CopilotAuthState>,
    codex_oauth_state: &State<'_, CodexOAuthState>,
    claude_oauth_state: &State<'_, ClaudeOAuthState>,
    gemini_oauth_state: &State<'_, GeminiOAuthState>,
) -> Result<(Option<AuthInfo>, Option<String>, Option<String>), AppError> {
    let auth_override = resolve_copilot_auth_override(provider, copilot_state)
        .await?
        .or(resolve_codex_oauth_auth_override(app_type, provider, codex_oauth_state).await?)
        .or(resolve_claude_oauth_auth_override(provider, claude_oauth_state).await?)
        .or(resolve_gemini_oauth_auth_override(app_type, provider, gemini_oauth_state).await?);
    let base_url_override = resolve_codex_oauth_base_url_override(app_type, provider)
        .or(resolve_copilot_base_url_override(provider, copilot_state).await?)
        .or(resolve_claude_oauth_base_url_override(provider))
        .or(resolve_gemini_oauth_base_url_override(app_type, provider));
    let claude_api_format_override = resolve_claude_api_format_override(
        app_type,
        provider,
        config,
        copilot_state,
        auth_override.as_ref(),
    )
    .await?;

    Ok((auth_override, base_url_override, claude_api_format_override))
}

#[derive(Debug, Clone, Copy)]
enum StreamCheckOAuthKind {
    Codex,
}

fn oauth_retry_account(
    provider: &crate::provider::Provider,
    auth_override: Option<&AuthInfo>,
) -> Option<(StreamCheckOAuthKind, String)> {
    let auth = auth_override?;
    if auth.strategy != AuthStrategy::CodexOAuth {
        return None;
    }

    if !(provider.is_codex_oauth_provider() || provider.is_codex_official_with_managed_auth()) {
        return None;
    }

    auth.managed_account_id
        .clone()
        .or_else(|| {
            provider
                .meta
                .as_ref()
                .and_then(|meta| meta.managed_account_id_for("codex_oauth"))
        })
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
        .map(|id| (StreamCheckOAuthKind::Codex, id))
}

async fn check_deepseek_account_provider(
    provider: &crate::provider::Provider,
    config: &StreamCheckConfig,
    manager: std::sync::Arc<
        tokio::sync::RwLock<crate::proxy::providers::deepseek_account_auth::DeepSeekAccountManager>,
    >,
) -> StreamCheckResult {
    let start = Instant::now();
    let model = extract_claude_env_model(provider).unwrap_or_else(|| config.claude_model.clone());
    let body = json!({
        "model": model,
        "max_tokens": 64,
        "stream": false,
        "messages": [{
            "role": "user",
            "content": config.test_prompt
        }]
    });

    let result = crate::proxy::providers::deepseek_claude::forward_deepseek_claude_with_manager(
        manager, provider, &body,
    )
    .await;
    let response_time = start.elapsed().as_millis() as u64;
    let tested_at = chrono::Utc::now().timestamp();

    match result {
        Ok(response) => {
            let status = response.status();
            let bytes = response.bytes().await;
            if status.is_success() {
                StreamCheckResult {
                    status: if response_time > config.degraded_threshold_ms {
                        HealthStatus::Degraded
                    } else {
                        HealthStatus::Operational
                    },
                    success: true,
                    message: "Check succeeded".to_string(),
                    response_time_ms: Some(response_time),
                    http_status: Some(status.as_u16()),
                    model_used: model,
                    tested_at,
                    retry_count: 0,
                    error_category: None,
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_read_tokens: 0,
                    cache_creation_tokens: 0,
                }
            } else {
                StreamCheckResult {
                    status: HealthStatus::Failed,
                    success: false,
                    message: bytes
                        .ok()
                        .and_then(|b| String::from_utf8(b.to_vec()).ok())
                        .unwrap_or_else(|| format!("HTTP {}", status.as_u16())),
                    response_time_ms: Some(response_time),
                    http_status: Some(status.as_u16()),
                    model_used: model,
                    tested_at,
                    retry_count: 0,
                    error_category: None,
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_read_tokens: 0,
                    cache_creation_tokens: 0,
                }
            }
        }
        Err(error) => StreamCheckResult {
            status: HealthStatus::Failed,
            success: false,
            message: error.to_string(),
            response_time_ms: Some(response_time),
            http_status: None,
            model_used: model,
            tested_at,
            retry_count: 0,
            error_category: None,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        },
    }
}

async fn check_antigravity_oauth_provider(
    app_type: &AppType,
    provider: &crate::provider::Provider,
    config: &StreamCheckConfig,
    manager: std::sync::Arc<
        tokio::sync::RwLock<
            crate::proxy::providers::antigravity_oauth_auth::AntigravityOAuthManager,
        >,
    >,
) -> StreamCheckResult {
    let effective_config = ModelTestService::merge_provider_config(provider, config);
    let mut last_result = None;

    for attempt in 0..=effective_config.max_retries {
        let result = check_antigravity_oauth_provider_once(
            app_type,
            provider,
            &effective_config,
            manager.clone(),
        )
        .await;
        if result.success || attempt >= effective_config.max_retries {
            return StreamCheckResult {
                retry_count: attempt,
                ..result
            };
        }
        last_result = Some(result);
    }

    last_result.unwrap_or_else(|| StreamCheckResult {
        status: HealthStatus::Failed,
        success: false,
        message: "Antigravity OAuth 检查失败".to_string(),
        response_time_ms: None,
        http_status: None,
        model_used: ModelTestService::resolve_effective_test_model(
            app_type,
            provider,
            &effective_config,
        ),
        tested_at: chrono::Utc::now().timestamp(),
        retry_count: effective_config.max_retries,
        error_category: None,
        input_tokens: 0,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
    })
}

async fn check_antigravity_oauth_provider_once(
    app_type: &AppType,
    provider: &crate::provider::Provider,
    config: &StreamCheckConfig,
    manager: std::sync::Arc<
        tokio::sync::RwLock<
            crate::proxy::providers::antigravity_oauth_auth::AntigravityOAuthManager,
        >,
    >,
) -> StreamCheckResult {
    let start = Instant::now();
    let model = resolve_mapped_antigravity_test_model(app_type, provider, config);
    let timeout = std::time::Duration::from_secs(config.timeout_secs);

    let probe = async {
        let (account_id, token, project_id) =
            resolve_antigravity_oauth_credentials(provider, manager.clone()).await?;
        match send_antigravity_oauth_stream_check(
            &model,
            &config.test_prompt,
            timeout,
            &token,
            &project_id,
            provider.antigravity_client_profile(),
        )
        .await
        {
            Err(AppError::HttpStatus { status: 401, .. }) => {
                manager
                    .read()
                    .await
                    .invalidate_cached_token(&account_id)
                    .await;
                let (_, token, project_id) =
                    resolve_antigravity_oauth_credentials(provider, manager.clone()).await?;
                send_antigravity_oauth_stream_check(
                    &model,
                    &config.test_prompt,
                    timeout,
                    &token,
                    &project_id,
                    provider.antigravity_client_profile(),
                )
                .await
            }
            other => other,
        }
    };

    let result = tokio::time::timeout(timeout, probe)
        .await
        .unwrap_or_else(|_| {
            Err(AppError::Message(format!(
                "Antigravity OAuth 检查超时: {} 秒",
                config.timeout_secs
            )))
        });

    let response_time = start.elapsed().as_millis() as u64;
    let tested_at = chrono::Utc::now().timestamp();

    match result {
        Ok(status_code) => StreamCheckResult {
            status: if response_time > config.degraded_threshold_ms {
                HealthStatus::Degraded
            } else {
                HealthStatus::Operational
            },
            success: true,
            message: "Check succeeded".to_string(),
            response_time_ms: Some(response_time),
            http_status: Some(status_code),
            model_used: model,
            tested_at,
            retry_count: 0,
            error_category: None,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        },
        Err(AppError::HttpStatus { status, body }) => StreamCheckResult {
            status: HealthStatus::Failed,
            success: false,
            message: if body.trim().is_empty() {
                format!("Antigravity OAuth 检查出错: HTTP {status}")
            } else {
                format!("Antigravity OAuth 检查出错: HTTP {status}: {}", body.trim())
            },
            response_time_ms: Some(response_time),
            http_status: Some(status),
            model_used: model,
            tested_at,
            retry_count: 0,
            error_category: ModelTestService::detect_error_category(status, &body)
                .map(str::to_string),
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        },
        Err(error) => StreamCheckResult {
            status: HealthStatus::Failed,
            success: false,
            message: format!("Antigravity OAuth 检查出错: {error}"),
            response_time_ms: Some(response_time),
            http_status: None,
            model_used: model,
            tested_at,
            retry_count: 0,
            error_category: None,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        },
    }
}

fn resolve_mapped_antigravity_test_model(
    app_type: &AppType,
    provider: &crate::provider::Provider,
    config: &StreamCheckConfig,
) -> String {
    let model = ModelTestService::resolve_effective_test_model(app_type, provider, config);
    let (mapped_body, _, mapped_model) = crate::proxy::model_mapper::apply_model_mapping(
        json!({ "model": model.clone() }),
        provider,
    );

    mapped_model
        .or_else(|| {
            mapped_body
                .get("model")
                .and_then(|value| value.as_str())
                .map(ToString::to_string)
        })
        .unwrap_or(model)
}

async fn resolve_antigravity_oauth_credentials(
    provider: &crate::provider::Provider,
    manager: std::sync::Arc<
        tokio::sync::RwLock<
            crate::proxy::providers::antigravity_oauth_auth::AntigravityOAuthManager,
        >,
    >,
) -> Result<(String, String, String), AppError> {
    let auth_manager = manager.read().await;
    let account_id = provider
        .meta
        .as_ref()
        .and_then(|meta| meta.managed_account_id_for("antigravity_oauth"));
    let resolved_account_id = match account_id {
        Some(id) => id,
        None => auth_manager.default_account_id().await.ok_or_else(|| {
            AppError::Message("Antigravity OAuth 认证失败: 未找到可用账号".to_string())
        })?,
    };

    let token = auth_manager
        .get_valid_token_for_account(&resolved_account_id)
        .await
        .map_err(|e| AppError::Message(format!("Antigravity OAuth 认证失败: {e}")))?;
    let project_id = auth_manager
        .project_id_for_account(&resolved_account_id)
        .await
        .map_err(|e| AppError::Message(format!("Antigravity OAuth project 读取失败: {e}")))?;

    Ok((resolved_account_id, token, project_id))
}

async fn send_antigravity_oauth_stream_check(
    model: &str,
    test_prompt: &str,
    timeout: std::time::Duration,
    access_token: &str,
    project_id: &str,
    client_profile: &str,
) -> Result<u16, AppError> {
    let model = crate::services::antigravity_models::normalize_antigravity_model_id(model);
    let request = json!({
        "model": &model,
        "contents": [{
            "role": "user",
            "parts": [{ "text": test_prompt }]
        }],
        "generationConfig": { "maxOutputTokens": 1 },
        "stream": true
    });
    let endpoint = format!("/v1beta/models/{model}:streamGenerateContent?alt=sse");
    let session_id = format!("stream-check-{}", uuid::Uuid::new_v4());
    let (url, mut body) =
        crate::proxy::build_antigravity_forward_request(&endpoint, &request, &session_id)
            .map_err(proxy_error_to_app_error)?;
    body["project"] = json!(project_id);

    let user_agent = if client_profile == "ide" {
        crate::proxy::antigravity_desktop_user_agent()
    } else {
        crate::proxy::antigravity_harness_user_agent()
    };
    let mut urls = crate::proxy::build_antigravity_forward_url_candidates(&endpoint, &request);
    if urls.is_empty() {
        urls.push(url);
    }
    let mut last_error: Option<AppError> = None;

    for url in urls {
        let mut request_builder = crate::proxy::http_client::get()
            .post(&url)
            .timeout(timeout)
            .header("authorization", format!("Bearer {access_token}"))
            .header("user-agent", user_agent.clone())
            .header("x-request-source", "local")
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .header("accept-encoding", "identity")
            .json(&body);

        if client_profile == "ide" {
            request_builder = request_builder
                .header("x-client-name", "antigravity")
                .header("x-client-version", "1.107.0");
        }

        let response = request_builder
            .send()
            .await
            .map_err(|e| AppError::Message(format!("Antigravity OAuth 请求失败: {e}")))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            let error = AppError::HttpStatus {
                status: status.as_u16(),
                body,
            };
            if matches!(status.as_u16(), 403 | 404 | 500..=599) {
                last_error = Some(error);
                continue;
            }
            return Err(error);
        }

        let status_code = status.as_u16();
        let mut stream = response.bytes_stream();
        return match stream.next().await {
            Some(Ok(_)) => Ok(status_code),
            Some(Err(err)) => Err(AppError::Message(format!(
                "Antigravity OAuth 读取响应失败: {err}"
            ))),
            None => Err(AppError::Message(
                "Antigravity OAuth 检查出错: No response data received".to_string(),
            )),
        };
    }

    Err(last_error.unwrap_or_else(|| {
        AppError::Message("Antigravity OAuth 检查出错: no endpoint attempted".to_string())
    }))
}

async fn check_kiro_oauth_provider(
    app_handle: &AppHandle,
    provider: &crate::provider::Provider,
    config: &StreamCheckConfig,
) -> StreamCheckResult {
    let effective_config = ModelTestService::merge_provider_config(provider, config);
    let mut last_result = None;

    for attempt in 0..=effective_config.max_retries {
        let result = check_kiro_oauth_provider_once(app_handle, provider, &effective_config).await;
        if result.success || attempt >= effective_config.max_retries {
            return StreamCheckResult {
                retry_count: attempt,
                ..result
            };
        }
        last_result = Some(result);
    }

    last_result.unwrap_or_else(|| StreamCheckResult {
        status: HealthStatus::Failed,
        success: false,
        message: "Kiro OAuth 检查失败".to_string(),
        response_time_ms: None,
        http_status: None,
        model_used: ModelTestService::resolve_effective_test_model(
            &AppType::Claude,
            provider,
            &effective_config,
        ),
        tested_at: chrono::Utc::now().timestamp(),
        retry_count: effective_config.max_retries,
        error_category: None,
        input_tokens: 0,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
    })
}

async fn check_kiro_oauth_provider_once(
    app_handle: &AppHandle,
    provider: &crate::provider::Provider,
    config: &StreamCheckConfig,
) -> StreamCheckResult {
    let start = Instant::now();
    let model = ModelTestService::resolve_effective_test_model(&AppType::Claude, provider, config);
    let body = json!({
        "model": model,
        "max_tokens": 1,
        "stream": true,
        "messages": [{
            "role": "user",
            "content": config.test_prompt
        }]
    });

    let timeout = std::time::Duration::from_secs(config.timeout_secs);
    let probe = async {
        let response = crate::proxy::providers::kiro_claude::forward_kiro_claude(
            Some(app_handle),
            provider,
            &body,
        )
        .await
        .map_err(proxy_error_to_app_error)?;

        let status = response.status();
        if !status.is_success() {
            let body = response
                .bytes()
                .await
                .ok()
                .and_then(|bytes| String::from_utf8(bytes.to_vec()).ok())
                .unwrap_or_default();
            return Err(AppError::HttpStatus {
                status: status.as_u16(),
                body,
            });
        }

        let status_code = status.as_u16();
        let mut stream = response.bytes_stream();
        match stream.next().await {
            Some(Ok(_)) => Ok(status_code),
            Some(Err(err)) => Err(AppError::Message(format!("Kiro OAuth 读取响应失败: {err}"))),
            None => Err(AppError::Message(
                "Kiro OAuth 检查出错: No response data received".to_string(),
            )),
        }
    };

    let result = tokio::time::timeout(timeout, probe)
        .await
        .unwrap_or_else(|_| {
            Err(AppError::Message(format!(
                "Kiro OAuth 检查超时: {} 秒",
                config.timeout_secs
            )))
        });

    let response_time = start.elapsed().as_millis() as u64;
    let tested_at = chrono::Utc::now().timestamp();

    match result {
        Ok(status_code) => StreamCheckResult {
            status: if response_time > config.degraded_threshold_ms {
                HealthStatus::Degraded
            } else {
                HealthStatus::Operational
            },
            success: true,
            message: "Check succeeded".to_string(),
            response_time_ms: Some(response_time),
            http_status: Some(status_code),
            model_used: model,
            tested_at,
            retry_count: 0,
            error_category: None,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        },
        Err(AppError::HttpStatus { status, body }) => StreamCheckResult {
            status: HealthStatus::Failed,
            success: false,
            message: if body.trim().is_empty() {
                format!("Kiro OAuth 检查出错: HTTP {status}")
            } else {
                format!("Kiro OAuth 检查出错: HTTP {status}: {}", body.trim())
            },
            response_time_ms: Some(response_time),
            http_status: Some(status),
            model_used: model,
            tested_at,
            retry_count: 0,
            error_category: ModelTestService::detect_error_category(status, &body)
                .map(str::to_string),
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        },
        Err(error) => StreamCheckResult {
            status: HealthStatus::Failed,
            success: false,
            message: format!("Kiro OAuth 检查出错: {error}"),
            response_time_ms: Some(response_time),
            http_status: None,
            model_used: model,
            tested_at,
            retry_count: 0,
            error_category: None,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        },
    }
}

async fn check_cursor_oauth_provider(
    app_handle: &AppHandle,
    app_type: &AppType,
    provider: &crate::provider::Provider,
    config: &StreamCheckConfig,
) -> StreamCheckResult {
    let effective_config = ModelTestService::merge_provider_config(provider, config);
    let provider_label = if provider.is_cursor_apikey_provider() {
        "Cursor API Key"
    } else {
        "Cursor OAuth"
    };
    let mut last_result = None;

    for attempt in 0..=effective_config.max_retries {
        let result =
            check_cursor_oauth_provider_once(app_handle, app_type, provider, &effective_config)
                .await;
        if result.success || attempt >= effective_config.max_retries {
            return StreamCheckResult {
                retry_count: attempt,
                ..result
            };
        }
        last_result = Some(result);
    }

    last_result.unwrap_or_else(|| StreamCheckResult {
        status: HealthStatus::Failed,
        success: false,
        message: format!("{provider_label} 检查失败"),
        response_time_ms: None,
        http_status: None,
        model_used: ModelTestService::resolve_effective_test_model(
            app_type,
            provider,
            &effective_config,
        ),
        tested_at: chrono::Utc::now().timestamp(),
        retry_count: effective_config.max_retries,
        error_category: None,
        input_tokens: 0,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
    })
}

async fn check_cursor_oauth_provider_once(
    app_handle: &AppHandle,
    app_type: &AppType,
    provider: &crate::provider::Provider,
    config: &StreamCheckConfig,
) -> StreamCheckResult {
    let start = Instant::now();
    let model = ModelTestService::resolve_effective_test_model(app_type, provider, config);
    let timeout = std::time::Duration::from_secs(config.timeout_secs);
    let provider_label = if provider.is_cursor_apikey_provider() {
        "Cursor API Key"
    } else {
        "Cursor OAuth"
    };

    let probe = async {
        let response = match app_type {
            AppType::Claude => {
                let body = json!({
                    "model": model,
                    "max_tokens": 1,
                    "stream": true,
                    "messages": [{
                        "role": "user",
                        "content": config.test_prompt
                    }]
                });
                if provider.is_cursor_apikey_provider() {
                    crate::proxy::providers::cursor_apikey::forward_cursor_apikey_claude(
                        provider, None, &body,
                    )
                    .await
                } else {
                    crate::proxy::providers::cursor_claude::forward_cursor_claude(
                        Some(app_handle),
                        provider,
                        None,
                        &body,
                    )
                    .await
                }
            }
            AppType::Codex => {
                let body = json!({
                    "model": model,
                    "max_output_tokens": 1,
                    "stream": true,
                    "input": config.test_prompt
                });
                if provider.is_cursor_apikey_provider() {
                    crate::proxy::providers::cursor_apikey::forward_cursor_apikey_codex(
                        provider,
                        None,
                        "/v1/responses",
                        &body,
                    )
                    .await
                } else {
                    crate::proxy::providers::cursor_codex::forward_cursor_codex(
                        Some(app_handle),
                        provider,
                        None,
                        "/v1/responses",
                        &body,
                    )
                    .await
                }
            }
            _ => Err(crate::proxy::ProxyError::InvalidRequest(format!(
                "{provider_label} stream check only supports Claude/Codex"
            ))),
        }
        .map_err(proxy_error_to_app_error)?;

        let status = response.status();
        if !status.is_success() {
            let body = response
                .bytes()
                .await
                .ok()
                .and_then(|bytes| String::from_utf8(bytes.to_vec()).ok())
                .unwrap_or_default();
            return Err(AppError::HttpStatus {
                status: status.as_u16(),
                body,
            });
        }

        let status_code = status.as_u16();
        let mut stream = response.bytes_stream();
        match stream.next().await {
            Some(Ok(_)) => Ok(status_code),
            Some(Err(err)) => Err(AppError::Message(format!(
                "{provider_label} 读取响应失败: {err}"
            ))),
            None => Err(AppError::Message(format!(
                "{provider_label} 检查出错: No response data received"
            ))),
        }
    };

    let result = tokio::time::timeout(timeout, probe)
        .await
        .unwrap_or_else(|_| {
            Err(AppError::Message(format!(
                "{provider_label} 检查超时: {} 秒",
                config.timeout_secs
            )))
        });

    let response_time = start.elapsed().as_millis() as u64;
    let tested_at = chrono::Utc::now().timestamp();

    match result {
        Ok(status_code) => StreamCheckResult {
            status: if response_time > config.degraded_threshold_ms {
                HealthStatus::Degraded
            } else {
                HealthStatus::Operational
            },
            success: true,
            message: "Check succeeded".to_string(),
            response_time_ms: Some(response_time),
            http_status: Some(status_code),
            model_used: model,
            tested_at,
            retry_count: 0,
            error_category: None,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        },
        Err(AppError::HttpStatus { status, body }) => StreamCheckResult {
            status: HealthStatus::Failed,
            success: false,
            message: if body.trim().is_empty() {
                format!("{provider_label} 检查出错: HTTP {status}")
            } else {
                format!("{provider_label} 检查出错: HTTP {status}: {}", body.trim())
            },
            response_time_ms: Some(response_time),
            http_status: Some(status),
            model_used: model,
            tested_at,
            retry_count: 0,
            error_category: ModelTestService::detect_error_category(status, &body)
                .map(str::to_string),
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        },
        Err(error) => StreamCheckResult {
            status: HealthStatus::Failed,
            success: false,
            message: format!("{provider_label} 检查出错: {error}"),
            response_time_ms: Some(response_time),
            http_status: None,
            model_used: model,
            tested_at,
            retry_count: 0,
            error_category: None,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        },
    }
}

fn proxy_error_to_app_error(error: crate::proxy::ProxyError) -> AppError {
    match error {
        crate::proxy::ProxyError::UpstreamError { status, body } => AppError::HttpStatus {
            status,
            body: body.unwrap_or_default(),
        },
        other => AppError::Message(other.to_string()),
    }
}

fn extract_claude_env_model(provider: &crate::provider::Provider) -> Option<String> {
    provider
        .settings_config
        .get("env")
        .and_then(Value::as_object)
        .and_then(|env| env.get("ANTHROPIC_MODEL"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_string)
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
        AppType::Claude => {
            provider.is_codex_oauth_provider() || provider.is_codex_official_with_managed_auth()
        }
        _ => false,
    }
}

fn resolve_codex_oauth_base_url_override(
    app_type: &AppType,
    provider: &crate::provider::Provider,
) -> Option<String> {
    if matches!(app_type, AppType::Claude) && provider.is_codex_official_with_managed_auth() {
        return Some("https://chatgpt.com/backend-api/codex".to_string());
    }
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

    let is_codex_oauth = auth_override
        .map(|auth| auth.strategy == crate::proxy::providers::AuthStrategy::CodexOAuth)
        .unwrap_or(false);
    if is_codex_oauth
        && (provider.is_codex_oauth_provider() || provider.is_codex_official_with_managed_auth())
    {
        return Ok(Some("openai_responses".to_string()));
    }

    let is_copilot = auth_override
        .map(|auth| auth.strategy == crate::proxy::providers::AuthStrategy::GitHubCopilot)
        .unwrap_or(false);
    if !is_copilot {
        return Ok(None);
    }

    let model_id = ModelTestService::resolve_effective_test_model(app_type, provider, config);
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
        resolve_codex_oauth_base_url_override, resolve_mapped_antigravity_test_model,
        uses_codex_oauth_auth,
    };
    use crate::app_config::AppType;
    use crate::provider::{
        AuthBinding, AuthBindingSource, Provider, ProviderMeta, ProviderTestConfig,
    };
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
    fn antigravity_test_model_applies_provider_model_mapping() {
        let provider = Provider {
            id: "agy".to_string(),
            name: "Antigravity CLI (agy)".to_string(),
            settings_config: json!({
                "env": {
                    "ANTHROPIC_MODEL": "claude-opus-4-7",
                    "ANTHROPIC_DEFAULT_OPUS_MODEL": "claude-opus-4-6-thinking"
                }
            }),
            website_url: None,
            category: Some("official".to_string()),
            created_at: None,
            sort_index: None,
            notes: None,
            meta: Some(ProviderMeta {
                provider_type: Some("agy_oauth".to_string()),
                test_config: Some(ProviderTestConfig {
                    enabled: true,
                    test_model: Some("claude-opus-4-7".to_string()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            icon: None,
            icon_color: None,
            in_failover_queue: false,
        };

        assert_eq!(
            resolve_mapped_antigravity_test_model(
                &AppType::Claude,
                &provider,
                &crate::services::model_test::StreamCheckConfig::default(),
            ),
            "claude-opus-4-6-thinking"
        );
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
            name: "OpenAI Official (OAuth)".to_string(),
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
        assert_eq!(
            resolve_codex_oauth_base_url_override(&AppType::Claude, &provider).as_deref(),
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

    #[test]
    fn claude_openai_official_provider_uses_codex_oauth_auth_override() {
        let provider = Provider {
            id: "p7".to_string(),
            name: "OpenAI Official (OAuth)".to_string(),
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

        assert!(uses_codex_oauth_auth(&AppType::Claude, &provider));
        assert!(provider.supports_stream_check(&AppType::Claude));
    }
}
