//! Cursor OAuth provider for Claude-compatible proxy requests.

use crate::commands::CursorOAuthState;
use crate::provider::Provider;
use crate::proxy::hyper_client::ProxyResponse;
use crate::proxy::ProxyError;
use bytes::Bytes;
use http::StatusCode;
use serde_json::Value;
use tauri::Manager;

use super::cursor_protocol::{
    requested_model, response_to_json, response_to_sse_stream, send_cursor_request,
    CursorRequestContext, CursorResponseFormat,
};

pub async fn forward_cursor_claude(
    app_handle: Option<&tauri::AppHandle>,
    provider: &Provider,
    body: &Value,
) -> Result<ProxyResponse, ProxyError> {
    let Some(app_handle) = app_handle else {
        return Err(ProxyError::AuthError(
            "Cursor OAuth 认证不可用（无 AppHandle）".to_string(),
        ));
    };

    let state = app_handle.state::<CursorOAuthState>();
    let manager = state.0.read().await;
    let account_id = provider
        .meta
        .as_ref()
        .and_then(|m| m.managed_account_id_for("cursor_oauth"));

    let resolved_account = match account_id.as_deref() {
        Some(id) => manager.get_account(id).await,
        None => manager.get_default_account().await,
    }
    .ok_or_else(|| ProxyError::AuthError("未找到可用 Cursor OAuth 账号".to_string()))?;

    let token = match account_id.as_deref() {
        Some(id) => manager.get_valid_token_for_account(id).await,
        None => manager.get_valid_token().await,
    }
    .map_err(|e| ProxyError::AuthError(format!("Cursor OAuth 认证失败: {e}")))?;

    let ctx = CursorRequestContext {
        account: resolved_account.clone(),
        access_token: token,
        body: normalize_stream_body(body),
    };
    let response = send_cursor_request(&ctx).await?;
    let response = if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        manager
            .invalidate_cached_token(&resolved_account.account_id)
            .await;
        let token = manager
            .get_valid_token_for_account(&resolved_account.account_id)
            .await
            .map_err(|e| ProxyError::AuthError(format!("Cursor OAuth 认证失败: {e}")))?;
        send_cursor_request(&CursorRequestContext {
            access_token: token,
            ..ctx
        })
        .await?
    } else {
        response
    };

    if !response.status().is_success() {
        let status = response.status().as_u16();
        let body = response.text().await.ok();
        return Err(ProxyError::UpstreamError { status, body });
    }

    let model = requested_model(body);
    let is_stream = body
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if is_stream {
        let stream =
            response_to_sse_stream(response, model, CursorResponseFormat::AnthropicMessages);
        Ok(ProxyResponse::local_sse(Box::pin(stream)))
    } else {
        let (_, bytes) =
            response_to_json(response, &model, CursorResponseFormat::AnthropicMessages).await?;
        Ok(ProxyResponse::local_json(StatusCode::OK, bytes))
    }
}

fn normalize_stream_body(body: &Value) -> Value {
    let mut next = body.clone();
    if next
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        next["stream"] = serde_json::json!(true);
    }
    next
}

#[allow(dead_code)]
fn json_bytes(value: &Value) -> Bytes {
    Bytes::from(serde_json::to_vec(value).unwrap_or_default())
}
