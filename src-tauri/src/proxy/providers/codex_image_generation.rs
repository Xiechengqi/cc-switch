use crate::proxy::codex_identity::{codex_cli_user_agent, CODEX_CLI_VERSION};
use crate::proxy::sse::{append_utf8_safe, strip_sse_field, take_sse_block};
use crate::proxy::{http_client, ProxyError};
use base64::Engine;
use bytes::Bytes;
use futures::stream::Stream;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::pin::Pin;
use std::time::Duration;

const CODEX_IMAGE_BACKEND_URL: &str = "https://chatgpt.com/backend-api/codex/responses";
const CODEX_IMAGE_MODEL_DEFAULT: &str = "gpt-5.5";
const CODEX_IMAGE_OUTPUT_FORMAT_DEFAULT: &str = "png";
const CODEX_IMAGEGEN_TIMEOUT_SECS: u64 = 300;

#[derive(Debug, Clone, Deserialize)]
pub struct OpenAiImageGenerationRequest {
    pub model: Option<String>,
    pub prompt: String,
    pub n: Option<u32>,
    pub size: Option<String>,
    pub response_format: Option<String>,
    pub output_format: Option<String>,
    pub quality: Option<String>,
    pub style: Option<String>,
    pub background: Option<String>,
    pub stream: Option<bool>,
    pub partial_images: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedImageGenerationRequest {
    model: String,
    prompt: String,
    size: Option<String>,
    output_format: String,
    quality: Option<String>,
    style: Option<String>,
    background: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedImage {
    pub b64_json: String,
    pub revised_prompt: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct OpenAiImagesResponse {
    pub created: i64,
    pub data: Vec<OpenAiImageData>,
}

#[derive(Debug, Serialize)]
pub struct OpenAiImageData {
    pub b64_json: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revised_prompt: Option<String>,
}

pub fn build_openai_images_response(image: GeneratedImage) -> OpenAiImagesResponse {
    OpenAiImagesResponse {
        created: chrono::Utc::now().timestamp(),
        data: vec![OpenAiImageData {
            b64_json: image.b64_json,
            revised_prompt: image.revised_prompt,
        }],
    }
}

pub async fn generate_image_with_codex_oauth(
    token: &str,
    account_id: Option<&str>,
    request: OpenAiImageGenerationRequest,
) -> Result<GeneratedImage, ProxyError> {
    let response = send_codex_image_request(token, account_id, request).await?;
    collect_image_generation_result(response).await
}

pub async fn stream_image_with_codex_oauth(
    token: &str,
    account_id: Option<&str>,
    request: OpenAiImageGenerationRequest,
) -> Result<Pin<Box<dyn Stream<Item = Result<Bytes, std::io::Error>> + Send>>, ProxyError> {
    let response = send_codex_image_request(token, account_id, request).await?;
    Ok(Box::pin(normalized_image_generation_sse_stream(response)))
}

async fn send_codex_image_request(
    token: &str,
    account_id: Option<&str>,
    request: OpenAiImageGenerationRequest,
) -> Result<reqwest::Response, ProxyError> {
    let parsed = validate_image_request(request)?;
    let payload = build_codex_image_payload(&parsed);
    let session_id = uuid::Uuid::new_v4().to_string();

    let client = http_client::get();
    let mut builder = client
        .post(CODEX_IMAGE_BACKEND_URL)
        .timeout(Duration::from_secs(CODEX_IMAGEGEN_TIMEOUT_SECS))
        .bearer_auth(token)
        .header(reqwest::header::ACCEPT, "text/event-stream")
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(reqwest::header::CONNECTION, "Keep-Alive")
        .header(reqwest::header::USER_AGENT, codex_imagegen_user_agent())
        .header("version", CODEX_CLI_VERSION)
        .header("originator", "codex_cli_rs")
        .header("session_id", &session_id)
        .header("x-client-request-id", &session_id)
        .header("x-codex-window-id", format!("{session_id}:0"))
        .json(&payload);

    if let Some(account_id) = account_id.map(str::trim).filter(|value| !value.is_empty()) {
        builder = builder.header("chatgpt-account-id", account_id);
    }

    let response = builder
        .send()
        .await
        .map_err(|e| ProxyError::ForwardFailed(format!("Codex image request failed: {e}")))?;

    let status = response.status();
    if !status.is_success() {
        let body = response
            .text()
            .await
            .unwrap_or_else(|e| format!("Failed to read upstream error body: {e}"));
        return Err(ProxyError::UpstreamError {
            status: status.as_u16(),
            body: Some(truncate_for_error(body, 1200)),
        });
    }

    Ok(response)
}

fn codex_imagegen_user_agent() -> String {
    codex_cli_user_agent(
        std::env::consts::OS,
        std::env::consts::ARCH,
        "cc-switch image generation",
    )
}

fn validate_image_request(
    request: OpenAiImageGenerationRequest,
) -> Result<ParsedImageGenerationRequest, ProxyError> {
    let prompt = request.prompt.trim();
    if prompt.is_empty() {
        return Err(ProxyError::InvalidRequest(
            "prompt is required for image generation".to_string(),
        ));
    }

    if request.n.unwrap_or(1) > 1 {
        return Err(ProxyError::InvalidRequest(
            "Codex OAuth image generation currently supports n=1 only".to_string(),
        ));
    }

    if let Some(format) = request.response_format.as_deref().map(str::trim) {
        if !format.is_empty() && format != "b64_json" {
            return Err(ProxyError::InvalidRequest(
                "Only response_format=b64_json is supported".to_string(),
            ));
        }
    }

    let output_format = request
        .output_format
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(CODEX_IMAGE_OUTPUT_FORMAT_DEFAULT);
    if !matches!(output_format, "png" | "jpeg" | "webp") {
        return Err(ProxyError::InvalidRequest(
            "output_format must be one of: png, jpeg, webp".to_string(),
        ));
    }

    let size = request
        .size
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "auto")
        .map(validate_size)
        .transpose()?;
    let _stream_requested = request.stream.unwrap_or(false);
    let _partial_images = request.partial_images.unwrap_or(0);

    Ok(ParsedImageGenerationRequest {
        model: request
            .model
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(CODEX_IMAGE_MODEL_DEFAULT)
            .to_string(),
        prompt: prompt.to_string(),
        size,
        output_format: output_format.to_string(),
        quality: clean_optional(request.quality),
        style: clean_optional(request.style),
        background: clean_optional(request.background),
    })
}

fn clean_optional(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty() && value != "auto")
}

fn validate_size(value: &str) -> Result<String, ProxyError> {
    let Some((width, height)) = value.split_once('x') else {
        return Err(ProxyError::InvalidRequest(
            "size must be auto or WIDTHxHEIGHT".to_string(),
        ));
    };
    let width = width
        .parse::<u32>()
        .map_err(|_| ProxyError::InvalidRequest("size must be auto or WIDTHxHEIGHT".to_string()))?;
    let height = height
        .parse::<u32>()
        .map_err(|_| ProxyError::InvalidRequest("size must be auto or WIDTHxHEIGHT".to_string()))?;
    if width == 0 || height == 0 {
        return Err(ProxyError::InvalidRequest(
            "size dimensions must be greater than zero".to_string(),
        ));
    }
    Ok(value.to_string())
}

fn build_codex_image_payload(request: &ParsedImageGenerationRequest) -> Value {
    let user_text = build_image_prompt_text(request);
    let mut image_tool = json!({
        "type": "image_generation",
        "output_format": request.output_format,
    });
    if let Some(size) = &request.size {
        image_tool["size"] = Value::String(size.clone());
    }

    json!({
        "model": request.model,
        "stream": true,
        "instructions": "You are an image generation assistant.",
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{
                "type": "input_text",
                "text": user_text,
            }],
        }],
        "tools": [image_tool],
        "tool_choice": "auto",
        "parallel_tool_calls": false,
        "store": false,
        "reasoning": { "effort": "low", "summary": "auto" },
        "include": ["reasoning.encrypted_content"],
        "text": { "verbosity": "low" },
    })
}

fn normalized_image_generation_sse_stream(
    response: reqwest::Response,
) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send {
    async_stream::stream! {
        let mut stream = response.bytes_stream();
        let mut buffer = String::new();
        let mut utf8_remainder = Vec::new();
        let mut event_types = Vec::new();

        yield Ok(sse_event_bytes(
            "image_generation.started",
            json!({ "type": "image_generation.started" }),
        ));

        while let Some(chunk) = stream.next().await {
            let bytes = match chunk {
                Ok(bytes) => bytes,
                Err(err) => {
                    yield Ok(sse_error_event_bytes(&format!("Failed to read Codex image SSE: {err}")));
                    return;
                }
            };
            append_utf8_safe(&mut buffer, &mut utf8_remainder, &bytes);

            while let Some(block) = take_sse_block(&mut buffer) {
                match image_generation_stream_event_from_block(&block, &mut event_types) {
                    Ok(Some(ImageGenerationStreamEvent::Progress(event_type))) => {
                        yield Ok(sse_event_bytes(
                            event_type,
                            json!({ "type": event_type }),
                        ));
                    }
                    Ok(Some(ImageGenerationStreamEvent::Completed(image))) => {
                        if let Err(err) = validate_image_base64(&image.b64_json) {
                            yield Ok(sse_error_event_bytes(&err.to_string()));
                            return;
                        }
                        let response = build_openai_images_response(image);
                        yield Ok(sse_event_bytes("image_generation.completed", json!(response)));
                        yield Ok(Bytes::from_static(b"data: [DONE]\n\n"));
                        return;
                    }
                    Ok(None) => {}
                    Err(err) => {
                        yield Ok(sse_error_event_bytes(&err.to_string()));
                        return;
                    }
                }
            }
        }

        if !buffer.trim().is_empty() {
            let tail = std::mem::take(&mut buffer) + "\n\n";
            buffer.push_str(&tail);
            while let Some(block) = take_sse_block(&mut buffer) {
                match image_generation_stream_event_from_block(&block, &mut event_types) {
                    Ok(Some(ImageGenerationStreamEvent::Progress(event_type))) => {
                        yield Ok(sse_event_bytes(
                            event_type,
                            json!({ "type": event_type }),
                        ));
                    }
                    Ok(Some(ImageGenerationStreamEvent::Completed(image))) => {
                        if let Err(err) = validate_image_base64(&image.b64_json) {
                            yield Ok(sse_error_event_bytes(&err.to_string()));
                            return;
                        }
                        let response = build_openai_images_response(image);
                        yield Ok(sse_event_bytes("image_generation.completed", json!(response)));
                        yield Ok(Bytes::from_static(b"data: [DONE]\n\n"));
                        return;
                    }
                    Ok(None) => {}
                    Err(err) => {
                        yield Ok(sse_error_event_bytes(&err.to_string()));
                        return;
                    }
                }
            }
        }

        let events_seen = if event_types.is_empty() {
            "(none)".to_string()
        } else {
            event_types.sort();
            event_types.dedup();
            event_types.join(", ")
        };
        yield Ok(sse_error_event_bytes(&format!(
            "Codex image generation finished without image_generation_call.result; events seen: {events_seen}"
        )));
    }
}

fn build_image_prompt_text(request: &ParsedImageGenerationRequest) -> String {
    let mut text = format!(
        "Use the image_generation tool to render the following. Request: {}. Output format: {}.",
        request.prompt, request.output_format
    );
    if let Some(size) = &request.size {
        text.push_str(&format!(" Size: {size}."));
    }
    if let Some(quality) = &request.quality {
        text.push_str(&format!(" Requested quality: {quality}."));
    }
    if let Some(style) = &request.style {
        text.push_str(&format!(" Requested style: {style}."));
    }
    if let Some(background) = &request.background {
        text.push_str(&format!(" Requested background: {background}."));
    }
    text.push_str(" Do not include explanatory text; produce only the image.");
    text
}

async fn collect_image_generation_result(
    response: reqwest::Response,
) -> Result<GeneratedImage, ProxyError> {
    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    let mut utf8_remainder = Vec::new();
    let mut event_types = Vec::new();

    while let Some(chunk) = stream.next().await {
        let bytes = chunk.map_err(|e| {
            ProxyError::ForwardFailed(format!("Failed to read Codex image SSE: {e}"))
        })?;
        append_utf8_safe(&mut buffer, &mut utf8_remainder, &bytes);

        while let Some(block) = take_sse_block(&mut buffer) {
            if let Some(image) = parse_image_generation_sse_block(&block, &mut event_types)? {
                validate_image_base64(&image.b64_json)?;
                return Ok(image);
            }
        }
    }

    if !buffer.trim().is_empty() {
        let tail = std::mem::take(&mut buffer) + "\n\n";
        buffer.push_str(&tail);
        while let Some(block) = take_sse_block(&mut buffer) {
            if let Some(image) = parse_image_generation_sse_block(&block, &mut event_types)? {
                validate_image_base64(&image.b64_json)?;
                return Ok(image);
            }
        }
    }

    let events_seen = if event_types.is_empty() {
        "(none)".to_string()
    } else {
        event_types.sort();
        event_types.dedup();
        event_types.join(", ")
    };

    Err(ProxyError::UpstreamError {
        status: 502,
        body: Some(format!(
            "Codex image generation finished without image_generation_call.result; events seen: {events_seen}"
        )),
    })
}

fn parse_image_generation_sse_block(
    block: &str,
    event_types: &mut Vec<String>,
) -> Result<Option<GeneratedImage>, ProxyError> {
    let Some(event) = parse_sse_json_block(block, event_types)? else {
        return Ok(None);
    };
    image_from_codex_event(&event)
}

enum ImageGenerationStreamEvent {
    Progress(&'static str),
    Completed(GeneratedImage),
}

fn image_generation_stream_event_from_block(
    block: &str,
    event_types: &mut Vec<String>,
) -> Result<Option<ImageGenerationStreamEvent>, ProxyError> {
    let Some(event) = parse_sse_json_block(block, event_types)? else {
        return Ok(None);
    };
    if let Some(image) = image_from_codex_event(&event)? {
        return Ok(Some(ImageGenerationStreamEvent::Completed(image)));
    }
    let progress = match event.get("type").and_then(Value::as_str) {
        Some("response.image_generation_call.in_progress") => Some("image_generation.queued"),
        Some("response.image_generation_call.generating") => Some("image_generation.generating"),
        Some("response.image_generation_call.partial_image") => {
            Some("image_generation.partial_image")
        }
        _ => None,
    };
    Ok(progress.map(ImageGenerationStreamEvent::Progress))
}

fn parse_sse_json_block(
    block: &str,
    event_types: &mut Vec<String>,
) -> Result<Option<Value>, ProxyError> {
    let data_lines: Vec<&str> = block
        .lines()
        .filter_map(|line| strip_sse_field(line, "data"))
        .collect();
    if data_lines.is_empty() {
        return Ok(None);
    }

    let data = data_lines.join("\n");
    if data.trim() == "[DONE]" {
        return Ok(None);
    }

    let event: Value = serde_json::from_str(&data).map_err(|e| {
        ProxyError::TransformError(format!("Failed to parse Codex image SSE event: {e}"))
    })?;
    if let Some(event_type) = event.get("type").and_then(Value::as_str) {
        event_types.push(event_type.to_string());
    }

    Ok(Some(event))
}

fn image_from_codex_event(event: &Value) -> Result<Option<GeneratedImage>, ProxyError> {
    if event.get("type").and_then(Value::as_str) != Some("response.output_item.done") {
        return Ok(None);
    }
    let item = event.get("item").and_then(Value::as_object);
    let Some(item) = item else {
        return Ok(None);
    };
    if item.get("type").and_then(Value::as_str) != Some("image_generation_call") {
        return Ok(None);
    }
    let Some(result) = item.get("result").and_then(Value::as_str) else {
        return Ok(None);
    };

    Ok(Some(GeneratedImage {
        b64_json: result.to_string(),
        revised_prompt: item
            .get("revised_prompt")
            .and_then(Value::as_str)
            .map(ToString::to_string),
    }))
}

fn sse_event_bytes(event: &str, data: Value) -> Bytes {
    let data = serde_json::to_string(&data).unwrap_or_else(|_| "{}".to_string());
    Bytes::from(format!("event: {event}\ndata: {data}\n\n"))
}

fn sse_error_event_bytes(message: &str) -> Bytes {
    sse_event_bytes(
        "error",
        json!({
            "error": {
                "message": message,
                "type": "upstream_error",
            }
        }),
    )
}

fn validate_image_base64(value: &str) -> Result<(), ProxyError> {
    base64::engine::general_purpose::STANDARD
        .decode(value)
        .map(|_| ())
        .map_err(|_| ProxyError::UpstreamError {
            status: 502,
            body: Some(
                "Codex image backend returned invalid base64 in image_generation_call.result"
                    .to_string(),
            ),
        })
}

fn truncate_for_error(mut value: String, limit: usize) -> String {
    if value.len() <= limit {
        return value;
    }

    let mut end = limit;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
    value.push_str("...");
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_generation_uses_current_codex_cli_identity() {
        assert_eq!(CODEX_CLI_VERSION, "0.139.0");
        let user_agent = codex_imagegen_user_agent();
        assert!(user_agent.starts_with("codex_cli_rs/0.139.0"));
        assert!(user_agent.contains("cc-switch image generation"));
    }

    #[test]
    fn validate_request_defaults_to_b64_png_single_image() {
        let parsed = validate_image_request(OpenAiImageGenerationRequest {
            model: None,
            prompt: " a city at dawn ".to_string(),
            n: None,
            size: Some("1024x1024".to_string()),
            response_format: Some("b64_json".to_string()),
            output_format: None,
            quality: None,
            style: None,
            background: None,
            stream: None,
            partial_images: None,
        })
        .expect("valid request");

        assert_eq!(parsed.model, CODEX_IMAGE_MODEL_DEFAULT);
        assert_eq!(parsed.prompt, "a city at dawn");
        assert_eq!(parsed.size.as_deref(), Some("1024x1024"));
        assert_eq!(parsed.output_format, "png");
    }

    #[test]
    fn validate_request_rejects_url_response_format() {
        let err = validate_image_request(OpenAiImageGenerationRequest {
            model: None,
            prompt: "test".to_string(),
            n: Some(1),
            size: None,
            response_format: Some("url".to_string()),
            output_format: None,
            quality: None,
            style: None,
            background: None,
            stream: None,
            partial_images: None,
        })
        .expect_err("url response format should fail");

        assert!(matches!(err, ProxyError::InvalidRequest(_)));
    }

    #[test]
    fn build_payload_uses_image_generation_tool() {
        let parsed = validate_image_request(OpenAiImageGenerationRequest {
            model: Some("gpt-5.5".to_string()),
            prompt: "draw a small cabin".to_string(),
            n: None,
            size: Some("auto".to_string()),
            response_format: None,
            output_format: Some("webp".to_string()),
            quality: Some("medium".to_string()),
            style: None,
            background: None,
            stream: Some(true),
            partial_images: Some(0),
        })
        .expect("valid request");

        let payload = build_codex_image_payload(&parsed);
        assert_eq!(payload["stream"], true);
        assert_eq!(payload["tools"][0]["type"], "image_generation");
        assert_eq!(payload["tools"][0]["output_format"], "webp");
        assert!(payload["tools"][0].get("size").is_none());
    }

    #[test]
    fn parses_image_generation_sse_result() {
        let mut event_types = Vec::new();
        let block = r#"event: response.output_item.done
data: {"type":"response.output_item.done","item":{"type":"image_generation_call","result":"aGVsbG8=","revised_prompt":"hello image"}}"#;

        let image = parse_image_generation_sse_block(block, &mut event_types)
            .expect("parse")
            .expect("image");

        assert_eq!(image.b64_json, "aGVsbG8=");
        assert_eq!(image.revised_prompt.as_deref(), Some("hello image"));
        validate_image_base64(&image.b64_json).expect("valid base64");
    }

    #[test]
    fn truncate_for_error_preserves_utf8_boundaries() {
        assert_eq!(truncate_for_error("你好世界".to_string(), 5), "你...");
    }
}
