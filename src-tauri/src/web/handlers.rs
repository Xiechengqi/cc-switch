use std::path::{Component, Path, PathBuf};
use std::str::FromStr;

use axum::{
    body::Bytes,
    extract::{Path as AxumPath, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::{json, Value};
use tauri::{Emitter, Manager};

use crate::{
    app_config::AppType,
    commands::{
        share::{
            ClientTunnelUpdateParams, CreateShareParams, TransferShareOwnerParams,
            UpdateShareAclParams, UpdateShareAutoStartParams, UpdateShareDescriptionParams,
            UpdateShareExpirationParams, UpdateShareForSaleOfficialPricePercentParams,
            UpdateShareForSaleParams, UpdateShareOwnerEmailParams, UpdateShareParallelLimitParams,
            UpdateShareProviderBindingParams, UpdateShareSubdomainParams,
            UpdateShareTokenLimitParams,
        },
        AntigravityOAuthState, ClaudeOAuthState, CodexOAuthState, CopilotAuthState,
        CursorOAuthState, DeepSeekAccountState, GeminiOAuthState, KiroOAuthState, OauthQuotaState,
    },
    error::AppError,
    provider::{Provider, UniversalProvider},
    proxy::{
        server::ProxyState,
        types::{AppProxyConfig, GlobalProxyConfig, ProxyConfig, ProxyStatus},
        CircuitBreakerConfig,
    },
    services::{provider::ProviderSortUpdate, share::ShareService, ProviderService},
    store::AppState,
    tunnel::config::{ShareTunnelStatus, TunnelConfig},
};

const INDEX_HTML: &str = "index.html";

pub async fn context(State(state): State<ProxyState>, headers: HeaderMap) -> Response {
    match resolve_scope(&state, &headers) {
        Ok(WebScope::LocalAdmin(scope)) => Json(json!({
            "mode": "local-admin",
            "userEmail": scope.user_email,
            "role": scope.role,
            "permissions": [
                "local_admin"
            ],
        }))
        .into_response(),
        Ok(WebScope::Share(scope)) => Json(json!({
            "mode": "share",
            "shareId": scope.share.id,
            "shareName": scope.share.name,
            "subdomain": scope.share.subdomain,
            // P8: share 不再有单一 app_type；返回所有已绑定 slot 的列表。
            "supportedApps": scope.share.supported_apps(),
            "bindings": scope.share.bindings,
            "status": scope.share.status,
            "permissions": [
                "read_share"
            ],
        }))
        .into_response(),
        Err(err) => error_response(err.status, &err.message),
    }
}

pub async fn invoke(
    State(state): State<ProxyState>,
    headers: HeaderMap,
    AxumPath(command): AxumPath<String>,
    body: Bytes,
) -> Response {
    let args = if body.is_empty() {
        json!({})
    } else {
        match serde_json::from_slice::<Value>(&body) {
            Ok(value) => value,
            Err(err) => {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    &format!("invalid JSON request body: {err}"),
                );
            }
        }
    };

    match resolve_scope(&state, &headers) {
        Ok(WebScope::LocalAdmin(scope)) => {
            match invoke_local_admin_scoped(&state, scope, &command, args).await {
                Ok(value) => Json(value).into_response(),
                Err(err) => error_response(err.status, &err.message),
            }
        }
        Ok(WebScope::Share(scope)) => {
            match invoke_share_scoped(&state, scope, &command, args).await {
                Ok(value) => Json(value).into_response(),
                Err(err) => error_response(err.status, &err.message),
            }
        }
        Err(err) => error_response(err.status, &err.message),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailCodeRequest {
    email: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyEmailCodeRequest {
    email: String,
    code: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshSessionRequest {
    refresh_token: String,
}

pub async fn request_email_code(Json(input): Json<EmailCodeRequest>) -> Response {
    let email = input.email.trim().to_ascii_lowercase();
    if email.is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "email is required");
    }
    let config = current_tunnel_config();
    match crate::email_auth::request_code(&config, &email).await {
        Ok(value) => Json(value).into_response(),
        Err(err) => error_response(StatusCode::BAD_GATEWAY, &err),
    }
}

pub async fn verify_email_code(Json(input): Json<VerifyEmailCodeRequest>) -> Response {
    let email = input.email.trim().to_ascii_lowercase();
    let code = input.code.trim().to_string();
    if email.is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "email is required");
    }
    if code.is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "code is required");
    }
    let config = current_tunnel_config();
    match crate::email_auth::verify_client_web_code(&config, &email, &code).await {
        Ok(value) => Json(value).into_response(),
        Err(err) => error_response(StatusCode::BAD_GATEWAY, &err),
    }
}

pub async fn refresh_session(Json(input): Json<RefreshSessionRequest>) -> Response {
    let refresh_token = input.refresh_token.trim().to_string();
    if refresh_token.is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "refreshToken is required");
    }
    let config = current_tunnel_config();
    match crate::email_auth::refresh_client_web_session(&config, &refresh_token).await {
        Ok(value) => Json(value).into_response(),
        Err(err) => error_response(StatusCode::UNAUTHORIZED, &err),
    }
}

pub async fn serve_index(State(state): State<ProxyState>, headers: HeaderMap) -> Response {
    serve_dist_file(&state, &headers, Path::new(INDEX_HTML)).await
}

pub async fn serve_favicon(State(state): State<ProxyState>, headers: HeaderMap) -> Response {
    serve_dist_file(&state, &headers, Path::new("favicon.ico")).await
}

pub async fn serve_asset(
    State(state): State<ProxyState>,
    headers: HeaderMap,
    AxumPath(path): AxumPath<String>,
) -> Response {
    let Some(path) = sanitize_asset_path(&path) else {
        return error_response(StatusCode::BAD_REQUEST, "invalid asset path");
    };
    serve_dist_file(&state, &headers, &path).await
}

#[derive(Debug)]
struct WebError {
    status: StatusCode,
    message: String,
}

impl WebError {
    fn unauthorized(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: message.into(),
        }
    }

    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
        }
    }
}

impl From<AppError> for WebError {
    fn from(value: AppError) -> Self {
        Self::internal(value.to_string())
    }
}

#[derive(Clone)]
struct ShareScope {
    share: crate::database::ShareRecord,
}

#[derive(Clone)]
struct LocalAdminScope {
    user_email: String,
    role: String,
}

enum WebScope {
    Share(ShareScope),
    LocalAdmin(LocalAdminScope),
}

fn resolve_scope(state: &ProxyState, headers: &HeaderMap) -> Result<WebScope, WebError> {
    if let Some(user_email) = headers
        .get("x-cc-switch-web-user-email")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let role = headers
            .get("x-cc-switch-web-role")
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("owner");
        return Ok(WebScope::LocalAdmin(LocalAdminScope {
            user_email: user_email.to_ascii_lowercase(),
            role: role.to_string(),
        }));
    }

    let share_id = headers
        .get("x-cc-switch-share-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let Some(share_id) = share_id else {
        return Err(WebError::unauthorized("share id missing"));
    };

    let validation = ShareService::validate_share_for_invocation(&state.db, share_id)?
        .ok_or_else(|| WebError::unauthorized("share not found"))?;
    let Some(share) = validation.share else {
        return Err(WebError::unauthorized(
            validation
                .message
                .unwrap_or_else(|| "share is not currently routable".to_string()),
        ));
    };
    Ok(WebScope::Share(ShareScope { share }))
}

async fn invoke_local_admin_scoped(
    state: &ProxyState,
    _scope: LocalAdminScope,
    command: &str,
    args: Value,
) -> Result<Value, WebError> {
    if !is_local_admin_command_allowed(command) {
        return Err(WebError::not_found(format!(
            "local admin web command is not exposed: {command}"
        )));
    }

    match command {
        "get_settings" => Ok(json!(crate::settings::get_settings_for_frontend())),
        "save_settings" => {
            let settings: crate::settings::AppSettings =
                serde_json::from_value(args.get("settings").cloned().unwrap_or(args))
                    .map_err(|err| WebError::bad_request(format!("invalid settings: {err}")))?;
            crate::settings::update_settings(settings)
                .map_err(|err| WebError::internal(err.to_string()))?;
            Ok(json!(true))
        }
        "get_proxy_status" => Ok(json!(proxy_status(state).await)),
        "get_proxy_takeover_status" => {
            let app_state = required_app_state(state)?;
            Ok(json!(app_state
                .proxy_service
                .get_takeover_status()
                .await
                .map_err(WebError::internal)?))
        }
        "get_proxy_config" => {
            let app_state = required_app_state(state)?;
            Ok(json!(app_state.proxy_service.get_config().await))
        }
        "update_proxy_config" => {
            let app_state = required_app_state(state)?;
            let config: ProxyConfig = value_arg(&args, "config")?;
            app_state
                .proxy_service
                .update_config(&config)
                .await
                .map_err(WebError::internal)?;
            Ok(json!(null))
        }
        "get_global_proxy_config" => {
            let app_state = required_app_state(state)?;
            Ok(json!(app_state
                .db
                .get_global_proxy_config()
                .await
                .map_err(WebError::internal)?))
        }
        "update_global_proxy_config" => {
            let app_state = required_app_state(state)?;
            let config: GlobalProxyConfig = value_arg(&args, "config")?;
            app_state
                .db
                .update_global_proxy_config(config)
                .await
                .map_err(WebError::internal)?;
            Ok(json!(null))
        }
        "get_proxy_config_for_app" => {
            let app_state = required_app_state(state)?;
            let app_type = string_arg(&args, "appType")?;
            Ok(json!(app_state
                .db
                .get_proxy_config_for_app(&app_type)
                .await
                .map_err(WebError::internal)?))
        }
        "update_proxy_config_for_app" => {
            let app_state = required_app_state(state)?;
            let config: AppProxyConfig = value_arg(&args, "config")?;
            let app_type = config.app_type.clone();
            let circuit_config = CircuitBreakerConfig::from(&config);
            app_state
                .db
                .update_proxy_config_for_app(config)
                .await
                .map_err(WebError::internal)?;
            app_state
                .proxy_service
                .update_circuit_breaker_config_for_app(&app_type, circuit_config)
                .await
                .map_err(WebError::internal)?;
            Ok(json!(null))
        }
        "start_proxy_server" => {
            let app_state = required_app_state(state)?;
            Ok(json!(app_state
                .proxy_service
                .start()
                .await
                .map_err(WebError::internal)?))
        }
        "stop_proxy_server" => {
            let app_state = required_app_state(state)?;
            let takeover = app_state
                .proxy_service
                .get_takeover_status()
                .await
                .map_err(WebError::internal)?;
            if takeover.claude
                || takeover.codex
                || takeover.gemini
                || takeover.opencode
                || takeover.openclaw
            {
                return Err(WebError::bad_request(
                    "仍有应用处于代理接管状态，请先关闭对应应用接管后再停止本地路由。",
                ));
            }
            app_state
                .proxy_service
                .stop()
                .await
                .map_err(WebError::internal)?;
            Ok(json!(null))
        }
        "stop_proxy_with_restore" => {
            let app_state = required_app_state(state)?;
            app_state
                .proxy_service
                .stop_with_restore()
                .await
                .map_err(WebError::internal)?;
            Ok(json!(null))
        }
        "is_proxy_running" => {
            let app_state = required_app_state(state)?;
            Ok(json!(app_state.proxy_service.is_running().await))
        }
        "is_live_takeover_active" => {
            let app_state = required_app_state(state)?;
            Ok(json!(app_state
                .proxy_service
                .is_takeover_active()
                .await
                .map_err(WebError::internal)?))
        }
        "set_proxy_takeover_for_app" => {
            let app_state = required_app_state(state)?;
            let app_type = string_arg(&args, "appType")?;
            let enabled = bool_arg(&args, "enabled")?;
            app_state
                .proxy_service
                .set_takeover_for_app(&app_type, enabled)
                .await
                .map_err(WebError::internal)?;
            Ok(json!(null))
        }
        "switch_proxy_provider" => {
            let app_state = required_app_state(state)?;
            let app_type = string_arg(&args, "appType")?;
            let provider_id = string_arg(&args, "providerId")?;
            app_state
                .proxy_service
                .switch_proxy_target(&app_type, &provider_id)
                .await
                .map_err(WebError::internal)?;
            if let Ok(app_enum) = AppType::from_str(&app_type) {
                crate::tunnel::sync::schedule_share_runtime_refresh_after_provider_switch(
                    app_state.db.clone(),
                    app_enum,
                );
            }
            Ok(json!(null))
        }
        "list_shares" => {
            let Some(app_state) = app_state(state) else {
                return Ok(json!([]));
            };
            let shares = ShareService::list(&app_state.db)?;
            Ok(json!(shares
                .into_iter()
                .map(sanitize_share_for_web)
                .collect::<Vec<_>>()))
        }
        "get_share_detail" => {
            let share_id = string_arg(&args, "shareId")?;
            let Some(app_state) = app_state(state) else {
                return Ok(json!(null));
            };
            let share =
                ShareService::get_detail(&app_state.db, &share_id)?.map(sanitize_share_for_web);
            Ok(json!(share))
        }
        "get_tunnel_status" => {
            let share_id = string_arg(&args, "shareId")?;
            Ok(json!(share_tunnel_status(state, &share_id).await?))
        }
        "get_client_tunnel_status" => Ok(json!(client_tunnel_status(state).await)),
        "get_client_tunnel" => Ok(json!(client_tunnel_projection(state).await)),
        "get_providers" => {
            let app_state = required_app_state(state)?;
            let app_type = app_type_arg(&args, "app")?;
            Ok(json!(
                ProviderService::list(&app_state, app_type).map_err(WebError::internal)?
            ))
        }
        "get_current_provider" => {
            let app_state = required_app_state(state)?;
            let app_type = app_type_arg(&args, "app")?;
            Ok(json!(
                ProviderService::current(&app_state, app_type).map_err(WebError::internal)?
            ))
        }
        "add_provider" => {
            let app_state = required_app_state(state)?;
            let app_type = app_type_arg(&args, "app")?;
            let provider: Provider = value_arg(&args, "provider")?;
            let add_to_live = args
                .get("addToLive")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            Ok(json!(ProviderService::add(
                &app_state,
                app_type,
                provider,
                add_to_live
            )
            .map_err(WebError::internal)?))
        }
        "update_provider" => {
            let app_state = required_app_state(state)?;
            let app_type = app_type_arg(&args, "app")?;
            let provider: Provider = value_arg(&args, "provider")?;
            let original_id = args.get("originalId").and_then(Value::as_str);
            Ok(json!(ProviderService::update(
                &app_state,
                app_type.clone(),
                original_id,
                provider
            )
            .map_err(WebError::internal)?))
        }
        "delete_provider" => {
            let app_state = required_app_state(state)?;
            let app_type = app_type_arg(&args, "app")?;
            let id = string_arg(&args, "id")?;
            ProviderService::delete(&app_state, app_type, &id).map_err(WebError::internal)?;
            Ok(json!(true))
        }
        "switch_provider" => {
            let app_state = required_app_state(state)?;
            let app_type = app_type_arg(&args, "app")?;
            let id = string_arg(&args, "id")?;
            Ok(json!(
                ProviderService::switch(&app_state, app_type, &id).map_err(WebError::internal)?
            ))
        }
        "get_failover_queue" => {
            let app_state = required_app_state(state)?;
            let app_type = string_arg(&args, "appType")?;
            Ok(json!(app_state
                .db
                .get_failover_queue(&app_type)
                .map_err(WebError::internal)?))
        }
        "get_available_providers_for_failover" => {
            let app_state = required_app_state(state)?;
            let app_type = string_arg(&args, "appType")?;
            Ok(json!(app_state
                .db
                .get_available_providers_for_failover(&app_type)
                .map_err(WebError::internal)?))
        }
        "add_to_failover_queue" => {
            let app_state = required_app_state(state)?;
            let app_type = string_arg(&args, "appType")?;
            let provider_id = string_arg(&args, "providerId")?;
            app_state
                .db
                .add_to_failover_queue(&app_type, &provider_id)
                .map_err(WebError::internal)?;
            Ok(json!(null))
        }
        "remove_from_failover_queue" => {
            let app_state = required_app_state(state)?;
            let app_type = string_arg(&args, "appType")?;
            let provider_id = string_arg(&args, "providerId")?;
            app_state
                .db
                .remove_from_failover_queue(&app_type, &provider_id)
                .map_err(WebError::internal)?;
            Ok(json!(null))
        }
        "get_auto_failover_enabled" => {
            let app_state = required_app_state(state)?;
            let app_type = string_arg(&args, "appType")?;
            Ok(json!(
                app_state
                    .db
                    .get_proxy_config_for_app(&app_type)
                    .await
                    .map_err(WebError::internal)?
                    .auto_failover_enabled
            ))
        }
        "set_auto_failover_enabled" => {
            let app_state = required_app_state(state)?;
            let app = required_app_handle(state)?.clone();
            let app_type = string_arg(&args, "appType")?;
            let enabled = bool_arg(&args, "enabled")?;
            set_auto_failover_enabled_for_web(app, &app_state, &app_type, enabled).await?;
            Ok(json!(null))
        }
        "get_provider_health" => {
            let app_state = required_app_state(state)?;
            let provider_id = string_arg(&args, "providerId")?;
            let app_type = string_arg(&args, "appType")?;
            Ok(json!(app_state
                .db
                .get_provider_health(&provider_id, &app_type)
                .await
                .map_err(WebError::internal)?))
        }
        "reset_circuit_breaker" => {
            let app_state = required_app_state(state)?;
            let provider_id = string_arg(&args, "providerId")?;
            let app_type = string_arg(&args, "appType")?;
            app_state
                .db
                .update_provider_health(&provider_id, &app_type, true, None)
                .await
                .map_err(WebError::internal)?;
            app_state
                .proxy_service
                .reset_provider_circuit_breaker(&provider_id, &app_type)
                .await
                .map_err(WebError::internal)?;
            Ok(json!(null))
        }
        "stream_check_provider" => {
            let app_state = required_app_state(state)?;
            let app = required_app_handle(state)?;
            let app_type = app_type_arg(&args, "appType")?;
            let provider_id = string_arg(&args, "providerId")?;
            let providers = app_state
                .db
                .get_all_providers(app_type.as_str())
                .map_err(WebError::internal)?;
            let provider = providers
                .get(&provider_id)
                .ok_or_else(|| WebError::bad_request(format!("供应商 {provider_id} 不存在")))?;
            Ok(json!(crate::commands::run_stream_check_for_provider(
                &app_state.db,
                Some(app),
                &app_type,
                provider,
            )
            .await
            .map_err(WebError::internal)?))
        }
        "stream_check_all_providers" => {
            let app_state = required_app_state(state)?;
            let app = required_app_handle(state)?;
            let app_type = app_type_arg(&args, "appType")?;
            let proxy_targets_only = args
                .get("proxyTargetsOnly")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let providers = app_state
                .db
                .get_all_providers(app_type.as_str())
                .map_err(WebError::internal)?;
            let allowed_ids = if proxy_targets_only {
                Some(proxy_target_provider_ids(&app_state, app_type.as_str())?)
            } else {
                None
            };
            let mut results = Vec::new();
            for (provider_id, provider) in providers {
                if allowed_ids
                    .as_ref()
                    .is_some_and(|ids| !ids.contains(&provider_id))
                {
                    continue;
                }
                let result = crate::commands::run_stream_check_for_provider(
                    &app_state.db,
                    Some(app),
                    &app_type,
                    &provider,
                )
                .await
                .map_err(WebError::internal)?;
                results.push((provider_id, result));
            }
            Ok(json!(results))
        }
        "get_stream_check_config" => {
            let app_state = required_app_state(state)?;
            Ok(json!(app_state
                .db
                .get_stream_check_config()
                .map_err(WebError::internal)?))
        }
        "save_stream_check_config" => {
            let app_state = required_app_state(state)?;
            let config = value_arg(&args, "config")?;
            app_state
                .db
                .save_stream_check_config(&config)
                .map_err(WebError::internal)?;
            Ok(json!(null))
        }
        "fetch_models_for_config" => Ok(json!(crate::commands::fetch_models_for_config(
            string_arg(&args, "baseUrl")?,
            string_arg(&args, "apiKey")?,
            args.get("isFullUrl").and_then(Value::as_bool),
            optional_string_arg(&args, "modelsUrl"),
        )
        .await
        .map_err(WebError::internal)?)),
        "get_codex_oauth_models" => {
            let codex = required_state::<CodexOAuthState>(state, "codex oauth")?;
            Ok(json!(crate::commands::get_codex_oauth_models(
                optional_string_arg(&args, "accountId"),
                codex,
            )
            .await
            .map_err(WebError::internal)?))
        }
        "get_antigravity_oauth_models" => {
            let antigravity = required_state::<AntigravityOAuthState>(state, "antigravity oauth")?;
            Ok(json!(crate::commands::get_antigravity_oauth_models(
                optional_string_arg(&args, "accountId"),
                antigravity,
            )
            .await
            .map_err(WebError::internal)?))
        }
        "read_live_provider_settings" => Ok(json!(crate::commands::read_live_provider_settings(
            string_arg(&args, "app")?
        )
        .map_err(WebError::internal)?)),
        "test_api_endpoints" => Ok(json!(crate::commands::test_api_endpoints(
            value_arg(&args, "urls")?,
            args.get("timeoutSecs").and_then(Value::as_u64),
        )
        .await
        .map_err(WebError::internal)?)),
        "get_custom_endpoints" => {
            let app_state = required_app_state(state)?;
            Ok(json!(crate::commands::get_custom_endpoints(
                app_state,
                string_arg(&args, "app")?,
                string_arg(&args, "providerId")?,
            )
            .map_err(WebError::internal)?))
        }
        "add_custom_endpoint" => {
            let app_state = required_app_state(state)?;
            crate::commands::add_custom_endpoint(
                app_state,
                string_arg(&args, "app")?,
                string_arg(&args, "providerId")?,
                string_arg(&args, "url")?,
            )
            .map_err(WebError::internal)?;
            Ok(json!(null))
        }
        "remove_custom_endpoint" => {
            let app_state = required_app_state(state)?;
            crate::commands::remove_custom_endpoint(
                app_state,
                string_arg(&args, "app")?,
                string_arg(&args, "providerId")?,
                string_arg(&args, "url")?,
            )
            .map_err(WebError::internal)?;
            Ok(json!(null))
        }
        "update_endpoint_last_used" => {
            let app_state = required_app_state(state)?;
            crate::commands::update_endpoint_last_used(
                app_state,
                string_arg(&args, "app")?,
                string_arg(&args, "providerId")?,
                string_arg(&args, "url")?,
            )
            .map_err(WebError::internal)?;
            Ok(json!(null))
        }
        "get_claude_common_config_snippet" => {
            let app_state = required_app_state(state)?;
            Ok(json!(crate::commands::get_claude_common_config_snippet(
                app_state
            )
            .await
            .map_err(WebError::internal)?))
        }
        "set_claude_common_config_snippet" => {
            let app_state = required_app_state(state)?;
            crate::commands::set_claude_common_config_snippet(
                args.get("snippet")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                app_state,
            )
            .await
            .map_err(WebError::internal)?;
            Ok(json!(null))
        }
        "get_common_config_snippet" => {
            let app_state = required_app_state(state)?;
            Ok(json!(crate::commands::get_common_config_snippet(
                string_arg(&args, "appType")?,
                app_state,
            )
            .await
            .map_err(WebError::internal)?))
        }
        "set_common_config_snippet" => {
            let app_state = required_app_state(state)?;
            crate::commands::set_common_config_snippet(
                string_arg(&args, "appType")?,
                args.get("snippet")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                app_state,
            )
            .await
            .map_err(WebError::internal)?;
            Ok(json!(null))
        }
        "extract_common_config_snippet" => {
            let app_state = required_app_state(state)?;
            Ok(json!(crate::commands::extract_common_config_snippet(
                string_arg(&args, "appType")?,
                optional_string_arg(&args, "settingsConfig"),
                app_state,
            )
            .await
            .map_err(WebError::internal)?))
        }
        "queryProviderUsage" => {
            let app = required_app_handle(state)?.clone();
            let app_state = required_app_state(state)?;
            let copilot = required_state::<CopilotAuthState>(state, "copilot auth")?;
            Ok(json!(crate::commands::queryProviderUsage(
                app,
                app_state,
                copilot,
                string_arg(&args, "providerId")?,
                string_arg(&args, "app")?,
            )
            .await
            .map_err(WebError::internal)?))
        }
        "testUsageScript" => {
            let app_state = required_app_state(state)?;
            Ok(json!(crate::commands::testUsageScript(
                app_state,
                string_arg(&args, "providerId")?,
                string_arg(&args, "app")?,
                string_arg(&args, "scriptCode")?,
                args.get("timeout").and_then(Value::as_u64),
                optional_string_arg(&args, "apiKey"),
                optional_string_arg(&args, "baseUrl"),
                optional_string_arg(&args, "accessToken"),
                optional_string_arg(&args, "userId"),
                optional_string_arg(&args, "templateType"),
            )
            .await
            .map_err(WebError::internal)?))
        }
        "get_usage_summary" => {
            let app_state = required_app_state(state)?;
            Ok(json!(crate::commands::get_usage_summary(
                app_state,
                optional_i64_arg(&args, "startDate"),
                optional_i64_arg(&args, "endDate"),
                optional_string_arg(&args, "appType"),
            )
            .map_err(WebError::internal)?))
        }
        "get_usage_summary_by_app" => {
            let app_state = required_app_state(state)?;
            Ok(json!(crate::commands::get_usage_summary_by_app(
                app_state,
                optional_i64_arg(&args, "startDate"),
                optional_i64_arg(&args, "endDate"),
            )
            .map_err(WebError::internal)?))
        }
        "get_usage_trends" => {
            let app_state = required_app_state(state)?;
            Ok(json!(crate::commands::get_usage_trends(
                app_state,
                optional_i64_arg(&args, "startDate"),
                optional_i64_arg(&args, "endDate"),
                optional_string_arg(&args, "appType"),
            )
            .map_err(WebError::internal)?))
        }
        "get_provider_stats" => {
            let app_state = required_app_state(state)?;
            Ok(json!(crate::commands::get_provider_stats(
                app_state,
                optional_i64_arg(&args, "startDate"),
                optional_i64_arg(&args, "endDate"),
                optional_string_arg(&args, "appType"),
            )
            .map_err(WebError::internal)?))
        }
        "get_model_stats" => {
            let app_state = required_app_state(state)?;
            Ok(json!(crate::commands::get_model_stats(
                app_state,
                optional_i64_arg(&args, "startDate"),
                optional_i64_arg(&args, "endDate"),
                optional_string_arg(&args, "appType"),
            )
            .map_err(WebError::internal)?))
        }
        "get_request_logs" => {
            let app_state = required_app_state(state)?;
            Ok(json!(crate::commands::get_request_logs(
                app_state,
                value_arg(&args, "filters")?,
                optional_u32_arg(&args, "page").unwrap_or(0),
                optional_u32_arg(&args, "pageSize").unwrap_or(20),
            )
            .map_err(WebError::internal)?))
        }
        "get_request_detail" => {
            let app_state = required_app_state(state)?;
            Ok(json!(crate::commands::get_request_detail(
                app_state,
                string_arg(&args, "requestId")?
            )
            .map_err(WebError::internal)?))
        }
        "get_model_pricing" => {
            let app_state = required_app_state(state)?;
            Ok(json!(
                crate::commands::get_model_pricing(app_state).map_err(WebError::internal)?
            ))
        }
        "update_model_pricing" => {
            let app_state = required_app_state(state)?;
            crate::commands::update_model_pricing(
                app_state,
                string_arg(&args, "modelId")?,
                string_arg(&args, "displayName")?,
                string_arg(&args, "inputCost")?,
                string_arg(&args, "outputCost")?,
                string_arg(&args, "cacheReadCost")?,
                string_arg(&args, "cacheCreationCost")?,
            )
            .map_err(WebError::internal)?;
            Ok(json!(null))
        }
        "delete_model_pricing" => {
            let app_state = required_app_state(state)?;
            crate::commands::delete_model_pricing(app_state, string_arg(&args, "modelId")?)
                .map_err(WebError::internal)?;
            Ok(json!(null))
        }
        "check_provider_limits" => {
            let app_state = required_app_state(state)?;
            Ok(json!(crate::commands::check_provider_limits(
                app_state,
                string_arg(&args, "providerId")?,
                string_arg(&args, "appType")?,
            )
            .map_err(WebError::internal)?))
        }
        "sync_session_usage" => {
            let app_state = required_app_state(state)?;
            Ok(json!(
                crate::commands::sync_session_usage(app_state).map_err(WebError::internal)?
            ))
        }
        "get_usage_data_sources" => {
            let app_state = required_app_state(state)?;
            Ok(json!(
                crate::commands::get_usage_data_sources(app_state).map_err(WebError::internal)?
            ))
        }
        "check_env_conflicts" => Ok(json!(crate::commands::check_env_conflicts(string_arg(
            &args, "app"
        )?)
        .map_err(WebError::internal)?)),
        "delete_env_vars" => Ok(json!(crate::commands::delete_env_vars(value_arg(
            &args,
            "conflicts"
        )?)
        .map_err(WebError::internal)?)),
        "restore_env_backup" => {
            crate::commands::restore_env_backup(string_arg(&args, "backupPath")?)
                .map_err(WebError::internal)?;
            Ok(json!(null))
        }
        "get_circuit_breaker_config" => {
            let app_state = required_app_state(state)?;
            Ok(json!(app_state
                .db
                .get_circuit_breaker_config()
                .await
                .map_err(WebError::internal)?))
        }
        "update_circuit_breaker_config" => {
            let app_state = required_app_state(state)?;
            let config: CircuitBreakerConfig = value_arg(&args, "config")?;
            app_state
                .db
                .update_circuit_breaker_config(&config)
                .await
                .map_err(WebError::internal)?;
            app_state
                .proxy_service
                .update_circuit_breaker_configs(config)
                .await
                .map_err(WebError::internal)?;
            Ok(json!(null))
        }
        "get_circuit_breaker_stats" => Ok(json!(null)),
        "auth_list_accounts" => managed_auth_command(state, command, args).await,
        "auth_get_status" => managed_auth_command(state, command, args).await,
        "auth_start_login" => managed_auth_command(state, command, args).await,
        "auth_submit_oauth_code" => managed_auth_command(state, command, args).await,
        "auth_poll_for_account" => managed_auth_command(state, command, args).await,
        "auth_remove_account" => managed_auth_command(state, command, args).await,
        "auth_set_default_account" => managed_auth_command(state, command, args).await,
        "auth_logout" => managed_auth_command(state, command, args).await,
        "copilot_list_accounts" => copilot_command(state, command, args).await,
        "copilot_get_auth_status" => copilot_command(state, command, args).await,
        "copilot_start_device_flow" => copilot_command(state, command, args).await,
        "copilot_poll_for_auth" => copilot_command(state, command, args).await,
        "copilot_poll_for_account" => copilot_command(state, command, args).await,
        "copilot_remove_account" => copilot_command(state, command, args).await,
        "copilot_set_default_account" => copilot_command(state, command, args).await,
        "copilot_logout" => copilot_command(state, command, args).await,
        "copilot_is_authenticated" => copilot_command(state, command, args).await,
        "copilot_get_models" => copilot_command(state, command, args).await,
        "copilot_get_models_for_account" => copilot_command(state, command, args).await,
        "copilot_get_usage" => copilot_command(state, command, args).await,
        "copilot_get_usage_for_account" => copilot_command(state, command, args).await,
        "deepseek_account_list" => deepseek_command(state, command, args).await,
        "deepseek_account_status" => deepseek_command(state, command, args).await,
        "deepseek_account_add" => deepseek_command(state, command, args).await,
        "deepseek_account_remove" => deepseek_command(state, command, args).await,
        "deepseek_account_set_default" => deepseek_command(state, command, args).await,
        "get_cached_oauth_quota" => oauth_quota_command(state, command, args).await,
        "refresh_oauth_quota" => oauth_quota_command(state, command, args).await,
        "get_claude_oauth_quota" => oauth_quota_command(state, command, args).await,
        "get_codex_oauth_quota" => oauth_quota_command(state, command, args).await,
        "get_subscription_quota" => subscription_command(state, command, args).await,
        "get_coding_plan_quota" => subscription_command(state, command, args).await,
        "get_balance" => subscription_command(state, command, args).await,
        // ===== P0: share metadata / EditDialog 直调（12 条）=====
        "update_share_owner_email" => {
            let app_state = required_app_state(state)?;
            let params: UpdateShareOwnerEmailParams = value_arg(&args, "params")?;
            Ok(json!(sanitize_share_for_web(
                ShareService::update_owner_email(
                    &app_state.db,
                    &params.share_id,
                    &params.owner_email,
                )
                .map_err(WebError::internal)?,
            )))
        }
        "transfer_share_owner" => {
            let app_state = required_app_state(state)?;
            let params: TransferShareOwnerParams = value_arg(&args, "params")?;
            Ok(json!(sanitize_share_for_web(
                ShareService::transfer_owner_email(
                    &app_state.db,
                    &params.share_id,
                    &params.target_email,
                )
                .map_err(WebError::internal)?,
            )))
        }
        "update_share_acl" => {
            let app_state = required_app_state(state)?;
            let params: UpdateShareAclParams = value_arg(&args, "params")?;
            // 与 Tauri 命令一致：从已存在记录读 owner_email，避免 web 调用方伪造 owner。
            let share = ShareService::get_detail(&app_state.db, &params.share_id)
                .map_err(WebError::internal)?
                .ok_or_else(|| {
                    WebError::bad_request(format!("Share not found: {}", params.share_id))
                })?;
            let owner_email = share.owner_email.clone();
            Ok(json!(sanitize_share_for_web(
                ShareService::update_acl(
                    &app_state.db,
                    &params.share_id,
                    &owner_email,
                    params.shared_with_emails,
                    &params.market_access_mode,
                )
                .map_err(WebError::internal)?,
            )))
        }
        "update_share_subdomain" => {
            let app_state = required_app_state(state)?;
            let params: UpdateShareSubdomainParams = value_arg(&args, "params")?;
            // 镜像 Tauri 命令：先 claim_share_subdomain → stop tunnel → update DB → 若 active 重启 tunnel。
            let share = ShareService::get_detail(&app_state.db, &params.share_id)
                .map_err(WebError::internal)?
                .ok_or_else(|| {
                    WebError::bad_request(format!("Share not found: {}", params.share_id))
                })?;
            let requested_subdomain = params.subdomain.clone();
            let mut next = share.clone();
            next.subdomain = Some(requested_subdomain.clone());
            crate::tunnel::sync::claim_share_subdomain(&next, &app_state.db)
                .await
                .map_err(|e| {
                    WebError::internal(crate::email_auth::humanize_remote_owner_binding_error(&e))
                })?;
            {
                let mut mgr = app_state.tunnel_manager.write().await;
                if mgr.get_info(&params.share_id).is_some() {
                    mgr.stop_tunnel(&params.share_id)
                        .await
                        .map_err(WebError::internal)?;
                }
            }
            let updated = ShareService::update_subdomain(
                &app_state.db,
                &params.share_id,
                &requested_subdomain,
            )
            .map_err(WebError::internal)?;
            if updated.status == "active" {
                crate::commands::share::start_share_tunnel_with_error_tracking(
                    &app_state,
                    &params.share_id,
                )
                .await
                .map_err(WebError::internal)?;
            }
            Ok(json!(sanitize_share_for_web(updated)))
        }
        "update_share_description" => {
            let app_state = required_app_state(state)?;
            let params: UpdateShareDescriptionParams = value_arg(&args, "params")?;
            Ok(json!(sanitize_share_for_web(
                ShareService::update_description(
                    &app_state.db,
                    &params.share_id,
                    params.description,
                )
                .map_err(WebError::internal)?,
            )))
        }
        "update_share_expiration" => {
            let app_state = required_app_state(state)?;
            let params: UpdateShareExpirationParams = value_arg(&args, "params")?;
            Ok(json!(sanitize_share_for_web(
                ShareService::update_expires_at(
                    &app_state.db,
                    &params.share_id,
                    &params.expires_at,
                )
                .map_err(WebError::internal)?,
            )))
        }
        "update_share_auto_start" => {
            let app_state = required_app_state(state)?;
            let params: UpdateShareAutoStartParams = value_arg(&args, "params")?;
            Ok(json!(sanitize_share_for_web(
                ShareService::update_auto_start(
                    &app_state.db,
                    &params.share_id,
                    params.auto_start,
                )
                .map_err(WebError::internal)?,
            )))
        }
        "update_share_for_sale" => {
            let app_state = required_app_state(state)?;
            let params: UpdateShareForSaleParams = value_arg(&args, "params")?;
            Ok(json!(sanitize_share_for_web(
                ShareService::update_for_sale(
                    &app_state.db,
                    &params.share_id,
                    &params.for_sale,
                )
                .map_err(WebError::internal)?,
            )))
        }
        "update_share_for_sale_official_price_percent" => {
            let app_state = required_app_state(state)?;
            let params: UpdateShareForSaleOfficialPricePercentParams = value_arg(&args, "params")?;
            Ok(json!(sanitize_share_for_web(
                ShareService::update_for_sale_official_price_percent_by_app(
                    &app_state.db,
                    &params.share_id,
                    params.pricing,
                )
                .map_err(WebError::internal)?,
            )))
        }
        "update_share_token_limit" => {
            let app_state = required_app_state(state)?;
            let params: UpdateShareTokenLimitParams = value_arg(&args, "params")?;
            Ok(json!(sanitize_share_for_web(
                ShareService::update_token_limit(
                    &app_state.db,
                    &params.share_id,
                    params.token_limit,
                )
                .map_err(WebError::internal)?,
            )))
        }
        "update_share_parallel_limit" => {
            let app_state = required_app_state(state)?;
            let params: UpdateShareParallelLimitParams = value_arg(&args, "params")?;
            Ok(json!(sanitize_share_for_web(
                ShareService::update_parallel_limit(
                    &app_state.db,
                    &params.share_id,
                    params.parallel_limit,
                )
                .map_err(WebError::internal)?,
            )))
        }
        "update_share_provider_binding" => {
            let app_state = required_app_state(state)?;
            let params: UpdateShareProviderBindingParams = value_arg(&args, "params")?;
            Ok(json!(sanitize_share_for_web(
                ShareService::update_provider_binding(
                    &app_state.db,
                    &params.share_id,
                    &params.app_type,
                    params.provider_id.as_deref(),
                )
                .map_err(WebError::internal)?,
            )))
        }
        // ===== P1: share lifecycle（8 条）=====
        "create_share" => {
            let app_state = required_app_state(state)?;
            let params: CreateShareParams = value_arg(&args, "params")?;
            // 镜像 Tauri 命令：尝试 5 次申请 subdomain（被占就重试随机生成的）。
            let requested_subdomain = params.subdomain.clone();
            let mut last_claim_error: Option<String> = None;
            let mut created = None;
            for _ in 0..5 {
                let candidate = ShareService::prepare_create(
                    &app_state.db,
                    crate::services::share::PrepareShareParams {
                        owner_email: params.owner_email.clone(),
                        bindings: params.bindings.clone(),
                        dynamic_apps: params.dynamic_apps.iter().cloned().collect(),
                        description: params.description.clone(),
                        for_sale: params.for_sale.clone(),
                        token_limit: params.token_limit,
                        parallel_limit: params.parallel_limit,
                        expires_in_secs: params.expires_in_secs,
                        subdomain: requested_subdomain.clone(),
                        auto_start: params.auto_start,
                    },
                )
                .map_err(WebError::internal)?;
                match crate::tunnel::sync::claim_share_subdomain(&candidate, &app_state.db).await {
                    Ok(()) => {
                        created = Some(candidate);
                        break;
                    }
                    Err(err)
                        if requested_subdomain.is_none()
                            && err.contains("subdomain already claimed") =>
                    {
                        last_claim_error = Some(err);
                        continue;
                    }
                    Err(err) => {
                        return Err(WebError::internal(
                            crate::email_auth::humanize_remote_owner_binding_error(&err),
                        ));
                    }
                }
            }
            let share = created.ok_or_else(|| {
                WebError::internal(crate::email_auth::humanize_remote_owner_binding_error(
                    &last_claim_error.unwrap_or_else(|| {
                        "unable to allocate an available subdomain".to_string()
                    }),
                ))
            })?;
            Ok(json!(sanitize_share_for_web(
                ShareService::create(&app_state.db, share).map_err(WebError::internal)?,
            )))
        }
        "delete_share" => {
            let app_state = required_app_state(state)?;
            let share_id = string_arg(&args, "shareId")?;
            // 镜像 Tauri 命令：先停 tunnel，再删 DB，最后 schedule remote 通知。
            {
                let mut mgr = app_state.tunnel_manager.write().await;
                if mgr.get_info(&share_id).is_some() {
                    if let Err(e) = mgr.stop_tunnel(&share_id).await {
                        log::warn!("[Web] 停止隧道失败（将继续删除）: {e}");
                    }
                }
            }
            ShareService::delete(&app_state.db, &share_id).map_err(WebError::internal)?;
            crate::tunnel::sync::schedule_delete_share(share_id);
            Ok(json!(true))
        }
        "pause_share" => {
            let app_state = required_app_state(state)?;
            let share_id = string_arg(&args, "shareId")?;
            ShareService::pause(&app_state.db, &share_id).map_err(WebError::internal)?;
            Ok(json!(true))
        }
        "resume_share" => {
            let app_state = required_app_state(state)?;
            let share_id = string_arg(&args, "shareId")?;
            ShareService::resume(&app_state.db, &share_id).map_err(WebError::internal)?;
            Ok(json!(true))
        }
        "enable_share" => {
            let app_state = required_app_state(state)?;
            let share_id = string_arg(&args, "shareId")?;
            ShareService::resume(&app_state.db, &share_id).map_err(WebError::internal)?;
            let info = crate::commands::share::start_share_tunnel_with_error_tracking(
                &app_state,
                &share_id,
            )
            .await
            .map_err(WebError::internal)?;
            Ok(json!(info))
        }
        "disable_share" => {
            let app_state = required_app_state(state)?;
            let share_id = string_arg(&args, "shareId")?;
            // 镜像 Tauri 命令：清 tunnel 记录 → pause → auto_start=false → 远端 sync → 停 tunnel。
            app_state
                .db
                .clear_share_tunnel(&share_id)
                .map_err(WebError::internal)?;
            ShareService::pause(&app_state.db, &share_id).map_err(WebError::internal)?;
            app_state
                .db
                .update_share_auto_start(&share_id, false)
                .map_err(WebError::internal)?;
            if let Ok(Some(share)) = app_state.db.get_share_by_id(&share_id) {
                let metadata = crate::tunnel::sync::share_metadata_from_record(&share);
                if let Err(err) = crate::tunnel::sync::sync_share_metadata_now(metadata).await {
                    log::warn!("[Web] immediate remote sync after disable failed for {share_id}: {err}");
                }
            }
            {
                let mut mgr = app_state.tunnel_manager.write().await;
                if mgr.get_info(&share_id).is_some() {
                    match tokio::time::timeout(
                        std::time::Duration::from_secs(5),
                        mgr.stop_tunnel(&share_id),
                    )
                    .await
                    {
                        Ok(Ok(())) => {}
                        Ok(Err(err)) => {
                            log::warn!("[Web] stop tunnel after disable failed for {share_id}: {err}");
                        }
                        Err(_) => {
                            log::warn!("[Web] stop tunnel after disable timed out for {share_id}");
                        }
                    }
                }
            }
            Ok(json!(true))
        }
        "reset_share_usage" => {
            let app_state = required_app_state(state)?;
            let share_id = string_arg(&args, "shareId")?;
            Ok(json!(sanitize_share_for_web(
                ShareService::reset_usage(&app_state.db, &share_id).map_err(WebError::internal)?,
            )))
        }
        "list_share_binding_history" => {
            let app_state = required_app_state(state)?;
            let share_id = string_arg(&args, "shareId")?;
            let limit = args
                .get("limit")
                .and_then(Value::as_u64)
                .map(|v| v as usize)
                .unwrap_or(20);
            Ok(json!(ShareService::list_binding_history(
                &app_state.db,
                &share_id,
                limit,
            )
            .map_err(WebError::internal)?))
        }
        // ===== P2: tunnel 控制（3 share + 4 client = 7 条）=====
        "start_share_tunnel" => {
            let app_state = required_app_state(state)?;
            let share_id = string_arg(&args, "shareId")?;
            let info = crate::commands::share::start_share_tunnel_with_error_tracking(
                &app_state,
                &share_id,
            )
            .await
            .map_err(WebError::internal)?;
            Ok(json!(info))
        }
        "stop_share_tunnel" => {
            let app_state = required_app_state(state)?;
            let share_id = string_arg(&args, "shareId")?;
            {
                let mut mgr = app_state.tunnel_manager.write().await;
                mgr.stop_tunnel(&share_id).await.map_err(WebError::internal)?;
            }
            app_state
                .db
                .clear_share_tunnel(&share_id)
                .map_err(WebError::internal)?;
            Ok(json!(true))
        }
        "configure_tunnel" => {
            let app_state = required_app_state(state)?;
            let config: TunnelConfig = value_arg(&args, "config")?;
            // 与 Tauri 一致：持久化到 AppSettings + 同步 TunnelManager 里的 config。
            let mut settings = crate::settings::get_settings();
            settings.set_share_router_domain(Some(config.domain.clone()));
            crate::settings::update_settings(settings).map_err(WebError::internal)?;
            let mut mgr = app_state.tunnel_manager.write().await;
            mgr.set_config(config);
            Ok(json!(true))
        }
        "claim_client_tunnel" => {
            let app_state = required_app_state(state)?;
            let params: ClientTunnelUpdateParams = value_arg(&args, "params")?;
            Ok(json!(crate::commands::share::write_client_tunnel_config(
                &app_state, params, true,
            )
            .await
            .map_err(WebError::internal)?))
        }
        "update_client_tunnel" => {
            let app_state = required_app_state(state)?;
            let params: ClientTunnelUpdateParams = value_arg(&args, "params")?;
            Ok(json!(crate::commands::share::write_client_tunnel_config(
                &app_state, params, false,
            )
            .await
            .map_err(WebError::internal)?))
        }
        "start_client_tunnel" => {
            let app_state = required_app_state(state)?;
            let info = crate::commands::share::start_client_tunnel_with_error_tracking(&app_state)
                .await
                .map_err(WebError::internal)?;
            Ok(json!(info))
        }
        "stop_client_tunnel" => {
            let app_state = required_app_state(state)?;
            let mut mgr = app_state.tunnel_manager.write().await;
            if mgr
                .get_info(crate::commands::share::WEB_CLIENT_TUNNEL_ID)
                .is_some()
            {
                mgr.stop_tunnel(crate::commands::share::WEB_CLIENT_TUNNEL_ID)
                    .await
                    .map_err(WebError::internal)?;
            }
            Ok(json!(true))
        }
        // ===== P3: universal provider + 杂项（7 条）=====
        "get_universal_providers" => {
            let app_state = required_app_state(state)?;
            Ok(json!(
                ProviderService::list_universal(&app_state).map_err(WebError::internal)?
            ))
        }
        "get_universal_provider" => {
            let app_state = required_app_state(state)?;
            let id = string_arg(&args, "id")?;
            Ok(json!(
                ProviderService::get_universal(&app_state, &id).map_err(WebError::internal)?
            ))
        }
        "upsert_universal_provider" => {
            let app_state = required_app_state(state)?;
            let provider: UniversalProvider = value_arg(&args, "provider")?;
            let id = provider.id.clone();
            let result = ProviderService::upsert_universal(&app_state, provider)
                .map_err(WebError::internal)?;
            // emit 事件让 desktop UI 同步刷新（如果它和 web 共用同一进程）。
            if let Ok(handle) = required_app_handle(state) {
                let _ = handle.emit(
                    "universal-provider-synced",
                    crate::commands::UniversalProviderSyncedEvent {
                        action: "upsert".to_string(),
                        id,
                    },
                );
            }
            Ok(json!(result))
        }
        "delete_universal_provider" => {
            let app_state = required_app_state(state)?;
            let id = string_arg(&args, "id")?;
            let result = ProviderService::delete_universal(&app_state, &id)
                .map_err(WebError::internal)?;
            if let Ok(handle) = required_app_handle(state) {
                let _ = handle.emit(
                    "universal-provider-synced",
                    crate::commands::UniversalProviderSyncedEvent {
                        action: "delete".to_string(),
                        id,
                    },
                );
            }
            Ok(json!(result))
        }
        "sync_universal_provider" => {
            let app_state = required_app_state(state)?;
            let id = string_arg(&args, "id")?;
            let result = ProviderService::sync_universal_to_apps(&app_state, &id)
                .map_err(WebError::internal)?;
            if let Ok(handle) = required_app_handle(state) {
                let _ = handle.emit(
                    "universal-provider-synced",
                    crate::commands::UniversalProviderSyncedEvent {
                        action: "sync".to_string(),
                        id,
                    },
                );
            }
            Ok(json!(result))
        }
        "update_providers_sort_order" => {
            let app_state = required_app_state(state)?;
            let app_type = app_type_arg(&args, "app")?;
            let updates: Vec<ProviderSortUpdate> = value_arg(&args, "updates")?;
            Ok(json!(ProviderService::update_sort_order(
                &app_state, app_type, updates,
            )
            .map_err(WebError::internal)?))
        }
        "get_build_info" => Ok(crate::commands::get_build_info()),
        "get_share_connect_info" => {
            let app_state = required_app_state(state)?;
            let share_id = string_arg(&args, "shareId")?;
            let share = ShareService::get_detail(&app_state.db, &share_id)
                .map_err(WebError::internal)?
                .ok_or_else(|| WebError::not_found(format!("Share not found: {share_id}")))?;
            Ok(share_connect_info(&share))
        }
        "list_share_markets" => Ok(json!(
            crate::commands::share::list_share_markets()
                .await
                .map_err(WebError::internal)?
        )),
        _ => Err(WebError::not_found(format!(
            "local admin web command is allowlisted but not implemented yet: {command}"
        ))),
    }
}

async fn invoke_share_scoped(
    state: &ProxyState,
    scope: ShareScope,
    command: &str,
    _args: Value,
) -> Result<Value, WebError> {
    let share_id = scope.share.id.as_str();
    match command {
        "get_settings" => Ok(json!(share_settings_projection())),
        "get_proxy_status" => Ok(json!(proxy_status(state).await)),
        "get_proxy_takeover_status" => Ok(json!({
            "claude": false,
            "codex": false,
            "gemini": false,
            "opencode": false,
            "openclaw": false,
            "hermes": false,
        })),
        "get_providers" => Ok(json!({})),
        // P8: share 端 web admin 不再展示单一 current_provider；返回所有 slot bindings。
        "get_current_provider" => Ok(json!(scope.share.bindings.clone())),
        "list_shares" => Ok(json!([sanitize_share_for_web(scope.share.clone())])),
        "get_share_detail" => Ok(json!(Some(sanitize_share_for_web(scope.share.clone())))),
        "get_tunnel_status" => Ok(json!(share_tunnel_status(state, share_id).await?)),
        "get_share_connect_info" => Ok(json!(share_connect_info(&scope.share))),
        "list_share_markets" => Ok(json!([])),
        _ => Err(WebError::not_found(format!(
            "share web command is not exposed: {command}"
        ))),
    }
}

fn sanitize_share_for_web(mut share: crate::database::ShareRecord) -> crate::database::ShareRecord {
    share.api_key.clear();
    share.settings_config = None;
    share
}

async fn proxy_status(state: &ProxyState) -> ProxyStatus {
    let status = state.status.read().await.clone();
    if status.running {
        status
    } else {
        ProxyStatus {
            running: true,
            address: state.config.read().await.listen_address.clone(),
            port: state.config.read().await.listen_port,
            ..status
        }
    }
}

async fn share_tunnel_status(
    state: &ProxyState,
    share_id: &str,
) -> Result<ShareTunnelStatus, WebError> {
    let Some(app_state) = state
        .app_handle
        .as_ref()
        .and_then(|app| app.try_state::<AppState>())
    else {
        return Ok(ShareTunnelStatus {
            info: None,
            last_error: None,
            requires_owner_login: false,
        });
    };
    let mgr = app_state.tunnel_manager.read().await;
    let info = mgr.get_info(share_id);
    let last_error = mgr.get_last_error(share_id);
    Ok(ShareTunnelStatus {
        requires_owner_login: last_error
            .as_deref()
            .map(crate::commands::share::requires_owner_login_for_web)
            .unwrap_or(false),
        info,
        last_error,
    })
}

fn app_state(state: &ProxyState) -> Option<tauri::State<'_, AppState>> {
    state
        .app_handle
        .as_ref()
        .and_then(|app| app.try_state::<AppState>())
}

fn required_app_handle(state: &ProxyState) -> Result<&tauri::AppHandle, WebError> {
    state
        .app_handle
        .as_ref()
        .ok_or_else(|| WebError::internal("app handle is unavailable"))
}

fn required_app_state(state: &ProxyState) -> Result<tauri::State<'_, AppState>, WebError> {
    app_state(state).ok_or_else(|| WebError::internal("app state is unavailable"))
}

fn required_state<'a, T: Send + Sync + 'static>(
    state: &'a ProxyState,
    label: &str,
) -> Result<tauri::State<'a, T>, WebError> {
    required_app_handle(state)?
        .try_state::<T>()
        .ok_or_else(|| WebError::internal(format!("{label} state is unavailable")))
}

fn value_arg<T: DeserializeOwned>(args: &Value, key: &str) -> Result<T, WebError> {
    let value = args
        .get(key)
        .cloned()
        .ok_or_else(|| WebError::bad_request(format!("{key} is required")))?;
    serde_json::from_value(value)
        .map_err(|err| WebError::bad_request(format!("invalid {key}: {err}")))
}

fn optional_string_arg(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn optional_i64_arg(args: &Value, key: &str) -> Option<i64> {
    args.get(key).and_then(Value::as_i64)
}

fn optional_u32_arg(args: &Value, key: &str) -> Option<u32> {
    args.get(key)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
}

fn bool_arg(args: &Value, key: &str) -> Result<bool, WebError> {
    args.get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| WebError::bad_request(format!("{key} is required")))
}

fn app_type_arg(args: &Value, key: &str) -> Result<AppType, WebError> {
    AppType::from_str(&string_arg(args, key)?).map_err(WebError::internal)
}

fn proxy_target_provider_ids(
    state: &AppState,
    app_type: &str,
) -> Result<std::collections::HashSet<String>, WebError> {
    let mut ids = std::collections::HashSet::new();
    if let Some(current_id) = state
        .db
        .get_current_provider(app_type)
        .map_err(WebError::internal)?
    {
        ids.insert(current_id);
    }
    for item in state
        .db
        .get_failover_queue(app_type)
        .map_err(WebError::internal)?
    {
        ids.insert(item.provider_id);
    }
    Ok(ids)
}

async fn set_auto_failover_enabled_for_web(
    app: tauri::AppHandle,
    state: &AppState,
    app_type: &str,
    enabled: bool,
) -> Result<(), WebError> {
    let mut config = state
        .db
        .get_proxy_config_for_app(app_type)
        .await
        .map_err(WebError::internal)?;

    if enabled && !config.enabled {
        return Err(WebError::bad_request(
            "需要先启用该应用的代理接管，再开启故障转移",
        ));
    }

    let mut auto_added_provider_id: Option<String> = None;
    let p1_provider_id = if enabled {
        let mut queue = state
            .db
            .get_failover_queue(app_type)
            .map_err(WebError::internal)?;

        if queue.is_empty() {
            let app_enum = AppType::from_str(app_type)
                .map_err(|_| WebError::bad_request(format!("无效的应用类型: {app_type}")))?;
            let current_id = crate::settings::get_effective_current_provider(&state.db, &app_enum)
                .map_err(WebError::internal)?
                .ok_or_else(|| {
                    WebError::bad_request("故障转移队列为空，且未设置当前供应商，无法开启故障转移")
                })?;
            state
                .db
                .add_to_failover_queue(app_type, &current_id)
                .map_err(WebError::internal)?;
            auto_added_provider_id = Some(current_id);
            queue = state
                .db
                .get_failover_queue(app_type)
                .map_err(WebError::internal)?;
        }

        queue
            .first()
            .map(|item| item.provider_id.clone())
            .ok_or_else(|| WebError::bad_request("故障转移队列为空，无法开启故障转移"))?
    } else {
        String::new()
    };

    if enabled {
        if let Err(err) = state
            .proxy_service
            .switch_proxy_target(app_type, &p1_provider_id)
            .await
        {
            if let Some(provider_id) = auto_added_provider_id {
                let _ = state.db.remove_from_failover_queue(app_type, &provider_id);
            }
            return Err(WebError::internal(err));
        }
    }

    config.auto_failover_enabled = enabled;
    state
        .db
        .update_proxy_config_for_app(config)
        .await
        .map_err(WebError::internal)?;

    if enabled {
        let _ = app.emit(
            "provider-switched",
            json!({
                "appType": app_type,
                "providerId": p1_provider_id,
                "source": "failoverEnabled"
            }),
        );
    }
    if let Ok(new_menu) = crate::tray::create_tray_menu(&app, state) {
        if let Some(tray) = app.tray_by_id(crate::tray::TRAY_ID) {
            let _ = tray.set_menu(Some(new_menu));
        }
    }
    Ok(())
}

async fn managed_auth_command(
    state: &ProxyState,
    command: &str,
    args: Value,
) -> Result<Value, WebError> {
    let copilot = required_state::<CopilotAuthState>(state, "copilot auth")?;
    let codex = required_state::<CodexOAuthState>(state, "codex oauth")?;
    let claude = required_state::<ClaudeOAuthState>(state, "claude oauth")?;
    let gemini = required_state::<GeminiOAuthState>(state, "gemini oauth")?;
    let antigravity = required_state::<AntigravityOAuthState>(state, "antigravity oauth")?;
    let kiro = required_state::<KiroOAuthState>(state, "kiro oauth")?;
    let cursor = required_state::<CursorOAuthState>(state, "cursor oauth")?;
    let auth_provider = string_arg(&args, "authProvider")?;
    if command == "auth_start_login" && is_local_callback_auth_provider(&auth_provider) {
        return Err(WebError::bad_request(local_callback_auth_blocked_message()));
    }

    match command {
        "auth_list_accounts" => Ok(json!(crate::commands::auth_list_accounts(
            auth_provider,
            copilot,
            codex,
            claude,
            gemini,
            antigravity,
            kiro,
            cursor,
        )
        .await
        .map_err(WebError::internal)?)),
        "auth_get_status" => Ok(json!(crate::commands::auth_get_status(
            auth_provider,
            copilot,
            codex,
            claude,
            gemini,
            antigravity,
            kiro,
            cursor,
        )
        .await
        .map_err(WebError::internal)?)),
        "auth_start_login" => Ok(json!(crate::commands::auth_start_login(
            auth_provider,
            optional_string_arg(&args, "githubDomain"),
            optional_string_arg(&args, "oauthFlowMode"),
            copilot,
            codex,
            claude,
            gemini,
            antigravity,
            kiro,
            cursor,
        )
        .await
        .map_err(WebError::internal)?)),
        "auth_poll_for_account" => Ok(json!(crate::commands::auth_poll_for_account(
            auth_provider,
            string_arg(&args, "deviceCode")?,
            optional_string_arg(&args, "githubDomain"),
            copilot,
            codex,
            claude,
            gemini,
            antigravity,
            kiro,
            cursor,
        )
        .await
        .map_err(WebError::internal)?)),
        "auth_submit_oauth_code" => Ok(json!(crate::commands::auth_submit_oauth_code(
            auth_provider,
            string_arg(&args, "deviceCode")?,
            string_arg(&args, "code")?,
            claude,
        )
        .await
        .map_err(WebError::internal)?)),
        "auth_remove_account" => {
            crate::commands::auth_remove_account(
                auth_provider,
                string_arg(&args, "accountId")?,
                copilot,
                codex,
                claude,
                gemini,
                antigravity,
                kiro,
                cursor,
            )
            .await
            .map_err(WebError::internal)?;
            Ok(json!(null))
        }
        "auth_set_default_account" => {
            crate::commands::auth_set_default_account(
                auth_provider,
                string_arg(&args, "accountId")?,
                copilot,
                codex,
                claude,
                gemini,
                antigravity,
                kiro,
                cursor,
            )
            .await
            .map_err(WebError::internal)?;
            Ok(json!(null))
        }
        "auth_logout" => {
            crate::commands::auth_logout(
                auth_provider,
                copilot,
                codex,
                claude,
                gemini,
                antigravity,
                kiro,
                cursor,
            )
            .await
            .map_err(WebError::internal)?;
            Ok(json!(null))
        }
        _ => Err(WebError::not_found(format!(
            "managed auth web command is not exposed: {command}"
        ))),
    }
}

fn is_local_callback_auth_provider(auth_provider: &str) -> bool {
    matches!(
        auth_provider,
        "claude_oauth" | "google_gemini_oauth" | "antigravity_oauth"
    )
}

fn local_callback_auth_blocked_message() -> &'static str {
    "当前通过 client URL 访问，无法添加需要 localhost 回调的 OAuth 账号。请在 cc-switch 桌面端本机添加该账号后再回到 client URL 使用。Codex/Copilot/Kiro/Cursor 等非 localhost 回调登录不受影响。"
}

async fn copilot_command(
    state: &ProxyState,
    command: &str,
    args: Value,
) -> Result<Value, WebError> {
    let copilot = required_state::<CopilotAuthState>(state, "copilot auth")?;
    match command {
        "copilot_list_accounts" => Ok(json!(crate::commands::copilot_list_accounts(copilot)
            .await
            .map_err(WebError::internal)?)),
        "copilot_get_auth_status" => Ok(json!(crate::commands::copilot_get_auth_status(copilot)
            .await
            .map_err(WebError::internal)?)),
        "copilot_start_device_flow" => Ok(json!(crate::commands::copilot_start_device_flow(
            optional_string_arg(&args, "githubDomain"),
            copilot
        )
        .await
        .map_err(WebError::internal)?)),
        "copilot_poll_for_auth" => Ok(json!(crate::commands::copilot_poll_for_auth(
            string_arg(&args, "deviceCode")?,
            optional_string_arg(&args, "githubDomain"),
            copilot,
        )
        .await
        .map_err(WebError::internal)?)),
        "copilot_poll_for_account" => Ok(json!(crate::commands::copilot_poll_for_account(
            string_arg(&args, "deviceCode")?,
            optional_string_arg(&args, "githubDomain"),
            copilot,
        )
        .await
        .map_err(WebError::internal)?)),
        "copilot_remove_account" => {
            crate::commands::copilot_remove_account(string_arg(&args, "accountId")?, copilot)
                .await
                .map_err(WebError::internal)?;
            Ok(json!(null))
        }
        "copilot_set_default_account" => {
            crate::commands::copilot_set_default_account(string_arg(&args, "accountId")?, copilot)
                .await
                .map_err(WebError::internal)?;
            Ok(json!(null))
        }
        "copilot_logout" => {
            crate::commands::copilot_logout(copilot)
                .await
                .map_err(WebError::internal)?;
            Ok(json!(null))
        }
        "copilot_is_authenticated" => Ok(json!(crate::commands::copilot_is_authenticated(copilot)
            .await
            .map_err(WebError::internal)?)),
        "copilot_get_models" => Ok(json!(crate::commands::copilot_get_models(copilot)
            .await
            .map_err(WebError::internal)?)),
        "copilot_get_models_for_account" => {
            Ok(json!(crate::commands::copilot_get_models_for_account(
                string_arg(&args, "accountId")?,
                copilot
            )
            .await
            .map_err(WebError::internal)?))
        }
        "copilot_get_usage" => Ok(json!(crate::commands::copilot_get_usage(copilot)
            .await
            .map_err(WebError::internal)?)),
        "copilot_get_usage_for_account" => {
            Ok(json!(crate::commands::copilot_get_usage_for_account(
                string_arg(&args, "accountId")?,
                copilot
            )
            .await
            .map_err(WebError::internal)?))
        }
        _ => Err(WebError::not_found(format!(
            "copilot web command is not exposed: {command}"
        ))),
    }
}

async fn deepseek_command(
    state: &ProxyState,
    command: &str,
    args: Value,
) -> Result<Value, WebError> {
    let deepseek = required_state::<DeepSeekAccountState>(state, "deepseek account")?;
    match command {
        "deepseek_account_list" => Ok(json!(crate::commands::deepseek_account_list(deepseek)
            .await
            .map_err(WebError::internal)?)),
        "deepseek_account_status" => Ok(json!(crate::commands::deepseek_account_status(deepseek)
            .await
            .map_err(WebError::internal)?)),
        "deepseek_account_add" => Ok(json!(crate::commands::deepseek_account_add(
            optional_string_arg(&args, "email"),
            optional_string_arg(&args, "mobile"),
            string_arg(&args, "password")?,
            deepseek,
        )
        .await
        .map_err(WebError::internal)?)),
        "deepseek_account_remove" => {
            crate::commands::deepseek_account_remove(string_arg(&args, "accountId")?, deepseek)
                .await
                .map_err(WebError::internal)?;
            Ok(json!(null))
        }
        "deepseek_account_set_default" => {
            crate::commands::deepseek_account_set_default(
                string_arg(&args, "accountId")?,
                deepseek,
            )
            .await
            .map_err(WebError::internal)?;
            Ok(json!(null))
        }
        _ => Err(WebError::not_found(format!(
            "deepseek web command is not exposed: {command}"
        ))),
    }
}

async fn oauth_quota_command(
    state: &ProxyState,
    command: &str,
    args: Value,
) -> Result<Value, WebError> {
    let quota = required_state::<OauthQuotaState>(state, "oauth quota")?;
    let codex = required_state::<CodexOAuthState>(state, "codex oauth")?;
    let claude = required_state::<ClaudeOAuthState>(state, "claude oauth")?;
    let gemini = required_state::<GeminiOAuthState>(state, "gemini oauth")?;
    let copilot = required_state::<CopilotAuthState>(state, "copilot auth")?;
    let kiro = required_state::<KiroOAuthState>(state, "kiro oauth")?;
    let antigravity = required_state::<AntigravityOAuthState>(state, "antigravity oauth")?;
    let cursor = required_state::<CursorOAuthState>(state, "cursor oauth")?;

    match command {
        "get_cached_oauth_quota" => Ok(json!(crate::commands::get_cached_oauth_quota(
            string_arg(&args, "authProvider")?,
            optional_string_arg(&args, "accountId"),
            quota,
            codex,
            claude,
            gemini,
            copilot,
            kiro,
            antigravity,
            cursor,
        )
        .await
        .map_err(WebError::internal)?)),
        "refresh_oauth_quota" => Ok(json!(crate::commands::refresh_oauth_quota(
            string_arg(&args, "authProvider")?,
            optional_string_arg(&args, "accountId"),
            quota,
            codex,
            claude,
            gemini,
            copilot,
            kiro,
            antigravity,
            cursor,
        )
        .await
        .map_err(WebError::internal)?)),
        "get_claude_oauth_quota" => Ok(json!(crate::commands::get_claude_oauth_quota(
            optional_string_arg(&args, "accountId"),
            claude
        )
        .await
        .map_err(WebError::internal)?)),
        "get_codex_oauth_quota" => Ok(json!(crate::commands::get_codex_oauth_quota(
            optional_string_arg(&args, "accountId"),
            codex
        )
        .await
        .map_err(WebError::internal)?)),
        _ => Err(WebError::not_found(format!(
            "oauth quota web command is not exposed: {command}"
        ))),
    }
}

async fn subscription_command(
    state: &ProxyState,
    command: &str,
    args: Value,
) -> Result<Value, WebError> {
    match command {
        "get_subscription_quota" => {
            let app = required_app_handle(state)?.clone();
            let app_state = required_app_state(state)?;
            Ok(json!(crate::commands::get_subscription_quota(
                app,
                app_state,
                string_arg(&args, "tool")?
            )
            .await
            .map_err(WebError::internal)?))
        }
        "get_coding_plan_quota" => Ok(json!(crate::services::coding_plan::get_coding_plan_quota(
            &string_arg(&args, "baseUrl")?,
            &string_arg(&args, "apiKey")?,
        )
        .await
        .map_err(WebError::internal)?)),
        "get_balance" => Ok(json!(crate::services::balance::get_balance(
            &string_arg(&args, "baseUrl")?,
            &string_arg(&args, "apiKey")?,
        )
        .await
        .map_err(WebError::internal)?)),
        _ => Err(WebError::not_found(format!(
            "subscription web command is not exposed: {command}"
        ))),
    }
}

async fn client_tunnel_status(state: &ProxyState) -> ShareTunnelStatus {
    let Some(app_state) = app_state(state) else {
        return ShareTunnelStatus {
            info: None,
            last_error: None,
            requires_owner_login: false,
        };
    };
    let mgr = app_state.tunnel_manager.read().await;
    ShareTunnelStatus {
        info: mgr.get_info("__client_web__"),
        last_error: mgr.get_last_error("__client_web__"),
        requires_owner_login: false,
    }
}

async fn client_tunnel_projection(state: &ProxyState) -> Value {
    let settings = crate::settings::get_settings();
    let config = settings.client_tunnel.map(|config| {
        json!({
            "ownerEmail": config.owner_email,
            "subdomain": config.subdomain,
            "enabled": config.enabled,
            "autoStart": config.auto_start,
            "tunnelUrl": config.tunnel_url,
        })
    });
    json!({
        "config": config,
        "status": client_tunnel_status(state).await,
    })
}

fn string_arg(args: &Value, key: &str) -> Result<String, WebError> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| WebError::bad_request(format!("{key} is required")))
}

fn is_local_admin_command_allowed(command: &str) -> bool {
    matches!(
        command,
        "get_settings"
            | "save_settings"
            | "get_build_info"
            | "get_proxy_status"
            | "get_proxy_takeover_status"
            | "get_proxy_config"
            | "update_proxy_config"
            | "get_global_proxy_config"
            | "update_global_proxy_config"
            | "get_proxy_config_for_app"
            | "update_proxy_config_for_app"
            | "start_proxy_server"
            | "stop_proxy_server"
            | "stop_proxy_with_restore"
            | "is_proxy_running"
            | "is_live_takeover_active"
            | "set_proxy_takeover_for_app"
            | "switch_proxy_provider"
            | "get_providers"
            | "get_current_provider"
            | "add_provider"
            | "update_provider"
            | "delete_provider"
            | "switch_provider"
            | "update_providers_sort_order"
            | "get_universal_providers"
            | "get_universal_provider"
            | "upsert_universal_provider"
            | "delete_universal_provider"
            | "sync_universal_provider"
            | "create_share"
            | "list_shares"
            | "get_share_detail"
            | "enable_share"
            | "disable_share"
            | "pause_share"
            | "resume_share"
            | "delete_share"
            | "reset_share_usage"
            | "update_share_token_limit"
            | "update_share_parallel_limit"
            | "update_share_description"
            | "update_share_for_sale"
            | "update_share_for_sale_official_price_percent"
            | "update_share_expiration"
            | "update_share_auto_start"
            | "update_share_owner_email"
            | "transfer_share_owner"
            | "update_share_provider_binding"
            | "update_share_acl"
            | "update_share_subdomain"
            | "list_share_binding_history"
            | "list_share_markets"
            | "get_tunnel_status"
            | "start_share_tunnel"
            | "stop_share_tunnel"
            | "get_share_connect_info"
            | "configure_tunnel"
            | "get_client_tunnel"
            | "claim_client_tunnel"
            | "update_client_tunnel"
            | "start_client_tunnel"
            | "stop_client_tunnel"
            | "get_client_tunnel_status"
            | "get_failover_queue"
            | "get_available_providers_for_failover"
            | "add_to_failover_queue"
            | "remove_from_failover_queue"
            | "get_auto_failover_enabled"
            | "set_auto_failover_enabled"
            | "get_provider_health"
            | "reset_circuit_breaker"
            | "get_circuit_breaker_config"
            | "update_circuit_breaker_config"
            | "get_circuit_breaker_stats"
            | "stream_check_provider"
            | "stream_check_all_providers"
            | "get_stream_check_config"
            | "save_stream_check_config"
            | "fetch_models_for_config"
            | "get_codex_oauth_models"
            | "get_antigravity_oauth_models"
            | "read_live_provider_settings"
            | "test_api_endpoints"
            | "get_custom_endpoints"
            | "add_custom_endpoint"
            | "remove_custom_endpoint"
            | "update_endpoint_last_used"
            | "get_claude_common_config_snippet"
            | "set_claude_common_config_snippet"
            | "get_common_config_snippet"
            | "set_common_config_snippet"
            | "extract_common_config_snippet"
            | "auth_start_login"
            | "auth_submit_oauth_code"
            | "auth_poll_for_account"
            | "auth_list_accounts"
            | "auth_get_status"
            | "auth_remove_account"
            | "auth_set_default_account"
            | "auth_logout"
            | "copilot_start_device_flow"
            | "copilot_poll_for_auth"
            | "copilot_poll_for_account"
            | "copilot_list_accounts"
            | "copilot_get_auth_status"
            | "copilot_remove_account"
            | "copilot_set_default_account"
            | "copilot_logout"
            | "copilot_is_authenticated"
            | "copilot_get_models"
            | "copilot_get_models_for_account"
            | "copilot_get_usage"
            | "copilot_get_usage_for_account"
            | "deepseek_account_add"
            | "deepseek_account_list"
            | "deepseek_account_status"
            | "deepseek_account_remove"
            | "deepseek_account_set_default"
            | "get_subscription_quota"
            | "get_claude_oauth_quota"
            | "get_codex_oauth_quota"
            | "get_cached_oauth_quota"
            | "refresh_oauth_quota"
            | "get_coding_plan_quota"
            | "get_balance"
            | "queryProviderUsage"
            | "testUsageScript"
            | "get_usage_summary"
            | "get_usage_summary_by_app"
            | "get_usage_trends"
            | "get_provider_stats"
            | "get_model_stats"
            | "get_request_logs"
            | "get_request_detail"
            | "get_model_pricing"
            | "update_model_pricing"
            | "delete_model_pricing"
            | "check_provider_limits"
            | "sync_session_usage"
            | "get_usage_data_sources"
            | "check_env_conflicts"
            | "delete_env_vars"
            | "restore_env_backup"
    )
}

fn share_connect_info(share: &crate::database::ShareRecord) -> Value {
    let config = current_tunnel_config();
    let subdomain = share
        .subdomain
        .clone()
        .unwrap_or_else(|| format!("share-{}", &share.id[..8]));
    json!({
        "tunnelUrl": share.tunnel_url.clone().unwrap_or_else(|| config.get_tunnel_addr(&subdomain)),
        "subdomain": subdomain,
    })
}

fn current_tunnel_config() -> crate::tunnel::config::TunnelConfig {
    crate::tunnel::config::current_tunnel_config()
        .unwrap_or_else(crate::tunnel::config::TunnelConfig::default_public_service)
}

fn share_settings_projection() -> Value {
    let settings = crate::settings::get_settings_for_frontend();
    json!({
        "shareRouterDomain": settings.current_share_router_domain(),
    })
}

fn sanitize_asset_path(raw: &str) -> Option<PathBuf> {
    let mut output = PathBuf::new();
    for component in Path::new(raw).components() {
        match component {
            Component::Normal(part) => output.push(part),
            _ => return None,
        }
    }
    if output.as_os_str().is_empty() {
        None
    } else {
        Some(PathBuf::from("assets").join(output))
    }
}

async fn serve_dist_file(state: &ProxyState, headers: &HeaderMap, path: &Path) -> Response {
    let Some(root) = dist_root(state) else {
        return error_response(StatusCode::NOT_FOUND, "web dist directory not found");
    };
    let (disk_path, served_path, encoding) = encoded_dist_path(headers, &root, path)
        .unwrap_or_else(|| (root.join(path), path.to_path_buf(), None));
    let read_result = match tokio::fs::read(&disk_path).await {
        Ok(bytes) => Ok((bytes, served_path, encoding)),
        Err(_) if path != Path::new(INDEX_HTML) => {
            let index_path = PathBuf::from(INDEX_HTML);
            let (fallback_disk_path, fallback_served_path, fallback_encoding) =
                encoded_dist_path(headers, &root, &index_path)
                    .unwrap_or_else(|| (root.join(INDEX_HTML), index_path, None));
            tokio::fs::read(fallback_disk_path)
                .await
                .map(|bytes| (bytes, fallback_served_path, fallback_encoding))
        }
        Err(err) => Err(err),
    };

    match read_result {
        Ok((bytes, served_path, response_encoding)) => {
            let content_type = content_type_for(&served_path);
            let mut response = bytes.into_response();
            response
                .headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
            if let Some(encoding) = response_encoding {
                response
                    .headers_mut()
                    .insert(header::CONTENT_ENCODING, HeaderValue::from_static(encoding));
            }
            response
                .headers_mut()
                .insert(header::VARY, HeaderValue::from_static("Accept-Encoding"));
            response
        }
        Err(_) => error_response(StatusCode::NOT_FOUND, "web asset not found"),
    }
}

fn encoded_dist_path(
    headers: &HeaderMap,
    root: &Path,
    path: &Path,
) -> Option<(PathBuf, PathBuf, Option<&'static str>)> {
    let accept_encoding = headers
        .get(header::ACCEPT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    for (token, suffix) in [("br", "br"), ("gzip", "gz")] {
        if !accept_encoding
            .split(',')
            .any(|part| part.trim().starts_with(token))
        {
            continue;
        }
        let encoded_path = root.join(format!("{}.{}", path.display(), suffix));
        if encoded_path.exists() {
            return Some((encoded_path, path.to_path_buf(), Some(token)));
        }
    }
    None
}

fn dist_root(state: &ProxyState) -> Option<PathBuf> {
    if let Some(resource_dist) = state
        .app_handle
        .as_ref()
        .and_then(|app| app.path().resource_dir().ok())
        .map(|resource_dir| resource_dir.join("dist"))
        .filter(|path| path.join(INDEX_HTML).exists())
    {
        return Some(resource_dist);
    }

    let manifest_dist = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../dist");
    if manifest_dist.join(INDEX_HTML).exists() {
        return Some(manifest_dist);
    }
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    for candidate in [
        exe_dir.join("dist"),
        exe_dir.join("../dist"),
        exe_dir.join("resources/dist"),
    ] {
        if candidate.join(INDEX_HTML).exists() {
            return Some(candidate);
        }
    }
    None
}

fn content_type_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
    {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

fn error_response(status: StatusCode, message: &str) -> Response {
    (
        status,
        Json(json!({
            "ok": false,
            "error": message,
        })),
    )
        .into_response()
}
