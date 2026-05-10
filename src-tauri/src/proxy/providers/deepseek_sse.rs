use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeepSeekEvent {
    Text(String),
    Done,
    Ignored,
}

pub fn parse_sse_data_line(line: &str) -> DeepSeekEvent {
    let data = line.strip_prefix("data:").unwrap_or(line).trim();
    if data.is_empty() {
        return DeepSeekEvent::Ignored;
    }
    if data == "[DONE]" {
        return DeepSeekEvent::Done;
    }
    let Ok(value) = serde_json::from_str::<Value>(data) else {
        return DeepSeekEvent::Ignored;
    };
    if is_finished_event(&value) {
        return DeepSeekEvent::Done;
    }
    extract_text(&value)
        .map(DeepSeekEvent::Text)
        .unwrap_or(DeepSeekEvent::Ignored)
}

pub fn extract_text(value: &Value) -> Option<String> {
    if let Some(path) = value.get("p").and_then(Value::as_str).map(str::trim) {
        if is_finished_status(path, value.get("v")) || should_skip_path(path) {
            return None;
        }
        if path == "response/content" || path.ends_with("/content") {
            return string_value(value.get("v"));
        }
        if path == "response/fragments" {
            return fragment_append_text(value);
        }
        return None;
    }

    [
        pointer_string(value, "/choices/0/delta/content"),
        pointer_string(value, "/choices/0/message/content"),
        pointer_string(value, "/delta/content"),
        pointer_string(value, "/content"),
        pointer_string(value, "/text"),
        pointer_string(value, "/response/content"),
        pointer_string(value, "/response/text"),
        pointer_string(value, "/v").filter(|text| !text.eq_ignore_ascii_case("FINISHED")),
    ]
    .into_iter()
    .flatten()
    .find(|text| !text.is_empty())
}

pub fn is_finished_event(value: &Value) -> bool {
    value
        .get("p")
        .and_then(Value::as_str)
        .map(str::trim)
        .is_some_and(|path| is_finished_status(path, value.get("v")))
}

fn is_finished_status(path: &str, v: Option<&Value>) -> bool {
    matches!(path, "" | "status" | "response/status")
        && v.and_then(Value::as_str)
            .map(str::trim)
            .is_some_and(|s| s.eq_ignore_ascii_case("FINISHED"))
}

fn should_skip_path(path: &str) -> bool {
    path.contains("quasi_status")
        || path.contains("elapsed_secs")
        || path.contains("pending_fragment")
        || path.contains("conversation_mode")
        || path == "response/search_status"
        || (path.starts_with("response/fragments/") && path.ends_with("/status"))
}

fn string_value(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Object(obj)) => obj
            .get("text")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                obj.get("content")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            }),
        _ => None,
    }
}

fn fragment_append_text(value: &Value) -> Option<String> {
    if value.get("o").and_then(Value::as_str) != Some("APPEND") {
        return None;
    }
    let mut out = String::new();
    for item in value.get("v")?.as_array()? {
        let item_type = item
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_uppercase();
        if item_type == "RESPONSE" {
            if let Some(content) = item.get("content").and_then(Value::as_str) {
                out.push_str(content);
            }
        }
    }
    (!out.is_empty()).then_some(out)
}

fn pointer_string(value: &Value, pointer: &str) -> Option<String> {
    value.pointer(pointer).and_then(|v| match v {
        Value::String(s) => Some(s.clone()),
        _ => None,
    })
}
