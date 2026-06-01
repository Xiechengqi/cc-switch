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
        Ok(WebScope::Share(scope)) => Json(json!({
            "mode": "share",
            "shareId": scope.share.id,
            "shareName": scope.share.name,
            "subdomain": scope.share.subdomain,
            "appType": scope.share.app_type,
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
        Ok(WebScope::Share(scope)) => {
            match invoke_share_scoped(&state, scope, &command, args).await {
                Ok(value) => Json(value).into_response(),
                Err(err) => error_response(err.status, &err.message),
            }
        }
        Err(err) => error_response(err.status, &err.message),
    }
}

pub async fn serve_index() -> Response {
    serve_dist_file(Path::new(INDEX_HTML)).await
}

pub async fn serve_favicon() -> Response {
    serve_dist_file(Path::new("favicon.ico")).await
}

pub async fn serve_asset(AxumPath(path): AxumPath<String>) -> Response {
    let Some(path) = sanitize_asset_path(&path) else {
        return error_response(StatusCode::BAD_REQUEST, "invalid asset path");
    };
    serve_dist_file(&path).await
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

enum WebScope {
    Share(ShareScope),
}

fn resolve_scope(state: &ProxyState, headers: &HeaderMap) -> Result<WebScope, WebError> {
    let share_token = headers
        .get("x-share-token")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let Some(share_token) = share_token else {
        return Err(WebError::unauthorized("share token missing"));
    };

    let validation = ShareService::validate_token_with_reason(&state.db, share_token)?
        .ok_or_else(|| WebError::unauthorized("share token invalid"))?;
    let Some(share) = validation.share else {
        return Err(WebError::unauthorized(
            validation
                .message
                .unwrap_or_else(|| "share token invalid".to_string()),
        ));
    };
    if let Some(header_share_id) = headers
        .get("x-cc-switch-share-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if header_share_id != share.id {
            return Err(WebError::unauthorized("share id does not match token"));
        }
    }
    Ok(WebScope::Share(ShareScope { share }))
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
        "get_current_provider" => Ok(json!(scope.share.provider_id.clone().unwrap_or_default())),
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
    share.share_token.clear();
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

async fn serve_dist_file(path: &Path) -> Response {
    let Some(root) = dist_root() else {
        return error_response(StatusCode::NOT_FOUND, "web dist directory not found");
    };
    let full_path = root.join(path);
    let read_result = match tokio::fs::read(&full_path).await {
        Ok(bytes) => Ok((bytes, path.to_path_buf())),
        Err(_) if path != Path::new(INDEX_HTML) => {
            let index_path = PathBuf::from(INDEX_HTML);
            tokio::fs::read(root.join(INDEX_HTML))
                .await
                .map(|bytes| (bytes, index_path))
        }
        Err(err) => Err(err),
    };

    match read_result {
        Ok((bytes, served_path)) => {
            let content_type = content_type_for(&served_path);
            let mut response = bytes.into_response();
            response
                .headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
            response
        }
        Err(_) => error_response(StatusCode::NOT_FOUND, "web asset not found"),
    }
}

fn dist_root() -> Option<PathBuf> {
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
