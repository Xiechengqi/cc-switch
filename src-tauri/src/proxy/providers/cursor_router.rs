//! Protocol selection between Cursor's legacy `ChatService/StreamUnifiedChatWithTools`
//! (text-only) and the newer `agent.v1.AgentService/Run` (tools + KV + MCP).
//!
//! The router exposes [`select_protocol`], a pure function over the request
//! body + provider settings + protocol shape. It returns `CursorProtocol`:
//!
//! * `ChatService` — legacy text path; only used when provider sets
//!   `cursor_protocol: chat_service` explicitly.
//! * `AgentService` — default for `auto`; tools, images, Responses, Claude/Codex.
//!
//! Provider settings may override the choice:
//!
//! ```json
//! { "settingsConfig": { "cursor_protocol": "auto|chat_service|agent_service",
//!                       "cursor_tool_mode": "disabled|agent_mcp" } }
//! ```

use super::cursor_debug;
use crate::provider::Provider;
use serde_json::Value;

use super::cursor_request_builder::InboundProtocol;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorProtocol {
    /// Legacy `aiserver.v1.ChatService/StreamUnifiedChatWithTools`.
    ChatService,
    /// New `agent.v1.AgentService/Run`.
    AgentService,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorToolMode {
    /// Strip tools entirely. Cursor sees a text-only turn.
    Disabled,
    /// Default: surface tools via Agent's `mcp_tools` + reject built-ins.
    AgentMcp,
}

pub fn select_protocol(
    provider: &Provider,
    protocol: InboundProtocol,
    body: &Value,
) -> CursorProtocol {
    // Provider explicit override wins.
    if let Some(forced) = provider_cursor_protocol(provider) {
        cursor_debug::log_protocol_choice(
            match forced {
                CursorProtocol::ChatService => "chat_service",
                CursorProtocol::AgentService => "agent_service",
            },
            protocol_label(protocol),
            "provider_override",
        );
        return forced;
    }

    // Composer models are unreliable on legacy ChatService — always use AgentService.
    if is_composer_model(body) {
        cursor_debug::log_protocol_choice("agent_service", protocol_label(protocol), "composer_model");
        return CursorProtocol::AgentService;
    }

    // Claude Code always uses AgentService (OmniRoute default; avoids ChatService drift).
    if matches!(protocol, InboundProtocol::AnthropicMessages) {
        cursor_debug::log_protocol_choice("agent_service", protocol_label(protocol), "anthropic");
        return CursorProtocol::AgentService;
    }

    // Codex CLI primary path is OpenAI Responses — always AgentService.
    if matches!(protocol, InboundProtocol::OpenAiResponses) {
        cursor_debug::log_protocol_choice("agent_service", protocol_label(protocol), "openai_responses");
        return CursorProtocol::AgentService;
    }

    if has_openai_tools_or_results(body)
        || body
            .get("previous_response_id")
            .and_then(Value::as_str)
            .is_some()
        || has_images(body)
    {
        cursor_debug::log_protocol_choice("agent_service", protocol_label(protocol), "tools_or_images");
        return CursorProtocol::AgentService;
    }

    // `auto` default: AgentService (ChatService only via explicit provider override).
    cursor_debug::log_protocol_choice("agent_service", protocol_label(protocol), "auto_default");
    CursorProtocol::AgentService
}

pub fn select_tool_mode(provider: &Provider) -> CursorToolMode {
    provider_cursor_tool_mode(provider).unwrap_or(CursorToolMode::AgentMcp)
}

fn provider_cursor_protocol(provider: &Provider) -> Option<CursorProtocol> {
    let cfg = provider_settings(provider)?;
    let raw = cfg.get("cursor_protocol")?.as_str()?;
    match raw.trim().to_ascii_lowercase().as_str() {
        "auto" | "" => None,
        "chat_service" | "chat-service" | "chatservice" | "legacy" => {
            Some(CursorProtocol::ChatService)
        }
        "agent_service" | "agent-service" | "agentservice" | "agent" => {
            Some(CursorProtocol::AgentService)
        }
        _ => None,
    }
}

fn provider_cursor_tool_mode(provider: &Provider) -> Option<CursorToolMode> {
    let cfg = provider_settings(provider)?;
    let raw = cfg.get("cursor_tool_mode")?.as_str()?;
    match raw.trim().to_ascii_lowercase().as_str() {
        "disabled" | "off" | "none" => Some(CursorToolMode::Disabled),
        "agent_mcp" | "mcp" | "default" => Some(CursorToolMode::AgentMcp),
        _ => None,
    }
}

fn provider_settings(provider: &Provider) -> Option<&Value> {
    if provider.settings_config.is_null() {
        None
    } else {
        Some(&provider.settings_config)
    }
}

fn is_composer_model(body: &Value) -> bool {
    body.get("model")
        .and_then(Value::as_str)
        .map(|m| {
            let lower = m.to_ascii_lowercase();
            lower.starts_with("composer") || lower.contains("composer-")
        })
        .unwrap_or(false)
}

fn protocol_label(protocol: InboundProtocol) -> &'static str {
    match protocol {
        InboundProtocol::AnthropicMessages => "anthropic_messages",
        InboundProtocol::OpenAiChat => "openai_chat",
        InboundProtocol::OpenAiResponses => "openai_responses",
    }
}

fn has_openai_tools_or_results(body: &Value) -> bool {
    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        if !tools.is_empty() {
            return true;
        }
    }
    // Chat: role:"tool" message OR assistant.tool_calls
    if let Some(messages) = body.get("messages").and_then(Value::as_array) {
        for m in messages {
            let role = m.get("role").and_then(Value::as_str).unwrap_or("");
            if role == "tool" {
                return true;
            }
            if m.get("tool_calls")
                .and_then(Value::as_array)
                .map_or(false, |a| !a.is_empty())
            {
                return true;
            }
        }
    }
    // Responses: input array with function_call / function_call_output
    if let Some(input) = body.get("input").and_then(Value::as_array) {
        for item in input {
            let kind = item.get("type").and_then(Value::as_str).unwrap_or("");
            if matches!(kind, "function_call" | "function_call_output") {
                return true;
            }
        }
    }
    false
}

fn has_images(body: &Value) -> bool {
    // Anthropic
    if let Some(messages) = body.get("messages").and_then(Value::as_array) {
        for m in messages {
            if let Some(arr) = m.get("content").and_then(Value::as_array) {
                for block in arr {
                    let kind = block.get("type").and_then(Value::as_str).unwrap_or("");
                    if matches!(kind, "image" | "image_url" | "input_image") {
                        return true;
                    }
                }
            }
        }
    }
    // Responses
    if let Some(input) = body.get("input").and_then(Value::as_array) {
        for item in input {
            if let Some(content) = item.get("content").and_then(Value::as_array) {
                for block in content {
                    if block.get("type").and_then(Value::as_str) == Some("input_image") {
                        return true;
                    }
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_provider() -> Provider {
        Provider::with_id("test".to_string(), "test".to_string(), json!({}), None)
    }

    fn provider_with_setting(key: &str, value: &str) -> Provider {
        Provider::with_id(
            "test".to_string(),
            "test".to_string(),
            json!({ key: value }),
            None,
        )
    }

    #[test]
    fn plain_text_anthropic_picks_agent_service() {
        let p = make_provider();
        let body = json!({ "messages": [{ "role": "user", "content": "hi" }] });
        assert_eq!(
            select_protocol(&p, InboundProtocol::AnthropicMessages, &body),
            CursorProtocol::AgentService
        );
    }

    #[test]
    fn tools_picks_agent_service() {
        let p = make_provider();
        let body = json!({
            "messages": [{ "role": "user", "content": "hi" }],
            "tools": [{ "name": "weather", "input_schema": {} }]
        });
        assert_eq!(
            select_protocol(&p, InboundProtocol::AnthropicMessages, &body),
            CursorProtocol::AgentService
        );
    }

    #[test]
    fn previous_response_id_picks_agent() {
        let p = make_provider();
        let body = json!({ "previous_response_id": "resp_abc", "input": "again" });
        assert_eq!(
            select_protocol(&p, InboundProtocol::OpenAiResponses, &body),
            CursorProtocol::AgentService
        );
    }

    #[test]
    fn explicit_chat_service_override_wins() {
        let p = provider_with_setting("cursor_protocol", "chat_service");
        let body = json!({
            "messages": [{ "role": "user", "content": "hi" }],
            "tools": [{ "name": "weather", "input_schema": {} }]
        });
        assert_eq!(
            select_protocol(&p, InboundProtocol::AnthropicMessages, &body),
            CursorProtocol::ChatService
        );
    }

    #[test]
    fn images_pick_agent_service() {
        let p = make_provider();
        let body = json!({
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "image", "source": { "type": "url", "url": "https://x/y.png" } }
                ]
            }]
        });
        assert_eq!(
            select_protocol(&p, InboundProtocol::AnthropicMessages, &body),
            CursorProtocol::AgentService
        );
    }

    #[test]
    fn tool_mode_disabled_override() {
        let p = provider_with_setting("cursor_tool_mode", "disabled");
        assert_eq!(select_tool_mode(&p), CursorToolMode::Disabled);
        let p2 = make_provider();
        assert_eq!(select_tool_mode(&p2), CursorToolMode::AgentMcp);
    }

    #[test]
    fn plain_text_openai_responses_picks_agent_service() {
        let p = make_provider();
        let body = json!({ "input": "hi", "model": "gpt-5" });
        assert_eq!(
            select_protocol(&p, InboundProtocol::OpenAiResponses, &body),
            CursorProtocol::AgentService
        );
    }

    #[test]
    fn auto_default_plain_chat_completions_picks_agent_service() {
        let p = make_provider();
        let body = json!({
            "messages": [{ "role": "user", "content": "hi" }]
        });
        assert_eq!(
            select_protocol(&p, InboundProtocol::OpenAiChat, &body),
            CursorProtocol::AgentService
        );
    }

    #[test]
    fn composer_model_picks_agent_service() {
        let p = make_provider();
        let body = json!({ "model": "composer-2.5", "input": "hi" });
        assert_eq!(
            select_protocol(&p, InboundProtocol::OpenAiResponses, &body),
            CursorProtocol::AgentService
        );
    }
}
