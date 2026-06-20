use super::{hyper_client::ProxyResponse, ProxyError};
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use http::{HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};

const CODEX_RESPONSES_WS_URL: &str = "wss://chatgpt.com/backend-api/codex/responses";
const CODEX_RESPONSES_WS_PROTOCOL: &str = "responses_websockets=2026-02-06";

pub(crate) fn forward_codex_responses_ws(
    headers: HeaderMap,
    body: Value,
    connect_timeout: std::time::Duration,
) -> Result<ProxyResponse, ProxyError> {
    let payload = build_response_create_payload(body)?;
    let stream = async_stream::stream! {
        match connect_and_stream(headers, payload, connect_timeout).await {
            Ok(upstream) => {
                futures::pin_mut!(upstream);
                while let Some(item) = upstream.next().await {
                    yield item;
                }
            }
            Err(err) => {
                yield Err(std::io::Error::other(err.to_string()));
            }
        }
    };

    Ok(ProxyResponse::local_sse(Box::pin(stream)))
}

fn build_response_create_payload(mut body: Value) -> Result<String, ProxyError> {
    let obj = body.as_object_mut().ok_or_else(|| {
        ProxyError::InvalidRequest(
            "Codex Responses WebSocket body must be a JSON object".to_string(),
        )
    })?;
    obj.insert(
        "type".to_string(),
        Value::String("response.create".to_string()),
    );
    obj.remove("stream");
    obj.remove("stream_options");
    obj.remove("background");
    serde_json::to_string(&body).map_err(|err| {
        ProxyError::Internal(format!(
            "Failed to serialize Codex WebSocket payload: {err}"
        ))
    })
}

async fn connect_and_stream(
    headers: HeaderMap,
    payload: String,
    connect_timeout: std::time::Duration,
) -> Result<impl futures::Stream<Item = Result<Bytes, std::io::Error>>, ProxyError> {
    let mut request = CODEX_RESPONSES_WS_URL
        .into_client_request()
        .map_err(|err| {
            ProxyError::ForwardFailed(format!("Failed to build Codex WebSocket request: {err}"))
        })?;

    copy_codex_ws_headers(&headers, request.headers_mut());
    if !request.headers().contains_key("openai-beta") {
        request.headers_mut().insert(
            HeaderName::from_static("openai-beta"),
            HeaderValue::from_static(CODEX_RESPONSES_WS_PROTOCOL),
        );
    }

    let connect = tokio_tungstenite::connect_async(request);
    let (mut socket, _) = tokio::time::timeout(connect_timeout, connect)
        .await
        .map_err(|_| {
            ProxyError::Timeout(format!(
                "Codex WebSocket connect timeout: {}s",
                connect_timeout.as_secs()
            ))
        })?
        .map_err(|err| {
            ProxyError::ForwardFailed(format!("Codex WebSocket connect failed: {err}"))
        })?;

    socket
        .send(Message::Text(payload.into()))
        .await
        .map_err(|err| ProxyError::ForwardFailed(format!("Codex WebSocket send failed: {err}")))?;

    let stream = async_stream::stream! {
        while let Some(message) = socket.next().await {
            match message {
                Ok(Message::Text(text)) => match codex_ws_message_to_sse(text.as_str()) {
                    Ok(Some(event)) => {
                        yield Ok(event.bytes);
                        if event.terminal {
                            yield Ok(Bytes::from_static(b"data: [DONE]\n\n"));
                            break;
                        }
                    }
                    Ok(None) => {}
                    Err(err) => {
                        yield Err(std::io::Error::other(err.to_string()));
                        break;
                    }
                },
                Ok(Message::Binary(_)) => {
                    yield Err(std::io::Error::other("Unexpected binary frame from Codex WebSocket"));
                    break;
                }
                Ok(Message::Close(_)) => break,
                Ok(Message::Ping(payload)) => {
                    if let Err(err) = socket.send(Message::Pong(payload)).await {
                        yield Err(std::io::Error::other(format!("Codex WebSocket pong failed: {err}")));
                        break;
                    }
                }
                Ok(Message::Pong(_)) => {}
                Ok(Message::Frame(_)) => {}
                Err(err) => {
                    yield Err(std::io::Error::other(format!("Codex WebSocket read failed: {err}")));
                    break;
                }
            }
        }
    };

    Ok(stream)
}

fn copy_codex_ws_headers(source: &HeaderMap, target: &mut HeaderMap) {
    for (name, value) in source {
        let lower = name.as_str().to_ascii_lowercase();
        if matches!(
            lower.as_str(),
            "host"
                | "connection"
                | "upgrade"
                | "content-length"
                | "content-type"
                | "accept-encoding"
                | "sec-websocket-key"
                | "sec-websocket-version"
                | "sec-websocket-extensions"
                | "sec-websocket-protocol"
        ) {
            continue;
        }
        target.insert(name.clone(), value.clone());
    }
}

#[derive(Debug)]
struct CodexWsSseEvent {
    bytes: Bytes,
    terminal: bool,
}

fn codex_ws_message_to_sse(text: &str) -> Result<Option<CodexWsSseEvent>, ProxyError> {
    let event: Value = serde_json::from_str(text).map_err(|err| {
        ProxyError::ForwardFailed(format!("Failed to parse Codex WebSocket event: {err}"))
    })?;

    if is_wrapped_error(&event) {
        return Err(ProxyError::UpstreamError {
            status: event
                .get("status")
                .or_else(|| event.get("status_code"))
                .and_then(Value::as_u64)
                .unwrap_or(502) as u16,
            body: Some(text.to_string()),
        });
    }

    let event_type = event
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("message");
    let out = format!("event: {event_type}\ndata: {text}\n\n");
    Ok(Some(CodexWsSseEvent {
        bytes: Bytes::from(out),
        terminal: is_terminal_event_type(event_type),
    }))
}

fn is_wrapped_error(event: &Value) -> bool {
    if event.get("type").and_then(Value::as_str) != Some("error") {
        return false;
    }
    event
        .get("status")
        .or_else(|| event.get("status_code"))
        .and_then(Value::as_u64)
        .is_some_and(|status| status >= 400)
}

fn is_terminal_event_type(event_type: &str) -> bool {
    matches!(
        event_type,
        "response.completed"
            | "response.done"
            | "response.failed"
            | "response.incomplete"
            | "error"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn payload_removes_http_only_fields() {
        let payload = build_response_create_payload(json!({
            "model": "gpt-5.5",
            "stream": true,
            "stream_options": {"include_usage": true},
            "background": false,
            "input": [{"role": "user", "content": "ping"}]
        }))
        .unwrap();
        let value: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(
            value.get("type").and_then(Value::as_str),
            Some("response.create")
        );
        assert!(value.get("stream").is_none());
        assert!(value.get("stream_options").is_none());
        assert!(value.get("background").is_none());
    }

    #[test]
    fn ws_event_becomes_sse_event() {
        let bytes = codex_ws_message_to_sse(
            r#"{"type":"response.completed","response":{"status":"completed"}}"#,
        )
        .unwrap()
        .unwrap();
        assert!(bytes.terminal);
        let text = String::from_utf8(bytes.bytes.to_vec()).unwrap();
        assert!(text.starts_with("event: response.completed\n"));
        assert!(text.contains("data: {\"type\":\"response.completed\""));
    }

    #[test]
    fn wrapped_error_is_upstream_error() {
        let err = codex_ws_message_to_sse(
            r#"{"type":"error","status":401,"error":{"message":"Unauthorized"}}"#,
        )
        .unwrap_err();
        assert!(matches!(err, ProxyError::UpstreamError { status: 401, .. }));
    }
}
