//! Cursor OAuth provider for Codex/OpenAI-compatible proxy requests.

use crate::commands::{CursorOAuthState, CursorSessionState};
use crate::provider::Provider;
use crate::proxy::hyper_client::ProxyResponse;
use crate::proxy::ProxyError;
use http::HeaderMap;
use http::StatusCode;
use serde_json::Value;
use tauri::Manager;

use super::cursor_agent_service::{run_agent, AgentRunOptions};
use super::cursor_protocol::{
    conversation_id_from_headers, prepare_cursor_codex_body, response_error_body, response_to_json,
    response_to_sse_stream, send_cursor_request, CursorRequestContext, CursorResponseFormat,
};
use super::cursor_request_builder::{build_plan, InboundProtocol, ToolResultBlock};
use super::cursor_router::{select_protocol, select_tool_mode, CursorProtocol, CursorToolMode};
use super::cursor_session::CursorSessionManager;

pub async fn forward_cursor_codex(
    app_handle: Option<&tauri::AppHandle>,
    provider: &Provider,
    headers: Option<&HeaderMap>,
    endpoint: &str,
    body: &Value,
) -> Result<(ProxyResponse, String), ProxyError> {
    let (mapped_body, response_model, upstream_model) = prepare_cursor_codex_body(provider, body);

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

    let (response_format, inbound_protocol) = if endpoint.contains("/chat/completions") {
        (
            CursorResponseFormat::OpenAiChatCompletions,
            InboundProtocol::OpenAiChat,
        )
    } else {
        (
            CursorResponseFormat::OpenAiResponses,
            InboundProtocol::OpenAiResponses,
        )
    };

    let protocol = select_protocol(provider, inbound_protocol, &mapped_body);
    log::debug!(
        "[CursorOAuth] Codex 选定协议: {:?}（model={}, endpoint={}）",
        protocol,
        upstream_model,
        endpoint
    );

    match protocol {
        CursorProtocol::AgentService => {
            let session_state = app_handle.state::<CursorSessionState>();
            let mut plan = build_plan(inbound_protocol, &mapped_body);
            let session_key = derive_session_key(
                headers,
                &session_state.0,
                &plan.tool_results,
                plan.previous_response_id.as_deref(),
            )
            .await;
            if matches!(select_tool_mode(provider), CursorToolMode::Disabled) {
                plan.tools.clear();
            }
            let is_stream = body
                .get("stream")
                .and_then(Value::as_bool)
                .unwrap_or(matches!(inbound_protocol, InboundProtocol::OpenAiResponses));
            let response = run_agent(AgentRunOptions {
                account: &resolved_account,
                access_token: &token,
                session_manager: &session_state.0,
                session_key,
                plan,
                format: response_format,
                response_model,
                stream: is_stream,
            })
            .await?;
            Ok((response, upstream_model))
        }
        CursorProtocol::ChatService => {
            let ctx = CursorRequestContext {
                account: resolved_account.clone(),
                access_token: token,
                body: normalize_cursor_body(&mapped_body),
                conversation_id: conversation_id_from_headers(headers),
            };
            let response = send_cursor_request(&ctx).await?;
            let response = if response.status() == StatusCode::UNAUTHORIZED {
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
                let body = response_error_body(response).await;
                return Err(ProxyError::UpstreamError { status, body });
            }

            let is_stream = body
                .get("stream")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if is_stream {
                let stream = response_to_sse_stream(
                    response,
                    response_model,
                    mapped_body.clone(),
                    response_format,
                );
                Ok((ProxyResponse::local_sse(Box::pin(stream)), upstream_model))
            } else {
                let (_, bytes) =
                    response_to_json(response, &response_model, &mapped_body, response_format)
                        .await?;
                Ok((
                    ProxyResponse::local_json(StatusCode::OK, bytes),
                    upstream_model,
                ))
            }
        }
    }
}

fn normalize_cursor_body(body: &Value) -> Value {
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

async fn derive_session_key(
    headers: Option<&HeaderMap>,
    session_manager: &CursorSessionManager,
    tool_results: &[ToolResultBlock],
    previous_response_id: Option<&str>,
) -> String {
    if let Some(s) = conversation_id_from_headers(headers) {
        return s;
    }
    for tr in tool_results {
        if let Some(s) = session_manager.resolve_tool_call_id(&tr.tool_call_id).await {
            return s;
        }
    }
    if let Some(prev) = previous_response_id {
        if !prev.is_empty() {
            if let Some(s) = session_manager.resolve_response_id(prev).await {
                return s;
            }
            return crate::proxy::providers::cursor_protocol::stable_uuid_like(prev);
        }
    }
    uuid::Uuid::new_v4().to_string()
}
