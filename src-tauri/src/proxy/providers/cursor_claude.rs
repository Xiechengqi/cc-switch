//! Cursor OAuth provider for Claude-compatible proxy requests.

use crate::commands::{CursorOAuthState, CursorSessionState};
use crate::provider::Provider;
use crate::proxy::hyper_client::ProxyResponse;
use crate::proxy::ProxyError;
use bytes::Bytes;
use http::HeaderMap;
use http::StatusCode;
use serde_json::{json, Value};
use tauri::Manager;

use super::cursor_agent_service::{run_agent, AgentRunOptions};
use super::cursor_protocol::{
    conversation_id_from_headers, requested_model, response_error_body, response_to_json,
    response_to_sse_stream, send_cursor_request, CursorRequestContext, CursorResponseFormat,
};
use super::cursor_request_builder::{build_plan, validate_tool_result_context, InboundProtocol, ToolResultBlock};
use super::cursor_router::{select_protocol, select_tool_mode, CursorProtocol, CursorToolMode};
use super::cursor_session::CursorSessionManager;

pub async fn forward_cursor_claude(
    app_handle: Option<&tauri::AppHandle>,
    provider: &Provider,
    headers: Option<&HeaderMap>,
    body: &Value,
) -> Result<(ProxyResponse, String), ProxyError> {
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

    let (mapped_body, response_model, upstream_model) =
        prepare_cursor_oauth_claude_body(provider, body);

    let protocol = select_protocol(provider, InboundProtocol::AnthropicMessages, &mapped_body);
    log::debug!(
        "[CursorOAuth] 选定协议: {:?}（model={}）",
        protocol,
        upstream_model
    );

    match protocol {
        CursorProtocol::AgentService => {
            let session_state = app_handle.state::<CursorSessionState>();
            let mut plan = build_plan(InboundProtocol::AnthropicMessages, &mapped_body);
            if let Err(msg) = validate_tool_result_context(&plan) {
                return Err(ProxyError::InvalidRequest(msg));
            }
            let session_key =
                derive_session_key(headers, &mapped_body, &session_state.0, &plan.tool_results)
                    .await;
            if matches!(select_tool_mode(provider), CursorToolMode::Disabled) {
                plan.tools.clear();
            }
            let is_stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
            let response = run_agent(AgentRunOptions {
                account: &resolved_account,
                access_token: &token,
                session_manager: &session_state.0,
                session_key,
                plan,
                format: CursorResponseFormat::AnthropicMessages,
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
                body: normalize_stream_body(&mapped_body),
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
                    CursorResponseFormat::AnthropicMessages,
                );
                Ok((ProxyResponse::local_sse(Box::pin(stream)), upstream_model))
            } else {
                let (_, bytes) = response_to_json(
                    response,
                    &response_model,
                    &mapped_body,
                    CursorResponseFormat::AnthropicMessages,
                )
                .await?;
                Ok((
                    ProxyResponse::local_json(StatusCode::OK, bytes),
                    upstream_model,
                ))
            }
        }
    }
}

fn normalize_stream_body(body: &Value) -> Value {
    let mut next = body.clone();
    if next
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        next["stream"] = json!(true);
    }
    next
}

fn prepare_cursor_oauth_claude_body(provider: &Provider, body: &Value) -> (Value, String, String) {
    let response_model = requested_model(body);
    let (mapped_body, original_model, mapped_model) =
        crate::proxy::model_mapper::apply_model_mapping(body.clone(), provider);
    if let (Some(original), Some(mapped)) = (original_model.as_deref(), mapped_model.as_deref()) {
        log::debug!("[CursorOAuth] Claude 模型映射: {original} -> {mapped}");
    }
    let mapped_body =
        crate::proxy::model_mapper::strip_one_m_suffix_for_upstream_from_body(mapped_body);
    let upstream_model = requested_model(&mapped_body);
    (mapped_body, response_model, upstream_model)
}

/// Derive a stable session key for AgentService routing. Priority:
///   1. `x-session-affinity` header
///   2. `x-opencode-session-id` / `x-opencode-session`
///   3. pending tool_call_id index
///   4. body-level `metadata.conversation_id` (Claude Code passes this)
///   5. fresh fallback id (avoids concurrent no-header sessions colliding)
async fn derive_session_key(
    headers: Option<&HeaderMap>,
    body: &Value,
    session_manager: &CursorSessionManager,
    tool_results: &[ToolResultBlock],
) -> String {
    if let Some(s) = conversation_id_from_headers(headers) {
        return s;
    }
    for tr in tool_results {
        if let Some(s) = session_manager.resolve_tool_call_id(&tr.tool_call_id).await {
            return s;
        }
    }
    if let Some(meta) = body.get("metadata").and_then(Value::as_object) {
        if let Some(id) = meta.get("conversation_id").and_then(Value::as_str) {
            if !id.is_empty() {
                return crate::proxy::providers::cursor_protocol::stable_uuid_like(id);
            }
        }
    }
    uuid::Uuid::new_v4().to_string()
}

#[allow(dead_code)]
fn json_bytes(value: &Value) -> Bytes {
    Bytes::from(serde_json::to_vec(value).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(settings_config: Value) -> Provider {
        Provider::with_id(
            "cursor-oauth-test".to_string(),
            "Cursor OAuth".to_string(),
            settings_config,
            Some("https://cursor.com".to_string()),
        )
    }

    #[test]
    fn claude_body_uses_provider_mapping_but_keeps_response_model() {
        let provider = provider(json!({
            "modelMapping": {
                "mode": "single",
                "upstreamModel": "composer-2.5"
            }
        }));
        let (mapped_body, response_model, upstream_model) = prepare_cursor_oauth_claude_body(
            &provider,
            &json!({
                "model": "claude-opus-4-7",
                "messages": []
            }),
        );

        assert_eq!(mapped_body["model"], json!("composer-2.5"));
        assert_eq!(response_model, "claude-opus-4-7");
        assert_eq!(upstream_model, "composer-2.5");
    }

    #[test]
    fn claude_body_strips_mapped_one_m_marker_before_cursor() {
        let provider = provider(json!({
            "modelMapping": {
                "mode": "single",
                "upstreamModel": "composer-2.5 [1M]"
            }
        }));
        let (mapped_body, response_model, upstream_model) = prepare_cursor_oauth_claude_body(
            &provider,
            &json!({
                "model": "claude-sonnet-4-5[1m]",
                "messages": []
            }),
        );

        assert_eq!(mapped_body["model"], json!("composer-2.5"));
        assert_eq!(response_model, "claude-sonnet-4-5[1m]");
        assert_eq!(upstream_model, "composer-2.5");
    }
}
