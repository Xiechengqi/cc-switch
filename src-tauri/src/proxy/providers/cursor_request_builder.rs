//! Pull the structured fields cursor's AgentService needs out of the three
//! request body shapes cc-switch accepts (Anthropic Messages, OpenAI Chat
//! Completions, OpenAI Responses).
//!
//! The output is intentionally narrow: cursor's AgentService takes a single
//! flat `user_text`, optional `system_prompt`, an `mcp_tools` inventory,
//! optional vision inputs, and (on follow-up turns) a stash of tool-call
//! results that we need to map onto a parked session.
//!
//! Anything cursor doesn't understand (custom OpenAI params, sampling
//! controls, `tool_choice`) is dropped here — we communicate constraints
//! through the system prompt where it matters and rely on tool-presence to
//! steer the model otherwise.

use super::cursor_agent_proto::{
    anthropic_tools_to_mcp_defs, openai_tools_to_mcp_defs, McpToolDef,
};
use super::cursor_image::ImageRef;
use bytes::Bytes;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboundProtocol {
    AnthropicMessages,
    OpenAiChat,
    OpenAiResponses,
}

#[derive(Debug, Clone)]
pub struct ToolResultBlock {
    /// Client-facing tool call id — what cc-switch emitted in the previous
    /// turn. Used to look up the pending exec_id in the session.
    pub tool_call_id: String,
    /// Result content as a plain string (cursor's McpResult expects text).
    pub content: String,
    pub is_error: bool,
}

#[derive(Debug, Clone)]
pub struct AgentRunPlan {
    pub system_prompt: Option<String>,
    pub user_text: String,
    pub tools: Vec<McpToolDef>,
    pub images: Vec<ImageRef>,
    pub tool_results: Vec<ToolResultBlock>,
    /// Cursor's `RequestedModel.model_id` — the value passed to
    /// `resolve_requested_model`. Comes from the upstream-mapped body.
    pub model_id: String,
    /// Optional Responses API `previous_response_id` — used to find a parked
    /// session.
    pub previous_response_id: Option<String>,
}

/// Validate tool-result context for AgentService routing. Returns an error
/// message if the request carries a `function_call_output` / `tool_result`
/// whose `call_id` is empty — Cursor's AgentService cannot match it to a
/// pending exec_id and the turn would silently fail. Mirrors sub2api's
/// `validateFunctionCallOutputRequest` guard.
pub fn validate_tool_result_context(plan: &AgentRunPlan) -> Result<(), String> {
    for tr in &plan.tool_results {
        if tr.tool_call_id.trim().is_empty() {
            return Err(
                "function_call_output requires a non-empty call_id; \
                 continuation via previous_response_id without call_id is not supported"
                    .to_string(),
            );
        }
    }
    Ok(())
}

/// Build a plan from a request body. The body is the **upstream-mapped**
/// version (after `apply_model_mapping`), so `model_id` here is what cursor
/// will see on the wire.
pub fn build_plan(protocol: InboundProtocol, body: &Value) -> AgentRunPlan {
    let model_id = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("default")
        .to_string();
    let previous_response_id = body
        .get("previous_response_id")
        .and_then(Value::as_str)
        .map(str::to_string);

    let (system_prompt, user_text, images, tool_results) = match protocol {
        InboundProtocol::AnthropicMessages => decompose_anthropic(body),
        InboundProtocol::OpenAiChat => decompose_openai_chat(body),
        InboundProtocol::OpenAiResponses => decompose_openai_responses(body),
    };
    let tools = match protocol {
        InboundProtocol::AnthropicMessages => body
            .get("tools")
            .map(anthropic_tools_to_mcp_defs)
            .unwrap_or_default(),
        InboundProtocol::OpenAiChat | InboundProtocol::OpenAiResponses => body
            .get("tools")
            .map(openai_tools_to_mcp_defs)
            .unwrap_or_default(),
    };

    AgentRunPlan {
        system_prompt,
        user_text,
        tools,
        images,
        tool_results,
        model_id,
        previous_response_id,
    }
}

// ─── Anthropic Messages ────────────────────────────────────────────────────

fn decompose_anthropic(
    body: &Value,
) -> (Option<String>, String, Vec<ImageRef>, Vec<ToolResultBlock>) {
    let mut system_prompt: Option<String> = body
        .get("system")
        .and_then(stringify_anthropic_text_or_blocks);

    let mut images = Vec::new();
    let mut tool_results = Vec::new();
    let mut conversation_lines: Vec<String> = Vec::new();
    let mut latest_user_text: Vec<String> = Vec::new();

    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    for (idx, msg) in messages.iter().enumerate() {
        let role = msg.get("role").and_then(Value::as_str).unwrap_or("user");
        let is_last = idx == messages.len() - 1;
        let content = msg.get("content");
        let Some(content) = content else { continue };

        match content {
            Value::String(s) => match role {
                "user" if is_last => latest_user_text.push(s.clone()),
                _ => conversation_lines.push(format!("{}: {}", role_label(role), s)),
            },
            Value::Array(blocks) => {
                let mut text_acc = Vec::new();
                for block in blocks {
                    let kind = block.get("type").and_then(Value::as_str).unwrap_or("");
                    match kind {
                        "text" => {
                            if let Some(t) = block.get("text").and_then(Value::as_str) {
                                text_acc.push(t.to_string());
                            }
                        }
                        "image" => {
                            if let Some(img) = anthropic_image_to_ref(block) {
                                images.push(img);
                            }
                        }
                        "tool_use" => {
                            // Assistant tool call from a prior turn. Render as
                            // a labeled line so the model has the context.
                            let name = block.get("name").and_then(Value::as_str).unwrap_or("");
                            let id = block.get("id").and_then(Value::as_str).unwrap_or("");
                            let input = block
                                .get("input")
                                .map(|v| v.to_string())
                                .unwrap_or_else(|| "{}".to_string());
                            conversation_lines.push(format!(
                                "Assistant called tool {name} ({id}) with arguments: {input}"
                            ));
                        }
                        "tool_result" => {
                            let id = block
                                .get("tool_use_id")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string();
                            let is_error = block
                                .get("is_error")
                                .and_then(Value::as_bool)
                                .unwrap_or(false);
                            let content_text = stringify_anthropic_text_or_blocks(
                                block.get("content").unwrap_or(&Value::Null),
                            )
                            .unwrap_or_default();
                            tool_results.push(ToolResultBlock {
                                tool_call_id: id.clone(),
                                content: content_text.clone(),
                                is_error,
                            });
                            // Also surface in conversation for cold-resume.
                            conversation_lines.push(format!("Tool result ({id}): {content_text}"));
                        }
                        _ => {}
                    }
                }
                let joined = text_acc.join("\n");
                if !joined.is_empty() {
                    if role == "user" && is_last {
                        latest_user_text.push(joined);
                    } else {
                        conversation_lines.push(format!("{}: {}", role_label(role), joined));
                    }
                }
            }
            _ => {}
        }
    }

    if system_prompt.is_none() {
        system_prompt = body
            .get("system")
            .and_then(Value::as_str)
            .map(str::to_string);
    }

    let user_text = if conversation_lines.is_empty() {
        latest_user_text.join("\n")
    } else {
        let mut all = conversation_lines;
        if !latest_user_text.is_empty() {
            all.push(format!("User: {}", latest_user_text.join("\n")));
        }
        all.join("\n\n")
    };
    (system_prompt, user_text, images, tool_results)
}

fn stringify_anthropic_text_or_blocks(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Array(arr) => {
            let parts: Vec<String> = arr
                .iter()
                .filter_map(|b| b.get("text").and_then(Value::as_str).map(str::to_string))
                .collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("\n"))
            }
        }
        _ => None,
    }
}

fn anthropic_image_to_ref(block: &Value) -> Option<ImageRef> {
    let source = block.get("source")?;
    let kind = source.get("type").and_then(Value::as_str).unwrap_or("");
    match kind {
        "base64" => {
            let media_type = source
                .get("media_type")
                .and_then(Value::as_str)
                .unwrap_or("image/png")
                .to_string();
            let data = source.get("data").and_then(Value::as_str)?;
            let decoded =
                base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data.trim())
                    .ok()?;
            Some(ImageRef::Inline {
                mime: media_type,
                data: Bytes::from(decoded),
            })
        }
        "url" => {
            let url = source.get("url").and_then(Value::as_str)?;
            if url.starts_with("data:") {
                Some(ImageRef::DataUri(url.to_string()))
            } else {
                Some(ImageRef::HttpUrl(url.to_string()))
            }
        }
        _ => None,
    }
}

// ─── OpenAI Chat Completions ───────────────────────────────────────────────

fn decompose_openai_chat(
    body: &Value,
) -> (Option<String>, String, Vec<ImageRef>, Vec<ToolResultBlock>) {
    let mut system_chunks: Vec<String> = Vec::new();
    let mut conversation_lines: Vec<String> = Vec::new();
    let mut latest_user_text: Vec<String> = Vec::new();
    let mut images = Vec::new();
    let mut tool_results = Vec::new();

    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    for (idx, msg) in messages.iter().enumerate() {
        let role = msg.get("role").and_then(Value::as_str).unwrap_or("user");
        let is_last = idx == messages.len() - 1;
        let content = msg.get("content");
        match role {
            "system" => {
                if let Some(text) = content.and_then(openai_content_text) {
                    system_chunks.push(text);
                }
            }
            "user" => {
                if let Some(c) = content {
                    let (text, mut imgs) = openai_content_parts(c);
                    images.append(&mut imgs);
                    if !text.is_empty() {
                        if is_last {
                            latest_user_text.push(text);
                        } else {
                            conversation_lines.push(format!("User: {text}"));
                        }
                    }
                }
            }
            "assistant" => {
                let text = content.and_then(openai_content_text).unwrap_or_default();
                if !text.is_empty() {
                    conversation_lines.push(format!("Assistant: {text}"));
                }
                if let Some(tool_calls) = msg.get("tool_calls").and_then(Value::as_array) {
                    for tc in tool_calls {
                        let name = tc
                            .get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        let id = tc.get("id").and_then(Value::as_str).unwrap_or("");
                        let args = tc
                            .get("function")
                            .and_then(|f| f.get("arguments"))
                            .and_then(Value::as_str)
                            .unwrap_or("{}");
                        conversation_lines.push(format!(
                            "Assistant called tool {name} ({id}) with arguments: {args}"
                        ));
                    }
                }
            }
            "tool" => {
                let id = msg
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let text = content.and_then(openai_content_text).unwrap_or_default();
                tool_results.push(ToolResultBlock {
                    tool_call_id: id.clone(),
                    content: text.clone(),
                    is_error: false,
                });
                conversation_lines.push(format!("Tool result ({id}): {text}"));
            }
            other => {
                let text = content.and_then(openai_content_text).unwrap_or_default();
                if !text.is_empty() {
                    conversation_lines.push(format!("{other}: {text}"));
                }
            }
        }
    }

    let user_text = if conversation_lines.is_empty() {
        latest_user_text.join("\n")
    } else {
        let mut all = conversation_lines;
        if !latest_user_text.is_empty() {
            all.push(format!("User: {}", latest_user_text.join("\n")));
        }
        all.join("\n\n")
    };
    let system_prompt = if system_chunks.is_empty() {
        None
    } else {
        Some(system_chunks.join("\n\n"))
    };
    (system_prompt, user_text, images, tool_results)
}

fn openai_content_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Array(arr) => {
            let parts: Vec<String> = arr
                .iter()
                .filter_map(|p| p.get("text").and_then(Value::as_str).map(str::to_string))
                .collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("\n"))
            }
        }
        _ => None,
    }
}

fn openai_content_parts(v: &Value) -> (String, Vec<ImageRef>) {
    let mut texts = Vec::new();
    let mut images = Vec::new();
    match v {
        Value::String(s) => texts.push(s.clone()),
        Value::Array(arr) => {
            for part in arr {
                let kind = part.get("type").and_then(Value::as_str).unwrap_or("");
                match kind {
                    "text" | "input_text" => {
                        if let Some(t) = part.get("text").and_then(Value::as_str) {
                            texts.push(t.to_string());
                        }
                    }
                    "image_url" => {
                        let url = part
                            .get("image_url")
                            .and_then(|iu| iu.get("url"))
                            .or_else(|| part.get("image_url").filter(|v| v.is_string()))
                            .and_then(Value::as_str);
                        if let Some(url) = url {
                            push_image_ref(url, &mut images);
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    (texts.join("\n"), images)
}

fn push_image_ref(url: &str, out: &mut Vec<ImageRef>) {
    if url.starts_with("data:") {
        out.push(ImageRef::DataUri(url.to_string()));
    } else if url.starts_with("http://") || url.starts_with("https://") {
        out.push(ImageRef::HttpUrl(url.to_string()));
    }
}

// ─── OpenAI Responses ──────────────────────────────────────────────────────

fn decompose_openai_responses(
    body: &Value,
) -> (Option<String>, String, Vec<ImageRef>, Vec<ToolResultBlock>) {
    let mut system_chunks: Vec<String> = Vec::new();
    let mut conversation_lines: Vec<String> = Vec::new();
    let mut latest_user_text: Vec<String> = Vec::new();
    let mut images = Vec::new();
    let mut tool_results = Vec::new();

    if let Some(instructions) = body.get("instructions").and_then(Value::as_str) {
        system_chunks.push(instructions.to_string());
    }

    // Responses `input` can be:
    //   * a string (single user turn)
    //   * an array of typed input items (messages, function_call,
    //     function_call_output, etc.)
    let input = body.get("input");
    if let Some(input) = input {
        match input {
            Value::String(s) => latest_user_text.push(s.clone()),
            Value::Array(items) => {
                let len = items.len();
                for (idx, item) in items.iter().enumerate() {
                    let kind = item
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("message");
                    let is_last = idx == len - 1;
                    match kind {
                        "message" | "" => {
                            let role = item.get("role").and_then(Value::as_str).unwrap_or("user");
                            let (text, mut imgs) = openai_responses_content_parts(
                                item.get("content").unwrap_or(&Value::Null),
                            );
                            images.append(&mut imgs);
                            match role {
                                "system" => {
                                    if !text.is_empty() {
                                        system_chunks.push(text);
                                    }
                                }
                                "user" => {
                                    if !text.is_empty() {
                                        if is_last {
                                            latest_user_text.push(text);
                                        } else {
                                            conversation_lines.push(format!("User: {text}"));
                                        }
                                    }
                                }
                                "assistant" => {
                                    if !text.is_empty() {
                                        conversation_lines.push(format!("Assistant: {text}"));
                                    }
                                }
                                other => {
                                    if !text.is_empty() {
                                        conversation_lines.push(format!("{other}: {text}"));
                                    }
                                }
                            }
                        }
                        "function_call" => {
                            let name = item.get("name").and_then(Value::as_str).unwrap_or("");
                            let call_id = item
                                .get("call_id")
                                .or_else(|| item.get("id"))
                                .and_then(Value::as_str)
                                .unwrap_or("");
                            let args = item
                                .get("arguments")
                                .and_then(Value::as_str)
                                .unwrap_or("{}");
                            conversation_lines.push(format!(
                                "Assistant called tool {name} ({call_id}) with arguments: {args}"
                            ));
                        }
                        "function_call_output" => {
                            let call_id = item
                                .get("call_id")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string();
                            let output = item
                                .get("output")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string();
                            tool_results.push(ToolResultBlock {
                                tool_call_id: call_id.clone(),
                                content: output.clone(),
                                is_error: false,
                            });
                            conversation_lines.push(format!("Tool result ({call_id}): {output}"));
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    let user_text = if conversation_lines.is_empty() {
        latest_user_text.join("\n")
    } else {
        let mut all = conversation_lines;
        if !latest_user_text.is_empty() {
            all.push(format!("User: {}", latest_user_text.join("\n")));
        }
        all.join("\n\n")
    };
    let system_prompt = if system_chunks.is_empty() {
        None
    } else {
        Some(system_chunks.join("\n\n"))
    };
    (system_prompt, user_text, images, tool_results)
}

fn openai_responses_content_parts(v: &Value) -> (String, Vec<ImageRef>) {
    let mut texts = Vec::new();
    let mut images = Vec::new();
    match v {
        Value::String(s) => texts.push(s.clone()),
        Value::Array(arr) => {
            for part in arr {
                let kind = part.get("type").and_then(Value::as_str).unwrap_or("");
                match kind {
                    "input_text" | "text" | "output_text" => {
                        if let Some(t) = part.get("text").and_then(Value::as_str) {
                            texts.push(t.to_string());
                        }
                    }
                    "input_image" => {
                        let url = part
                            .get("image_url")
                            .and_then(Value::as_str)
                            .or_else(|| part.get("url").and_then(Value::as_str));
                        if let Some(url) = url {
                            push_image_ref(url, &mut images);
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    (texts.join("\n"), images)
}

fn role_label(role: &str) -> &'static str {
    match role {
        "system" => "System",
        "assistant" => "Assistant",
        "tool" => "Tool",
        _ => "User",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn anthropic_single_user_string() {
        let body = json!({
            "model": "claude-sonnet-4-7",
            "messages": [{ "role": "user", "content": "hello" }]
        });
        let plan = build_plan(InboundProtocol::AnthropicMessages, &body);
        assert_eq!(plan.user_text, "hello");
        assert!(plan.tools.is_empty());
        assert!(plan.images.is_empty());
    }

    #[test]
    fn anthropic_system_and_tools() {
        let body = json!({
            "model": "claude-sonnet-4-7",
            "system": "be precise",
            "tools": [{ "name": "weather", "description": "wx",
                         "input_schema": {"type": "object"} }],
            "messages": [{ "role": "user", "content": "hello" }]
        });
        let plan = build_plan(InboundProtocol::AnthropicMessages, &body);
        assert_eq!(plan.system_prompt.as_deref(), Some("be precise"));
        assert_eq!(plan.tools.len(), 1);
        assert_eq!(plan.tools[0].name, "weather");
    }

    #[test]
    fn anthropic_tool_result_round_trip() {
        let body = json!({
            "model": "claude-sonnet-4-7",
            "messages": [
                { "role": "user", "content": "what is the weather?" },
                { "role": "assistant", "content": [
                    { "type": "tool_use", "id": "tc_1", "name": "weather", "input": {"city":"BJ"} }
                ]},
                { "role": "user", "content": [
                    { "type": "tool_result", "tool_use_id": "tc_1", "content": "sunny" }
                ]}
            ]
        });
        let plan = build_plan(InboundProtocol::AnthropicMessages, &body);
        assert_eq!(plan.tool_results.len(), 1);
        assert_eq!(plan.tool_results[0].tool_call_id, "tc_1");
        assert_eq!(plan.tool_results[0].content, "sunny");
    }

    #[test]
    fn openai_chat_image_url() {
        let body = json!({
            "model": "gpt-5",
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "text", "text": "look:" },
                    { "type": "image_url", "image_url": { "url": "https://example.com/x.png" } }
                ]
            }]
        });
        let plan = build_plan(InboundProtocol::OpenAiChat, &body);
        assert_eq!(plan.images.len(), 1);
        match &plan.images[0] {
            ImageRef::HttpUrl(u) => assert_eq!(u, "https://example.com/x.png"),
            _ => panic!("expected HttpUrl"),
        }
    }

    #[test]
    fn openai_responses_function_call_output() {
        let body = json!({
            "model": "gpt-5",
            "input": [
                { "type": "message", "role": "user", "content": [
                    { "type": "input_text", "text": "weather?" }
                ]},
                { "type": "function_call", "name": "weather", "call_id": "fc_1",
                  "arguments": "{\"city\":\"BJ\"}" },
                { "type": "function_call_output", "call_id": "fc_1", "output": "sunny" }
            ]
        });
        let plan = build_plan(InboundProtocol::OpenAiResponses, &body);
        assert_eq!(plan.tool_results.len(), 1);
        assert_eq!(plan.tool_results[0].tool_call_id, "fc_1");
        assert_eq!(plan.tool_results[0].content, "sunny");
    }

    #[test]
    fn openai_responses_previous_response_id_extracted() {
        let body = json!({
            "model": "gpt-5",
            "previous_response_id": "resp_abc",
            "input": "again"
        });
        let plan = build_plan(InboundProtocol::OpenAiResponses, &body);
        assert_eq!(plan.previous_response_id.as_deref(), Some("resp_abc"));
    }

    #[test]
    fn validate_tool_result_context_rejects_empty_call_id() {
        let body = json!({
            "model": "gpt-5",
            "input": [
                { "type": "function_call_output", "call_id": "", "output": "bad" }
            ]
        });
        let plan = build_plan(InboundProtocol::OpenAiResponses, &body);
        assert!(validate_tool_result_context(&plan).is_err());
    }

    #[test]
    fn validate_tool_result_context_accepts_non_empty_call_id() {
        let body = json!({
            "model": "gpt-5",
            "input": [
                { "type": "function_call_output", "call_id": "fc_1", "output": "ok" }
            ]
        });
        let plan = build_plan(InboundProtocol::OpenAiResponses, &body);
        assert!(validate_tool_result_context(&plan).is_ok());
    }

    #[test]
    fn validate_tool_result_context_accepts_no_tool_results() {
        let body = json!({ "model": "gpt-5", "input": "hello" });
        let plan = build_plan(InboundProtocol::OpenAiResponses, &body);
        assert!(validate_tool_result_context(&plan).is_ok());
    }
}
