use std::path::{Component, Path, PathBuf};

use axum::{
    body::Bytes,
    extract::{Path as AxumPath, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};
use tauri::Manager;

use crate::{
    error::AppError,
    proxy::{server::ProxyState, types::ProxyStatus},
    services::share::ShareService,
    store::AppState,
    tunnel::config::ShareTunnelStatus,
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
        "get_proxy_takeover_status" => Ok(json!({
            "claude": false,
            "codex": false,
            "gemini": false,
            "opencode": false,
            "openclaw": false,
            "hermes": false,
        })),
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
        "get_providers" => Ok(json!({ "providers": {}, "currentProviderId": null })),
        "get_current_provider" => Ok(json!(null)),
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
            | "start_proxy_server"
            | "stop_proxy_server"
            | "is_proxy_running"
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
    )
}

fn share_connect_info(share: &crate::database::ShareRecord) -> Value {
    let config = crate::tunnel::config::current_tunnel_config()
        .unwrap_or_else(crate::tunnel::config::TunnelConfig::default_public_service);
    let subdomain = share
        .subdomain
        .clone()
        .unwrap_or_else(|| format!("share-{}", &share.id[..8]));
    json!({
        "tunnelUrl": share.tunnel_url.clone().unwrap_or_else(|| config.get_tunnel_addr(&subdomain)),
        "subdomain": subdomain,
    })
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
