//! 响应处理器模块
//!
//! 统一处理流式和非流式 API 响应

use super::{
    content_encoding::{decompress_body, get_content_encoding},
    forwarder::ActiveConnectionGuard,
    handler_config::{StreamUsageEventFilter, UsageParserConfig},
    handler_context::{RequestContext, StreamingTimeoutConfig},
    hyper_client::ProxyResponse,
    server::ProxyState,
    sse::{strip_sse_field, take_sse_block},
    usage::parser::TokenUsage,
    ProxyError,
};
use crate::database::PRICING_SOURCE_REQUEST;
use axum::http::{header::HeaderMap, HeaderName};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use chrono::Utc;
use futures::stream::{Stream, StreamExt};
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::Mutex;

// ============================================================================
// 响应头处理
// ============================================================================

/// RFC 2616 / RFC 7230 中定义的不应被代理继续转发的响应头。
const HOP_BY_HOP_RESPONSE_HEADERS: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "trailer",
    "trailers",
    "transfer-encoding",
    "upgrade",
];

/// 移除响应侧 hop-by-hop 头，以及 `Connection` 中点名的扩展头。
pub(crate) fn strip_hop_by_hop_response_headers(headers: &mut HeaderMap) {
    let connection_listed_headers: Vec<HeaderName> = headers
        .get_all(axum::http::header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .filter_map(|name| HeaderName::from_bytes(name.as_bytes()).ok())
        .collect();

    for name in HOP_BY_HOP_RESPONSE_HEADERS {
        headers.remove(*name);
    }

    for name in connection_listed_headers {
        headers.remove(name);
    }
}

/// 移除在重建响应体后会失真的实体头。
pub(crate) fn strip_entity_headers_for_rebuilt_body(headers: &mut HeaderMap) {
    headers.remove(axum::http::header::CONTENT_ENCODING);
    headers.remove(axum::http::header::CONTENT_LENGTH);
    headers.remove(axum::http::header::TRANSFER_ENCODING);
}

/// 读取响应体并在需要时解压，确保 headers 与返回 body 一致。
///
/// `body_timeout`: 整包超时。当非零时用 `tokio::time::timeout` 包住 `.bytes()` 调用，
/// 防止上游发完响应头后卡住 body 导致请求永远挂住。
/// 传入 `Duration::ZERO` 表示不启用超时（故障转移关闭时）。
pub(crate) async fn read_decoded_body(
    response: ProxyResponse,
    tag: &str,
    body_timeout: Duration,
) -> Result<(HeaderMap, http::StatusCode, Bytes), ProxyError> {
    let mut headers = response.headers().clone();
    let status = response.status();
    let raw_bytes = if body_timeout.is_zero() {
        response.bytes().await?
    } else {
        tokio::time::timeout(body_timeout, response.bytes())
            .await
            .map_err(|_| {
                ProxyError::Timeout(format!(
                    "响应体读取超时: {}s（上游发完响应头后 body 未到达）",
                    body_timeout.as_secs()
                ))
            })??
    };

    log::debug!(
        "[{tag}] 已接收上游响应体: status={}, bytes={}, headers={}",
        status.as_u16(),
        raw_bytes.len(),
        format_headers(&headers)
    );

    let mut body_bytes = raw_bytes.clone();
    let mut decoded = false;

    if let Some(encoding) = get_content_encoding(&headers) {
        log::debug!("[{tag}] 解压非流式响应: content-encoding={encoding}");
        match decompress_body(&encoding, &raw_bytes) {
            Ok(Some(decompressed)) => {
                body_bytes = Bytes::from(decompressed);
                decoded = true;
            }
            // 不支持的编码：原样透传且保留 content-encoding 头，
            // 让下游诊断/客户端知道这仍是压缩字节
            Ok(None) => {}
            Err(e) => {
                log::warn!("[{tag}] 解压失败 ({encoding}): {e}，使用原始数据");
            }
        }
    }

    if decoded {
        strip_entity_headers_for_rebuilt_body(&mut headers);
    }

    Ok((headers, status, body_bytes))
}

// ============================================================================
// 公共接口
// ============================================================================

/// 检测响应是否为 SSE 流式响应
#[inline]
pub fn is_sse_response(response: &ProxyResponse) -> bool {
    response
        .content_type()
        .map(|ct| ct.contains("text/event-stream"))
        .unwrap_or(false)
}

#[inline]
fn should_handle_as_streaming_response(response: &ProxyResponse, ctx: &RequestContext) -> bool {
    is_sse_response(response) || ctx.request_is_streaming
}

#[cfg(test)]
fn should_handle_as_streaming_response_parts(
    content_type: Option<&str>,
    request_is_streaming: bool,
) -> bool {
    content_type
        .map(|ct| ct.contains("text/event-stream"))
        .unwrap_or(false)
        || request_is_streaming
}

/// 处理流式响应
pub async fn handle_streaming(
    response: ProxyResponse,
    ctx: &RequestContext,
    state: &ProxyState,
    parser_config: &UsageParserConfig,
    connection_guard: Option<ActiveConnectionGuard>,
) -> Response {
    let status = response.status();
    log::debug!(
        "[{}] 已接收上游流式响应: status={}, headers={}",
        ctx.tag,
        status.as_u16(),
        format_headers(response.headers())
    );
    // 检查流式响应是否被压缩（SSE 通常不压缩，如果压缩则 SSE 解析会失败）
    if let Some(encoding) = get_content_encoding(response.headers()) {
        log::warn!(
            "[{}] 流式响应含 content-encoding={encoding}，SSE 解析可能失败。\
             上游在 accept-encoding 透传后压缩了 SSE 流。",
            ctx.tag
        );
    }

    let mut response_headers = response.headers().clone();
    strip_hop_by_hop_response_headers(&mut response_headers);

    let mut builder = axum::response::Response::builder().status(status);

    // 复制响应头
    for (key, value) in &response_headers {
        builder = builder.header(key, value);
    }
    if !response_headers.contains_key(axum::http::header::CONTENT_TYPE) {
        builder = builder.header(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("text/event-stream"),
        );
    }

    // Locally generated SSE (Cursor AgentService) manages its own stall
    // deadlines; the generic idle timeout would RST the HTTP/2 stream mid-turn.
    let timeout_config = if response.is_local_stream() {
        StreamingTimeoutConfig {
            first_byte_timeout: 0,
            idle_timeout: 0,
        }
    } else {
        ctx.streaming_timeout_config()
    };

    // 创建字节流
    let stream = response.bytes_stream();

    // 创建使用量收集器；关闭 usage logging 时不要在流式热路径上解析每个 SSE event。
    let usage_collector = create_usage_collector(ctx, state, status.as_u16(), parser_config);

    // 创建带日志和超时的透传流
    let logged_stream = create_logged_passthrough_stream(
        stream,
        ctx.tag,
        usage_collector,
        timeout_config,
        connection_guard,
        terminal_fallback_for_context(ctx),
    );

    let body = axum::body::Body::from_stream(logged_stream);
    match builder.body(body) {
        Ok(resp) => resp,
        Err(e) => {
            log::error!("[{}] 构建流式响应失败: {e}", ctx.tag);
            ProxyError::Internal(format!("Failed to build streaming response: {e}")).into_response()
        }
    }
}

fn terminal_fallback_for_context(ctx: &RequestContext) -> SseTerminalFallback {
    match ctx.app_type_str {
        "codex" => SseTerminalFallback::OpenAiResponses,
        "claude" | "claude-desktop" => SseTerminalFallback::AnthropicMessages,
        _ => SseTerminalFallback::Disabled,
    }
}

/// 处理非流式响应
pub async fn handle_non_streaming(
    response: ProxyResponse,
    ctx: &RequestContext,
    state: &ProxyState,
    parser_config: &UsageParserConfig,
    // guard 在函数 scope 内持有，整包响应读取完成后随函数返回一并 drop
    _connection_guard: Option<ActiveConnectionGuard>,
) -> Result<Response, ProxyError> {
    // 整包超时：仅在故障转移开启且配置值非零时生效
    let body_timeout =
        if ctx.app_config.auto_failover_enabled && ctx.app_config.non_streaming_timeout > 0 {
            Duration::from_secs(ctx.app_config.non_streaming_timeout as u64)
        } else {
            Duration::ZERO
        };
    let (mut response_headers, status, body_bytes) =
        read_decoded_body(response, ctx.tag, body_timeout).await?;
    strip_hop_by_hop_response_headers(&mut response_headers);

    log::debug!(
        "[{}] 上游响应体内容: {}",
        ctx.tag,
        String::from_utf8_lossy(&body_bytes)
    );

    // 解析并记录使用量。关闭 usage logging 时直接跳过，避免非流式响应整包 JSON parse。
    if usage_logging_enabled(state) {
        if let Ok(json_value) = serde_json::from_slice::<Value>(&body_bytes) {
            // 解析使用量
            if let Some(usage) = (parser_config.response_parser)(&json_value) {
                // 归因优先级：明确的出站模型（模型映射真值）→ usage 模型 →
                // 响应 model 字段 → 客户端请求模型。Cursor 会为了客户端兼容回显
                // 请求模型，因此 outbound_model 必须优先。
                let model = ctx
                    .outbound_model
                    .clone()
                    .or_else(|| usage.model.clone().filter(|m| !m.is_empty()))
                    .or_else(|| {
                        json_value
                            .get("model")
                            .and_then(|m| m.as_str())
                            .filter(|m| !m.is_empty())
                            .map(str::to_string)
                    })
                    .unwrap_or_else(|| ctx.request_model.clone());

                spawn_log_usage(
                    state,
                    ctx,
                    usage,
                    &model,
                    &ctx.request_model,
                    status.as_u16(),
                    false,
                );
            } else {
                let model = ctx
                    .outbound_model
                    .clone()
                    .or_else(|| {
                        json_value
                            .get("model")
                            .and_then(|m| m.as_str())
                            .filter(|m| !m.is_empty())
                            .map(str::to_string)
                    })
                    .unwrap_or_else(|| ctx.request_model.clone());
                spawn_log_usage(
                    state,
                    ctx,
                    TokenUsage::default(),
                    &model,
                    &ctx.request_model,
                    status.as_u16(),
                    false,
                );
                log::debug!(
                    "[{}] 未能解析 usage 信息，跳过记录",
                    parser_config.app_type_str
                );
            }
        } else {
            log::debug!(
                "[{}] <<< 响应 (非 JSON): {} bytes",
                ctx.tag,
                body_bytes.len()
            );
            spawn_log_usage(
                state,
                ctx,
                TokenUsage::default(),
                ctx.outbound_model.as_deref().unwrap_or(&ctx.request_model),
                &ctx.request_model,
                status.as_u16(),
                false,
            );
        }
    } else {
        log::debug!("[{}] usage logging 已关闭，跳过非流式 usage 解析", ctx.tag);
    }

    // 构建响应
    let mut builder = axum::response::Response::builder().status(status);
    for (key, value) in response_headers.iter() {
        builder = builder.header(key, value);
    }

    let body = axum::body::Body::from(body_bytes);
    builder.body(body).map_err(|e| {
        log::error!("[{}] 构建响应失败: {e}", ctx.tag);
        ProxyError::Internal(format!("Failed to build response: {e}"))
    })
}

/// 通用响应处理入口
///
/// 根据响应类型自动选择流式或非流式处理
pub async fn process_response(
    response: ProxyResponse,
    ctx: &RequestContext,
    state: &ProxyState,
    parser_config: &UsageParserConfig,
    connection_guard: Option<ActiveConnectionGuard>,
) -> Result<Response, ProxyError> {
    if should_handle_as_streaming_response(&response, ctx) {
        Ok(handle_streaming(response, ctx, state, parser_config, connection_guard).await)
    } else {
        handle_non_streaming(response, ctx, state, parser_config, connection_guard).await
    }
}

// ============================================================================
// SSE 使用量收集器
// ============================================================================

type UsageCallbackWithTiming = Arc<dyn Fn(Vec<Value>, Option<u64>) + Send + Sync + 'static>;

/// SSE 使用量收集器
#[derive(Clone)]
pub struct SseUsageCollector {
    inner: Arc<SseUsageCollectorInner>,
}

struct SseUsageCollectorInner {
    events: Mutex<Vec<Value>>,
    first_event_time: Mutex<Option<std::time::Instant>>,
    first_event_set: AtomicBool,
    start_time: std::time::Instant,
    on_complete: UsageCallbackWithTiming,
    should_collect: Option<StreamUsageEventFilter>,
    finished: AtomicBool,
}

impl SseUsageCollector {
    /// 创建使用量收集器；`should_collect` 用来在 hot path 跳过与 usage 无关的事件。
    pub fn new(
        start_time: std::time::Instant,
        should_collect: Option<StreamUsageEventFilter>,
        callback: impl Fn(Vec<Value>, Option<u64>) + Send + Sync + 'static,
    ) -> Self {
        let on_complete: UsageCallbackWithTiming = Arc::new(callback);
        Self {
            inner: Arc::new(SseUsageCollectorInner {
                events: Mutex::new(Vec::new()),
                first_event_time: Mutex::new(None),
                first_event_set: AtomicBool::new(false),
                start_time,
                on_complete,
                should_collect,
                finished: AtomicBool::new(false),
            }),
        }
    }

    pub fn should_collect(&self, data: &str) -> bool {
        self.inner
            .should_collect
            .map(|filter| filter(data))
            .unwrap_or(true)
    }

    /// 标记首个被收集的 SSE 事件时间，沿用 `first_token_ms` 的既有近似语义。
    async fn mark_first_collected_event_time(&self) {
        if self.inner.first_event_set.load(Ordering::Acquire) {
            return;
        }
        let mut first_time = self.inner.first_event_time.lock().await;
        if first_time.is_none() {
            *first_time = Some(std::time::Instant::now());
            self.inner.first_event_set.store(true, Ordering::Release);
        }
    }

    /// 推送 SSE 事件
    pub async fn push(&self, event: Value) {
        self.mark_first_collected_event_time().await;
        let mut events = self.inner.events.lock().await;
        events.push(event);
    }

    /// 完成收集并触发回调
    pub async fn finish(&self) {
        if self.inner.finished.swap(true, Ordering::SeqCst) {
            return;
        }

        let events = {
            let mut guard = self.inner.events.lock().await;
            std::mem::take(&mut *guard)
        };

        let first_token_ms = {
            let first_time = self.inner.first_event_time.lock().await;
            first_time.map(|t| (t - self.inner.start_time).as_millis() as u64)
        };

        (self.inner.on_complete)(events, first_token_ms);
    }
}

struct SseUsageFinishGuard {
    collector: Option<SseUsageCollector>,
}

impl SseUsageFinishGuard {
    fn new(collector: SseUsageCollector) -> Self {
        Self {
            collector: Some(collector),
        }
    }

    fn disarm(&mut self) {
        self.collector = None;
    }
}

impl Drop for SseUsageFinishGuard {
    fn drop(&mut self) {
        if let Some(collector) = self.collector.take() {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    collector.finish().await;
                });
            } else {
                log::warn!("SSE 用量收尾保护触发时 Tokio runtime 不可用，跳过异步 finish");
            }
        }
    }
}

// ============================================================================
// 内部辅助函数
// ============================================================================

/// 创建使用量收集器
fn create_usage_collector(
    ctx: &RequestContext,
    state: &ProxyState,
    status_code: u16,
    parser_config: &UsageParserConfig,
) -> Option<SseUsageCollector> {
    let logging_enabled = state
        .config
        .try_read()
        .map(|c| c.enable_logging)
        .unwrap_or(true);
    let should_log_request = logging_enabled || ctx.share_id.is_some();
    if !should_log_request {
        return None;
    }

    let state = state.clone();
    let provider_id = ctx.provider.id.clone();
    let provider_name = ctx.provider.name.clone();
    let request_model = ctx.request_model.clone();
    // 流式事件缺失模型名时的归因兜底：映射后的出站模型（路由接管真值）优先，
    // 其次才是客户端请求别名
    let fallback_model = ctx
        .outbound_model
        .clone()
        .unwrap_or_else(|| ctx.request_model.clone());
    let forced_outbound_model = ctx.outbound_model.clone();
    // 用 ctx 的 app_type 而不是 parser_config 的：Claude Desktop 流式透传复用
    // CLAUDE_PARSER_CONFIG（app_type_str="claude"），按 parser_config 记账会把
    // claude-desktop 的行错记到 claude 名下，导致供应商计价覆盖解析不到。
    let app_type_str = ctx.app_type_str;
    let tag = ctx.tag;
    let start_time = ctx.start_time;
    let stream_parser = parser_config.stream_parser;
    let model_extractor = parser_config.model_extractor;
    let session_id = ctx.session_id.clone();
    let share_id = ctx.share_id.clone();
    let share_name = ctx.share_name.clone();
    let share_user_email = ctx.share_user_email.clone();
    let share_user_country = ctx.share_user_country.clone();
    let share_user_country_iso3 = ctx.share_user_country_iso3.clone();
    let incoming_request_id = ctx.incoming_request_id.clone();

    Some(SseUsageCollector::new(
        start_time,
        parser_config.stream_event_filter,
        move |events, first_token_ms| {
            if let Some(usage) = stream_parser(&events) {
                let model = forced_outbound_model
                    .clone()
                    .unwrap_or_else(|| model_extractor(&events, &fallback_model));
                let latency_ms = start_time.elapsed().as_millis() as u64;

                let state = state.clone();
                let provider_id = provider_id.clone();
                let session_id = session_id.clone();
                let request_model = request_model.clone();
                let share_id = share_id.clone();
                let share_name = share_name.clone();
                let share_user_email = share_user_email.clone();
                let share_user_country = share_user_country.clone();
                let share_user_country_iso3 = share_user_country_iso3.clone();
                let incoming_request_id = incoming_request_id.clone();
                let outbound_model = fallback_model.clone();
                let total_tokens = share_total_tokens(app_type_str, &usage);
                let request_id = incoming_request_id
                    .clone()
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                let usage_for_log = usage.clone();
                let usage_for_sync = usage.clone();
                let session_id_for_log = session_id.clone();
                let session_id_for_sync = session_id.clone();
                let share_name_for_log = share_name.clone();
                let share_name_for_sync = share_name.clone();
                let share_user_email_for_log = share_user_email.clone();
                let share_user_email_for_sync = share_user_email.clone();
                let share_user_country_for_sync = share_user_country.clone();
                let share_user_country_iso3_for_sync = share_user_country_iso3.clone();
                let provider_name_for_sync = provider_name.clone();

                tokio::spawn(async move {
                    if logging_enabled {
                        log_usage_internal(
                            &state,
                            &request_id,
                            &provider_id,
                            app_type_str,
                            &model,
                            &request_model,
                            &outbound_model,
                            usage_for_log,
                            latency_ms,
                            first_token_ms,
                            true, // is_streaming
                            status_code,
                            Some(session_id_for_log),
                            share_id.clone(),
                            share_name_for_log,
                            share_user_email_for_log,
                        )
                        .await;
                    }
                    if let Some(sid) = share_id {
                        crate::tunnel::sync::schedule_sync_share_request_log(
                            build_share_request_log(
                                &request_id,
                                &sid,
                                share_name_for_sync.as_deref().unwrap_or(""),
                                &provider_id,
                                &provider_name_for_sync,
                                app_type_str,
                                &model,
                                &request_model,
                                status_code,
                                latency_ms,
                                first_token_ms,
                                &usage_for_sync,
                                true,
                                Some(session_id_for_sync),
                                share_user_email_for_sync,
                                share_user_country_for_sync,
                                share_user_country_iso3_for_sync,
                            ),
                        );
                        crate::proxy::share_guard::record_share_request(
                            &state.db,
                            &sid,
                            total_tokens,
                        );
                    }
                });
            } else {
                let model = forced_outbound_model
                    .clone()
                    .unwrap_or_else(|| model_extractor(&events, &fallback_model));
                let latency_ms = start_time.elapsed().as_millis() as u64;
                let state = state.clone();
                let provider_id = provider_id.clone();
                let session_id = session_id.clone();
                let request_model = request_model.clone();
                let share_id = share_id.clone();
                let share_name = share_name.clone();
                let share_user_email = share_user_email.clone();
                let share_user_country = share_user_country.clone();
                let share_user_country_iso3 = share_user_country_iso3.clone();
                let incoming_request_id = incoming_request_id.clone();
                let outbound_model = fallback_model.clone();
                let request_id = incoming_request_id
                    .clone()
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                let session_id_for_log = session_id.clone();
                let session_id_for_sync = session_id.clone();
                let share_name_for_log = share_name.clone();
                let share_name_for_sync = share_name.clone();
                let share_user_email_for_log = share_user_email.clone();
                let share_user_email_for_sync = share_user_email.clone();
                let share_user_country_for_sync = share_user_country.clone();
                let share_user_country_iso3_for_sync = share_user_country_iso3.clone();
                let provider_name_for_sync = provider_name.clone();

                tokio::spawn(async move {
                    if logging_enabled {
                        log_usage_internal(
                            &state,
                            &request_id,
                            &provider_id,
                            app_type_str,
                            &model,
                            &request_model,
                            &outbound_model,
                            TokenUsage::default(),
                            latency_ms,
                            first_token_ms,
                            true, // is_streaming
                            status_code,
                            Some(session_id_for_log),
                            share_id.clone(),
                            share_name_for_log,
                            share_user_email_for_log,
                        )
                        .await;
                    }
                    if let Some(sid) = share_id {
                        crate::tunnel::sync::schedule_sync_share_request_log(
                            build_share_request_log(
                                &request_id,
                                &sid,
                                share_name_for_sync.as_deref().unwrap_or(""),
                                &provider_id,
                                &provider_name_for_sync,
                                app_type_str,
                                &model,
                                &request_model,
                                status_code,
                                latency_ms,
                                first_token_ms,
                                &TokenUsage::default(),
                                true,
                                Some(session_id_for_sync),
                                share_user_email_for_sync,
                                share_user_country_for_sync,
                                share_user_country_iso3_for_sync,
                            ),
                        );
                        crate::proxy::share_guard::record_share_request(&state.db, &sid, 0);
                    }
                });
                log::debug!("[{tag}] 流式响应缺少 usage 统计，跳过消费记录");
            }
        },
    ))
}

/// 异步记录使用量
fn spawn_log_usage(
    state: &ProxyState,
    ctx: &RequestContext,
    usage: TokenUsage,
    model: &str,
    request_model: &str,
    status_code: u16,
    is_streaming: bool,
) {
    let enable_logging = state
        .config
        .try_read()
        .map(|config| config.enable_logging)
        .unwrap_or(true);

    let state = state.clone();
    let provider_id = ctx.provider.id.clone();
    let provider_name = ctx.provider.name.clone();
    let app_type_str = ctx.app_type_str.to_string();
    let model = model.to_string();
    let request_model = request_model.to_string();
    // 「按请求计价」模式的锚点：映射后的出站模型，无映射时等于 request_model
    let outbound_model = ctx
        .outbound_model
        .clone()
        .unwrap_or_else(|| ctx.request_model.clone());
    let latency_ms = ctx.latency_ms();
    let session_id = ctx.session_id.clone();
    let share_id = ctx.share_id.clone();
    let share_name = ctx.share_name.clone();
    let share_user_email = ctx.share_user_email.clone();
    let share_user_country = ctx.share_user_country.clone();
    let share_user_country_iso3 = ctx.share_user_country_iso3.clone();
    let incoming_request_id = ctx.incoming_request_id.clone();
    let total_tokens = share_total_tokens(&app_type_str, &usage);
    let should_log_request = enable_logging || share_id.is_some();
    let request_id = incoming_request_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let usage_for_log = usage.clone();
    let usage_for_sync = usage.clone();
    let session_id_for_log = session_id.clone();
    let session_id_for_sync = session_id.clone();
    let share_name_for_log = share_name.clone();
    let share_name_for_sync = share_name.clone();
    let share_user_email_for_log = share_user_email.clone();
    let share_user_email_for_sync = share_user_email.clone();
    let share_user_country_for_sync = share_user_country.clone();
    let share_user_country_iso3_for_sync = share_user_country_iso3.clone();

    tokio::spawn(async move {
        if should_log_request {
            log_usage_internal(
                &state,
                &request_id,
                &provider_id,
                &app_type_str,
                &model,
                &request_model,
                &outbound_model,
                usage_for_log,
                latency_ms,
                None,
                is_streaming,
                status_code,
                Some(session_id_for_log),
                share_id.clone(),
                share_name_for_log,
                share_user_email_for_log,
            )
            .await;
        }

        // 共享 Token：请求成功进入本地代理后，总是记录一次请求；若解析到 usage，再累计 tokens。
        if let Some(sid) = share_id {
            crate::tunnel::sync::schedule_sync_share_request_log(build_share_request_log(
                &request_id,
                &sid,
                share_name_for_sync.as_deref().unwrap_or(""),
                &provider_id,
                &provider_name,
                &app_type_str,
                &model,
                &request_model,
                status_code,
                latency_ms,
                None,
                &usage_for_sync,
                is_streaming,
                Some(session_id_for_sync),
                share_user_email_for_sync,
                share_user_country_for_sync,
                share_user_country_iso3_for_sync,
            ));
            crate::proxy::share_guard::record_share_request(&state.db, &sid, total_tokens);
        }
    });
}

pub(crate) fn usage_logging_enabled(state: &ProxyState) -> bool {
    state
        .config
        .try_read()
        .map(|config| config.enable_logging)
        .unwrap_or(true)
}

/// 内部使用量记录函数
///
/// `outbound_model` 是「按请求计价」模式的锚点：实际发往上游的模型
/// （路由接管映射后的真值，无映射时等于 request_model）。该模式的语义是
/// 「按代理发出的请求计价、不信任上游回显」，接管场景下发出的请求模型是
/// 映射后的 Y 而非客户端别名 X，按 X 计价会用错定价表行。
#[allow(clippy::too_many_arguments)]
async fn log_usage_internal(
    state: &ProxyState,
    request_id: &str,
    provider_id: &str,
    app_type: &str,
    model: &str,
    request_model: &str,
    outbound_model: &str,
    usage: TokenUsage,
    latency_ms: u64,
    first_token_ms: Option<u64>,
    is_streaming: bool,
    status_code: u16,
    session_id: Option<String>,
    share_id: Option<String>,
    share_name: Option<String>,
    user_email: Option<String>,
) {
    use super::usage::logger::UsageLogger;

    let logger = UsageLogger::new(&state.db);
    let (multiplier, pricing_model_source) =
        logger.resolve_pricing_config(provider_id, app_type).await;
    let pricing_model = if pricing_model_source == PRICING_SOURCE_REQUEST {
        outbound_model
    } else {
        model
    };

    let request_id = if request_id.trim().is_empty() {
        usage.dedup_request_id()
    } else {
        request_id.to_string()
    };

    log::debug!(
        "[{app_type}] 记录请求日志: id={request_id}, provider={provider_id}, model={model}, streaming={is_streaming}, status={status_code}, latency_ms={latency_ms}, first_token_ms={first_token_ms:?}, session={}, input={}, output={}, cache_read={}, cache_creation={}",
        session_id.as_deref().unwrap_or("none"),
        usage.input_tokens,
        usage.output_tokens,
        usage.cache_read_tokens,
        usage.cache_creation_tokens
    );

    if let Err(e) = logger.log_with_calculation(
        request_id.to_string(),
        provider_id.to_string(),
        app_type.to_string(),
        model.to_string(),
        request_model.to_string(),
        pricing_model.to_string(),
        usage,
        multiplier,
        latency_ms,
        first_token_ms,
        status_code,
        session_id,
        None, // provider_type
        is_streaming,
        share_id,
        share_name,
        user_email,
    ) {
        log::warn!("[USG-001] 记录使用量失败: {e}");
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_share_request_log(
    request_id: &str,
    share_id: &str,
    share_name: &str,
    provider_id: &str,
    provider_name: &str,
    app_type: &str,
    model: &str,
    request_model: &str,
    status_code: u16,
    latency_ms: u64,
    first_token_ms: Option<u64>,
    usage: &TokenUsage,
    is_streaming: bool,
    session_id: Option<String>,
    user_email: Option<String>,
    user_country: Option<String>,
    user_country_iso3: Option<String>,
) -> crate::tunnel::config::ShareTunnelRequestLog {
    let usage = share_usage_for_display(app_type, usage);

    crate::tunnel::config::ShareTunnelRequestLog {
        request_id: request_id.to_string(),
        share_id: share_id.to_string(),
        share_name: share_name.to_string(),
        provider_id: provider_id.to_string(),
        provider_name: provider_name.to_string(),
        app_type: app_type.to_string(),
        model: model.to_string(),
        request_model: request_model.to_string(),
        request_agent: app_type.to_string(),
        requested_model: request_model.to_string(),
        actual_model: model.to_string(),
        actual_model_source: if model == request_model {
            "official".to_string()
        } else {
            "provider_mapping".to_string()
        },
        status_code,
        latency_ms,
        first_token_ms,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_read_tokens: usage.cache_read_tokens,
        cache_creation_tokens: usage.cache_creation_tokens,
        is_streaming,
        session_id,
        user_email,
        user_country,
        user_country_iso3,
        created_at: Utc::now().timestamp(),
    }
}

fn share_total_tokens(app_type: &str, usage: &TokenUsage) -> i64 {
    let usage = share_usage_for_display(app_type, usage);
    i64::from(usage.input_tokens)
        + i64::from(usage.output_tokens)
        + i64::from(usage.cache_read_tokens)
        + i64::from(usage.cache_creation_tokens)
}

fn share_usage_for_display(app_type: &str, usage: &TokenUsage) -> TokenUsage {
    if !matches!(app_type, "codex" | "gemini") {
        return usage.clone();
    }

    let mut normalized = usage.clone();
    if usage.input_tokens >= usage.cache_read_tokens {
        normalized.input_tokens = usage.input_tokens - usage.cache_read_tokens;
    }
    normalized
}

/// 创建带日志记录和超时控制的透传流
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SseTerminalFallback {
    Disabled,
    OpenAiResponses,
    AnthropicMessages,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ObservedSseKind {
    OpenAiResponses,
    AnthropicMessages,
    OpenAiChat,
}

struct SseTerminationTracker {
    fallback: SseTerminalFallback,
    observed: Option<ObservedSseKind>,
    terminal_seen: bool,
    response_id: Option<String>,
}

impl SseTerminationTracker {
    fn new(fallback: SseTerminalFallback) -> Self {
        Self {
            fallback,
            observed: None,
            terminal_seen: false,
            response_id: None,
        }
    }

    fn observe_event(&mut self, event_text: &str) {
        let mut event_name: Option<&str> = None;
        let mut data_lines = Vec::new();
        for line in event_text.lines() {
            if let Some(event) = strip_sse_field(line, "event") {
                let event = event.trim();
                if !event.is_empty() {
                    event_name = Some(event);
                    self.observe_event_name(event);
                }
            } else if let Some(data) = strip_sse_field(line, "data") {
                data_lines.push(data);
            }
        }

        if data_lines.is_empty() {
            return;
        }
        let data = data_lines.join("\n");
        let trimmed = data.trim();
        if trimmed == "[DONE]" {
            self.terminal_seen = true;
            return;
        }
        if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
            self.observe_json(event_name, &value);
        }
    }

    fn observe_event_name(&mut self, event: &str) {
        if event.starts_with("response.") || event == "error" {
            self.observed
                .get_or_insert(ObservedSseKind::OpenAiResponses);
            if matches!(
                event,
                "response.completed" | "response.failed" | "response.done" | "error"
            ) {
                self.terminal_seen = true;
            }
        } else if event.starts_with("message_") || event.starts_with("content_block_") {
            self.observed
                .get_or_insert(ObservedSseKind::AnthropicMessages);
            if event == "message_stop" {
                self.terminal_seen = true;
            }
        }
    }

    fn observe_json(&mut self, event_name: Option<&str>, value: &Value) {
        if value.get("choices").is_some() {
            self.observed.get_or_insert(ObservedSseKind::OpenAiChat);
            if value
                .get("choices")
                .and_then(Value::as_array)
                .is_some_and(|choices| {
                    choices.iter().any(|choice| {
                        choice
                            .get("finish_reason")
                            .is_some_and(|reason| !reason.is_null())
                    })
                })
            {
                self.terminal_seen = true;
            }
        }

        let typ = value.get("type").and_then(Value::as_str).or(event_name);
        if let Some(typ) = typ {
            if typ.starts_with("response.") {
                self.observed
                    .get_or_insert(ObservedSseKind::OpenAiResponses);
                if matches!(
                    typ,
                    "response.completed" | "response.failed" | "response.done"
                ) {
                    self.terminal_seen = true;
                }
            } else if typ.starts_with("message_") || typ.starts_with("content_block_") {
                self.observed
                    .get_or_insert(ObservedSseKind::AnthropicMessages);
                if typ == "message_stop" {
                    self.terminal_seen = true;
                }
            }
        }

        if let Some(id) = value
            .get("response")
            .and_then(|response| response.get("id"))
            .and_then(Value::as_str)
            .or_else(|| value.get("id").and_then(Value::as_str))
            .filter(|id| id.starts_with("resp_"))
        {
            self.response_id = Some(id.to_string());
        }

        let status = value
            .get("response")
            .and_then(|response| response.get("status"))
            .and_then(Value::as_str);
        if matches!(status, Some("completed" | "failed" | "cancelled")) {
            self.terminal_seen = true;
        }
    }

    fn fallback_bytes(&self, reason: &str) -> Option<Bytes> {
        if self.terminal_seen || self.fallback == SseTerminalFallback::Disabled {
            return None;
        }
        let kind = self.observed.or_else(|| match self.fallback {
            SseTerminalFallback::OpenAiResponses => Some(ObservedSseKind::OpenAiResponses),
            SseTerminalFallback::AnthropicMessages => Some(ObservedSseKind::AnthropicMessages),
            SseTerminalFallback::Disabled => None,
        })?;

        let body = match kind {
            ObservedSseKind::OpenAiResponses => {
                let response_id = self
                    .response_id
                    .clone()
                    .unwrap_or_else(|| format!("resp_{}", uuid::Uuid::new_v4().simple()));
                format!(
                    "{}data: [DONE]\n\n",
                    sse_event(
                        "response.failed",
                        json!({
                            "type": "response.failed",
                            "response": {
                                "id": response_id,
                                "object": "response",
                                "status": "failed",
                                "error": {
                                    "type": "stream_error",
                                    "code": "stream_disconnected",
                                    "message": reason,
                                }
                            }
                        })
                    )
                )
            }
            ObservedSseKind::AnthropicMessages => format!(
                "{}{}",
                sse_event(
                    "error",
                    json!({
                        "type": "error",
                        "error": {
                            "type": "upstream_error",
                            "message": reason,
                        }
                    })
                ),
                sse_event("message_stop", json!({ "type": "message_stop" }))
            ),
            ObservedSseKind::OpenAiChat => format!(
                "data: {}\n\ndata: [DONE]\n\n",
                json!({
                    "id": format!("chatcmpl_{}", uuid::Uuid::new_v4().simple()),
                    "object": "chat.completion.chunk",
                    "created": Utc::now().timestamp(),
                    "choices": [{
                        "index": 0,
                        "delta": {},
                        "finish_reason": "stop"
                    }]
                })
            ),
        };
        Some(Bytes::from(body))
    }
}

fn sse_event(event: &str, data: Value) -> String {
    format!("event: {event}\ndata: {data}\n\n")
}

pub fn create_logged_passthrough_stream(
    stream: impl Stream<Item = Result<Bytes, std::io::Error>> + Send + 'static,
    tag: &'static str,
    usage_collector: Option<SseUsageCollector>,
    timeout_config: StreamingTimeoutConfig,
    connection_guard: Option<ActiveConnectionGuard>,
    terminal_fallback: SseTerminalFallback,
) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send {
    async_stream::stream! {
        let _conn_guard = connection_guard;
        let mut buffer = String::new();
        let mut utf8_remainder: Vec<u8> = Vec::new();
        let mut collector = usage_collector;
        let mut finish_guard = collector.clone().map(SseUsageFinishGuard::new);
        let inspect_sse_events =
            collector.is_some() || log::log_enabled!(log::Level::Debug);
        let mut is_first_chunk = true;
        let mut emitted_any_bytes = false;
        let mut termination = SseTerminationTracker::new(terminal_fallback);

        // 超时配置
        let first_byte_timeout = if timeout_config.first_byte_timeout > 0 {
            Some(Duration::from_secs(timeout_config.first_byte_timeout))
        } else {
            None
        };
        let idle_timeout = if timeout_config.idle_timeout > 0 {
            Some(Duration::from_secs(timeout_config.idle_timeout))
        } else {
            None
        };

        tokio::pin!(stream);

        loop {
            // 选择超时时间：首字节超时或静默期超时
            let timeout_duration = if is_first_chunk {
                first_byte_timeout
            } else {
                idle_timeout
            };

            let chunk_result = match timeout_duration {
                Some(duration) => {
                    match tokio::time::timeout(duration, stream.next()).await {
                        Ok(Some(chunk)) => Some(chunk),
                        Ok(None) => None, // 流结束
                        Err(_) => {
                            // 超时
                            let timeout_type = if is_first_chunk { "首字节" } else { "静默期" };
                            log::error!("[{tag}] 流式响应{}超时 ({}秒)", timeout_type, duration.as_secs());
                            if emitted_any_bytes {
                                if let Some(bytes) = termination.fallback_bytes(&format!("stream {timeout_type} timeout before terminal event")) {
                                    yield Ok(bytes);
                                    break;
                                }
                            }
                            yield Err(std::io::Error::other(format!("流式响应{timeout_type}超时")));
                            break;
                        }
                    }
                }
                None => stream.next().await, // 无超时限制
            };

            match chunk_result {
                Some(Ok(bytes)) => {
                    if is_first_chunk {
                        log::debug!(
                            "[{tag}] 已接收上游流式首包: bytes={}",
                            bytes.len()
                        );
                    }
                    is_first_chunk = false;
                    if !bytes.is_empty() {
                        emitted_any_bytes = true;
                    }
                    crate::proxy::sse::append_utf8_safe(&mut buffer, &mut utf8_remainder, &bytes);

                    // 尝试解析并记录完整的 SSE 事件
                    while let Some(event_text) = take_sse_block(&mut buffer) {
                        if !event_text.trim().is_empty() {
                            termination.observe_event(&event_text);
                            // 提取 data 部分；只有 usage collector 存在时才解析 JSON。
                            for line in event_text.lines() {
                                if let Some(data) = strip_sse_field(line, "data") {
                                    if data.trim() != "[DONE]" {
                                        let collected = match &collector {
                                            Some(c) if c.should_collect(data) => {
                                                match serde_json::from_str::<Value>(data) {
                                                    Ok(json_value) => {
                                                        c.push(json_value).await;
                                                        true
                                                    }
                                                    Err(_) => false,
                                                }
                                            }
                                            _ => false,
                                        };
                                        if inspect_sse_events {
                                            if collected {
                                                log::debug!("[{tag}] <<< SSE 事件: {data}");
                                            } else {
                                                log::debug!("[{tag}] <<< SSE 数据: {data}");
                                            }
                                        }
                                    } else if inspect_sse_events {
                                        log::debug!("[{tag}] <<< SSE: [DONE]");
                                    }
                                }
                            }
                        }
                    }

                    yield Ok(bytes);
                }
                Some(Err(e)) => {
                    if emitted_any_bytes {
                        let reason = if is_trailing_sse_transport_close_error(&e) {
                            "stream transport closed before terminal event".to_string()
                        } else {
                            format!("stream error before terminal event: {e}")
                        };
                        if let Some(bytes) = termination.fallback_bytes(&reason) {
                            log::warn!("[{tag}] {reason}");
                            yield Ok(bytes);
                            break;
                        }
                    }
                    if emitted_any_bytes && is_trailing_sse_transport_close_error(&e) {
                        log::warn!(
                            "[{tag}] 上游 SSE 已输出数据后连接以可忽略的传输错误关闭: {e}"
                        );
                        break;
                    }
                    log::error!("[{tag}] 流错误: {e}");
                    yield Err(std::io::Error::other(e.to_string()));
                    break;
                }
                None => {
                    if emitted_any_bytes {
                        if let Some(bytes) = termination.fallback_bytes("stream closed before terminal event") {
                            log::warn!("[{tag}] stream closed before terminal SSE event; synthesizing terminal event");
                            yield Ok(bytes);
                        }
                    }
                    // 流正常结束
                    break;
                }
            }
        }

        if let Some(c) = collector.take() {
            c.finish().await;
        }
        if let Some(guard) = &mut finish_guard {
            guard.disarm();
        }
    }
}

fn is_trailing_sse_transport_close_error(error: &std::io::Error) -> bool {
    let lower = error.to_string().to_ascii_lowercase();
    lower.contains("error decoding response body")
        || (lower.contains("http/2") || lower.contains("h2") || lower.contains("stream"))
            && (lower.contains("internal_error")
                || lower.contains("not closed cleanly")
                || lower.contains("unspecific protocol error")
                || lower.contains("stream error received")
                || lower.contains("reset"))
}

fn format_headers(headers: &HeaderMap) -> String {
    headers
        .iter()
        .map(|(key, value)| {
            let value_str = value.to_str().unwrap_or("<non-utf8>");
            format!("{key}={value_str}")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::Database;
    use crate::error::AppError;
    use crate::provider::ProviderMeta;
    use crate::proxy::failover_switch::FailoverSwitchManager;
    use crate::proxy::provider_router::ProviderRouter;
    use crate::proxy::providers::{
        codex_chat_history::CodexChatHistoryStore, gemini_shadow::GeminiShadowStore,
    };
    use crate::proxy::types::{ProxyConfig, ProxyStatus};
    use rust_decimal::Decimal;
    use std::collections::HashMap;
    use std::str::FromStr;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    #[test]
    fn test_strip_sse_field_accepts_optional_space() {
        assert_eq!(
            super::strip_sse_field("data: {\"ok\":true}", "data"),
            Some("{\"ok\":true}")
        );
        assert_eq!(
            super::strip_sse_field("data:{\"ok\":true}", "data"),
            Some("{\"ok\":true}")
        );
        assert_eq!(
            super::strip_sse_field("event: message_start", "event"),
            Some("message_start")
        );
        assert_eq!(
            super::strip_sse_field("event:message_start", "event"),
            Some("message_start")
        );
        assert_eq!(super::strip_sse_field("id:1", "data"), None);
    }

    #[test]
    fn test_strip_hop_by_hop_response_headers_removes_standard_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::CONNECTION,
            axum::http::HeaderValue::from_static("keep-alive"),
        );
        headers.insert(
            axum::http::header::HeaderName::from_static("keep-alive"),
            axum::http::HeaderValue::from_static("timeout=5"),
        );
        headers.insert(
            axum::http::header::TRANSFER_ENCODING,
            axum::http::HeaderValue::from_static("chunked"),
        );
        headers.insert(
            axum::http::header::HeaderName::from_static("proxy-connection"),
            axum::http::HeaderValue::from_static("keep-alive"),
        );
        headers.insert(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/json"),
        );
        headers.insert(
            axum::http::header::CONTENT_LENGTH,
            axum::http::HeaderValue::from_static("12"),
        );

        strip_hop_by_hop_response_headers(&mut headers);

        assert!(!headers.contains_key(axum::http::header::CONNECTION));
        assert!(!headers.contains_key("keep-alive"));
        assert!(!headers.contains_key(axum::http::header::TRANSFER_ENCODING));
        assert!(!headers.contains_key("proxy-connection"));
        assert_eq!(
            headers.get(axum::http::header::CONTENT_TYPE),
            Some(&axum::http::HeaderValue::from_static("application/json"))
        );
        assert_eq!(
            headers.get(axum::http::header::CONTENT_LENGTH),
            Some(&axum::http::HeaderValue::from_static("12"))
        );
    }

    #[test]
    fn test_strip_hop_by_hop_response_headers_removes_connection_listed_extensions() {
        let mut headers = HeaderMap::new();
        headers.append(
            axum::http::header::CONNECTION,
            axum::http::HeaderValue::from_static("x-trace-hop, x-debug-hop"),
        );
        headers.append(
            axum::http::header::CONNECTION,
            axum::http::HeaderValue::from_static("upgrade"),
        );
        headers.insert(
            axum::http::header::HeaderName::from_static("x-trace-hop"),
            axum::http::HeaderValue::from_static("trace"),
        );
        headers.insert(
            axum::http::header::HeaderName::from_static("x-debug-hop"),
            axum::http::HeaderValue::from_static("debug"),
        );
        headers.insert(
            axum::http::header::UPGRADE,
            axum::http::HeaderValue::from_static("websocket"),
        );
        headers.insert(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("text/event-stream"),
        );

        strip_hop_by_hop_response_headers(&mut headers);

        assert!(!headers.contains_key(axum::http::header::CONNECTION));
        assert!(!headers.contains_key("x-trace-hop"));
        assert!(!headers.contains_key("x-debug-hop"));
        assert!(!headers.contains_key(axum::http::header::UPGRADE));
        assert_eq!(
            headers.get(axum::http::header::CONTENT_TYPE),
            Some(&axum::http::HeaderValue::from_static("text/event-stream"))
        );
    }

    #[test]
    fn test_streaming_detection_uses_request_flag_when_content_type_missing() {
        assert!(should_handle_as_streaming_response_parts(None, true));
        assert!(should_handle_as_streaming_response_parts(
            Some("text/event-stream"),
            false
        ));
        assert!(!should_handle_as_streaming_response_parts(
            Some("application/json"),
            false
        ));
    }

    #[test]
    fn test_share_usage_normalizes_codex_cache_inclusive_input() {
        let usage = TokenUsage {
            input_tokens: 156_541,
            output_tokens: 357,
            cache_read_tokens: 155_520,
            cache_creation_tokens: 0,
            model: None,
            message_id: None,
        };

        let log = build_share_request_log(
            "req-1",
            "share-1",
            "Share",
            "provider-1",
            "Provider",
            "codex",
            "gpt-5.5",
            "gpt-5.5",
            200,
            1000,
            Some(100),
            &usage,
            true,
            Some("session-1".to_string()),
            Some("user@example.com".to_string()),
            Some("JP".to_string()),
            Some("JPN".to_string()),
        );

        assert_eq!(log.input_tokens, 1_021);
        assert_eq!(log.output_tokens, 357);
        assert_eq!(log.cache_read_tokens, 155_520);
        assert_eq!(log.cache_creation_tokens, 0);
        assert_eq!(log.user_country.as_deref(), Some("JP"));
        assert_eq!(log.user_country_iso3.as_deref(), Some("JPN"));
        assert_eq!(share_total_tokens("codex", &usage), 156_898);
    }

    #[test]
    fn test_share_usage_keeps_claude_fresh_input() {
        let usage = TokenUsage {
            input_tokens: 200,
            output_tokens: 100,
            cache_read_tokens: 5_000,
            cache_creation_tokens: 10_000,
            model: None,
            message_id: None,
        };

        let log = build_share_request_log(
            "req-2",
            "share-1",
            "Share",
            "provider-1",
            "Provider",
            "claude",
            "claude-opus-4-6",
            "claude-opus-4-6",
            200,
            1000,
            None,
            &usage,
            false,
            None,
            None,
            None,
            None,
        );

        assert_eq!(log.input_tokens, 200);
        assert_eq!(log.cache_read_tokens, 5_000);
        assert_eq!(share_total_tokens("claude", &usage), 15_300);
    }

    #[test]
    fn test_share_usage_keeps_malformed_cache_inclusive_input() {
        let usage = TokenUsage {
            input_tokens: 100,
            output_tokens: 5,
            cache_read_tokens: 999,
            cache_creation_tokens: 0,
            model: None,
            message_id: None,
        };

        let normalized = share_usage_for_display("codex", &usage);

        assert_eq!(normalized.input_tokens, 100);
        assert_eq!(share_total_tokens("codex", &usage), 1_104);
    }

    fn build_state(db: Arc<Database>) -> ProxyState {
        ProxyState {
            db: db.clone(),
            config: Arc::new(RwLock::new(ProxyConfig::default())),
            status: Arc::new(RwLock::new(ProxyStatus::default())),
            start_time: Arc::new(RwLock::new(None)),
            current_providers: Arc::new(RwLock::new(HashMap::new())),
            provider_router: Arc::new(ProviderRouter::new(db.clone())),
            gemini_shadow: Arc::new(GeminiShadowStore::default()),
            codex_chat_history: Arc::new(CodexChatHistoryStore::default()),
            app_handle: None,
            failover_manager: Arc::new(FailoverSwitchManager::new(db)),
        }
    }

    fn seed_pricing(db: &Database) -> Result<(), AppError> {
        let conn = crate::database::lock_conn!(db.conn);
        conn.execute(
            "INSERT OR REPLACE INTO model_pricing (model_id, display_name, input_cost_per_million, output_cost_per_million)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params!["resp-model", "Resp Model", "1.0", "0"],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        conn.execute(
            "INSERT OR REPLACE INTO model_pricing (model_id, display_name, input_cost_per_million, output_cost_per_million)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params!["req-model", "Req Model", "2.0", "0"],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    fn insert_provider(
        db: &Database,
        id: &str,
        app_type: &str,
        meta: ProviderMeta,
    ) -> Result<(), AppError> {
        let meta_json =
            serde_json::to_string(&meta).map_err(|e| AppError::Database(e.to_string()))?;
        let conn = crate::database::lock_conn!(db.conn);
        conn.execute(
            "INSERT INTO providers (id, app_type, name, settings_config, meta)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![id, app_type, "Test Provider", "{}", meta_json],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    #[tokio::test]
    async fn test_log_usage_uses_provider_override_config() -> Result<(), AppError> {
        let db = Arc::new(Database::memory()?);
        let app_type = "claude";

        db.set_default_cost_multiplier(app_type, "1.5").await?;
        db.set_pricing_model_source(app_type, "response").await?;
        seed_pricing(&db)?;

        let meta = ProviderMeta {
            cost_multiplier: Some("2".to_string()),
            pricing_model_source: Some("request".to_string()),
            ..ProviderMeta::default()
        };
        insert_provider(&db, "provider-1", app_type, meta)?;

        let state = build_state(db.clone());
        let usage = TokenUsage {
            input_tokens: 1_000_000,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            model: None,
            message_id: None,
        };

        log_usage_internal(
            &state,
            "req-provider-override",
            "provider-1",
            app_type,
            "resp-model",
            "req-model",
            "req-model",
            usage,
            10,
            None,
            false,
            200,
            None,
            None,
            None,
            None,
        )
        .await;

        let conn = crate::database::lock_conn!(db.conn);
        let (model, request_model, total_cost, cost_multiplier): (String, String, String, String) =
            conn.query_row(
                "SELECT model, request_model, total_cost_usd, cost_multiplier
                 FROM proxy_request_logs WHERE provider_id = ?1",
                ["provider-1"],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .map_err(|e| AppError::Database(e.to_string()))?;

        assert_eq!(model, "resp-model");
        assert_eq!(request_model, "req-model");
        assert_eq!(
            Decimal::from_str(&cost_multiplier).unwrap(),
            Decimal::from_str("2").unwrap()
        );
        assert_eq!(
            Decimal::from_str(&total_cost).unwrap(),
            Decimal::from_str("4").unwrap()
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_request_pricing_mode_anchors_to_outbound_model() -> Result<(), AppError> {
        let db = Arc::new(Database::memory()?);
        let app_type = "claude";

        db.set_pricing_model_source(app_type, "request").await?;
        seed_pricing(&db)?;
        {
            let conn = crate::database::lock_conn!(db.conn);
            conn.execute(
                "INSERT OR REPLACE INTO model_pricing (model_id, display_name, input_cost_per_million, output_cost_per_million)
                 VALUES ('outbound-model', 'Outbound Model', '4.0', '0')",
                [],
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        }

        insert_provider(&db, "provider-3", app_type, ProviderMeta::default())?;

        let state = build_state(db.clone());
        let usage = TokenUsage {
            input_tokens: 1_000_000,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            model: None,
            message_id: None,
        };

        // 路由接管场景：客户端请求 req-model（$2/M），代理实际发出 outbound-model
        // （$4/M），上游回显 resp-model。「按请求计价」必须锚定实际发出的模型。
        log_usage_internal(
            &state,
            "request-1",
            "provider-3",
            app_type,
            "resp-model",
            "req-model",
            "outbound-model",
            usage,
            10,
            None,
            false,
            200,
            None,
            None,
            None,
            None,
        )
        .await;

        let conn = crate::database::lock_conn!(db.conn);
        let (model, request_model, total_cost): (String, String, String) = conn
            .query_row(
                "SELECT model, request_model, total_cost_usd
                 FROM proxy_request_logs WHERE provider_id = ?1",
                ["provider-3"],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(|e| AppError::Database(e.to_string()))?;

        // model / request_model 列不受计价锚点影响
        assert_eq!(model, "resp-model");
        assert_eq!(request_model, "req-model");
        // 按 outbound-model（$4/M）计价，而不是 req-model（$2/M）或 resp-model（$1/M）
        assert_eq!(
            Decimal::from_str(&total_cost).unwrap(),
            Decimal::from_str("4").unwrap()
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_claude_desktop_inherits_claude_global_defaults() -> Result<(), AppError> {
        use crate::proxy::usage::logger::UsageLogger;

        let db = Arc::new(Database::memory()?);

        // 全局计费配置只有 claude/codex/gemini 三行；claude-desktop 的
        // 全局默认必须继承 claude，而不是静默落回工厂默认（1 / response）
        db.set_default_cost_multiplier("claude", "1.5").await?;
        db.set_pricing_model_source("claude", "request").await?;

        let logger = UsageLogger::new(&db);
        let (multiplier, source) = logger
            .resolve_pricing_config("nonexistent-provider", "claude-desktop")
            .await;

        assert_eq!(multiplier, Decimal::from_str("1.5").unwrap());
        assert_eq!(source, "request");
        Ok(())
    }

    #[tokio::test]
    async fn test_log_usage_falls_back_to_global_defaults() -> Result<(), AppError> {
        let db = Arc::new(Database::memory()?);
        let app_type = "claude";

        db.set_default_cost_multiplier(app_type, "1.5").await?;
        db.set_pricing_model_source(app_type, "response").await?;
        seed_pricing(&db)?;

        let meta = ProviderMeta::default();
        insert_provider(&db, "provider-2", app_type, meta)?;

        let state = build_state(db.clone());
        let usage = TokenUsage {
            input_tokens: 1_000_000,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            model: None,
            message_id: None,
        };

        log_usage_internal(
            &state,
            "req-provider-default",
            "provider-2",
            app_type,
            "resp-model",
            "req-model",
            "req-model",
            usage,
            10,
            None,
            false,
            200,
            None,
            None,
            None,
            None,
        )
        .await;

        let conn = crate::database::lock_conn!(db.conn);
        let (total_cost, cost_multiplier): (String, String) = conn
            .query_row(
                "SELECT total_cost_usd, cost_multiplier
                 FROM proxy_request_logs WHERE provider_id = ?1",
                ["provider-2"],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|e| AppError::Database(e.to_string()))?;

        assert_eq!(
            Decimal::from_str(&cost_multiplier).unwrap(),
            Decimal::from_str("1.5").unwrap()
        );
        assert_eq!(
            Decimal::from_str(&total_cost).unwrap(),
            Decimal::from_str("1.5").unwrap()
        );
        Ok(())
    }

    #[tokio::test]
    async fn logged_passthrough_ignores_trailing_h2_reset_after_data() {
        let upstream = futures::stream::iter(vec![
            Ok(Bytes::from_static(b"data: ok\n\n")),
            Err(std::io::Error::other(
                "error decoding response body: HTTP/2 stream 0 was not closed cleanly: INTERNAL_ERROR",
            )),
        ]);
        let mut stream = Box::pin(create_logged_passthrough_stream(
            upstream,
            "test",
            None,
            StreamingTimeoutConfig {
                first_byte_timeout: 0,
                idle_timeout: 0,
            },
            None,
            SseTerminalFallback::Disabled,
        ));

        let first = stream
            .next()
            .await
            .expect("first chunk")
            .expect("first chunk ok");
        assert_eq!(first, Bytes::from_static(b"data: ok\n\n"));
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn logged_passthrough_preserves_initial_h2_reset_error() {
        let upstream = futures::stream::iter(vec![Err(std::io::Error::other(
            "error decoding response body: HTTP/2 stream 0 was not closed cleanly: INTERNAL_ERROR",
        ))]);
        let mut stream = Box::pin(create_logged_passthrough_stream(
            upstream,
            "test",
            None,
            StreamingTimeoutConfig {
                first_byte_timeout: 0,
                idle_timeout: 0,
            },
            None,
            SseTerminalFallback::Disabled,
        ));

        let err = stream
            .next()
            .await
            .expect("initial error")
            .expect_err("initial h2 reset must remain an error");
        assert!(
            err.to_string().contains("error decoding response body"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn logged_passthrough_ignores_plain_decode_error_after_data() {
        let upstream = futures::stream::iter(vec![
            Ok(Bytes::from_static(b"event: response.created\n\n")),
            Err(std::io::Error::other("error decoding response body")),
        ]);
        let mut stream = Box::pin(create_logged_passthrough_stream(
            upstream,
            "test",
            None,
            StreamingTimeoutConfig {
                first_byte_timeout: 0,
                idle_timeout: 0,
            },
            None,
            SseTerminalFallback::Disabled,
        ));

        assert!(stream.next().await.expect("first chunk").is_ok());
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn logged_passthrough_synthesizes_responses_terminal_after_transport_error() {
        let upstream = futures::stream::iter(vec![
            Ok(Bytes::from_static(
                br#"event: response.created
data: {"type":"response.created","response":{"id":"resp_test","status":"in_progress"}}

"#,
            )),
            Err(std::io::Error::other(
                "error decoding response body: HTTP/2 stream 0 was not closed cleanly: INTERNAL_ERROR",
            )),
        ]);
        let mut stream = Box::pin(create_logged_passthrough_stream(
            upstream,
            "test",
            None,
            StreamingTimeoutConfig {
                first_byte_timeout: 0,
                idle_timeout: 0,
            },
            None,
            SseTerminalFallback::OpenAiResponses,
        ));

        let first = stream
            .next()
            .await
            .expect("first chunk")
            .expect("first chunk ok");
        assert!(String::from_utf8_lossy(&first).contains("response.created"));

        let fallback = stream
            .next()
            .await
            .expect("fallback chunk")
            .expect("fallback chunk ok");
        let fallback = String::from_utf8_lossy(&fallback);
        assert!(fallback.contains("event: response.failed"), "{fallback}");
        assert!(fallback.contains(r#""id":"resp_test""#), "{fallback}");
        assert!(fallback.contains("data: [DONE]"), "{fallback}");
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn logged_passthrough_synthesizes_anthropic_terminal_on_clean_early_close() {
        let upstream = futures::stream::iter(vec![Ok(Bytes::from_static(
            br#"event: message_start
data: {"type":"message_start","message":{"id":"msg_test","type":"message"}}

"#,
        ))]);
        let mut stream = Box::pin(create_logged_passthrough_stream(
            upstream,
            "test",
            None,
            StreamingTimeoutConfig {
                first_byte_timeout: 0,
                idle_timeout: 0,
            },
            None,
            SseTerminalFallback::AnthropicMessages,
        ));

        assert!(stream.next().await.expect("first chunk").is_ok());
        let fallback = stream
            .next()
            .await
            .expect("fallback chunk")
            .expect("fallback chunk ok");
        let fallback = String::from_utf8_lossy(&fallback);
        assert!(fallback.contains("event: error"), "{fallback}");
        assert!(fallback.contains("event: message_stop"), "{fallback}");
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn logged_passthrough_does_not_synthesize_after_terminal_event() {
        let upstream = futures::stream::iter(vec![Ok(Bytes::from_static(
            br#"event: response.completed
data: {"type":"response.completed","response":{"id":"resp_test","status":"completed"}}

data: [DONE]

"#,
        ))]);
        let mut stream = Box::pin(create_logged_passthrough_stream(
            upstream,
            "test",
            None,
            StreamingTimeoutConfig {
                first_byte_timeout: 0,
                idle_timeout: 0,
            },
            None,
            SseTerminalFallback::OpenAiResponses,
        ));

        let only = stream
            .next()
            .await
            .expect("terminal chunk")
            .expect("terminal chunk ok");
        let only = String::from_utf8_lossy(&only);
        assert!(only.contains("response.completed"), "{only}");
        assert!(stream.next().await.is_none());
    }
}
