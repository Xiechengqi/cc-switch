//! 请求转发器
//!
//! 负责将请求转发到上游Provider，支持故障转移

use super::hyper_client::ProxyResponse;
use super::{
    body_filter::filter_private_params_with_whitelist,
    error::*,
    failover_switch::FailoverSwitchManager,
    json_canonical::{canonicalize_value, short_value_hash},
    log_codes::fwd as log_fwd,
    provider_router::ProviderRouter,
    providers::{
        codex_chat_history::CodexChatHistoryStore, gemini_shadow::GeminiShadowStore, get_adapter,
        AuthInfo, AuthStrategy, ProviderAdapter, ProviderType,
    },
    thinking_budget_rectifier::{rectify_thinking_budget, should_rectify_thinking_budget},
    thinking_rectifier::{
        normalize_thinking_type, rectify_anthropic_request, should_rectify_thinking_signature,
    },
    types::{CopilotOptimizerConfig, OptimizerConfig, ProxyStatus, RectifierConfig},
    ProxyError,
};
use crate::commands::{
    AntigravityOAuthState, ClaudeOAuthState, CodexOAuthState, CopilotAuthState, GeminiOAuthState,
};
use crate::proxy::providers::codex_oauth_auth::CodexOAuthManager;
use crate::proxy::providers::copilot_auth::CopilotAuthManager;
use crate::{app_config::AppType, provider::Provider};
use futures::StreamExt;
use http::Extensions;
use regex::Regex;
use serde_json::Value;
use std::sync::Arc;
use std::{hash::Hasher, sync::OnceLock};
use tauri::Manager;
use tokio::sync::RwLock;
use twox_hash::XxHash64;

const GEMINI_CODE_ASSIST_BASE_URL: &str = "https://cloudcode-pa.googleapis.com";
const ANTIGRAVITY_BASE_URL: &str = "https://daily-cloudcode-pa.googleapis.com";
const GEMINI_CLI_VERSION: &str = "0.31.0";
const GEMINI_CLI_API_CLIENT_HEADER: &str = "google-genai-sdk/1.41.0 gl-node/v22.19.0";
const ANTIGRAVITY_MAX_OUTPUT_TOKENS: i64 = 16_384;

const PROXY_AUTH_PLACEHOLDER: &str = "PROXY_MANAGED";

pub struct ForwardResult {
    pub response: ProxyResponse,
    pub provider: Provider,
    pub claude_api_format: Option<String>,
    /// 活跃连接 RAII guard：随响应一起流转到 response_processor / handle_claude_transform，
    /// 最终被 move 进流式 body future（或非流式响应作用域），覆盖整个响应生命周期。
    pub(crate) connection_guard: Option<ActiveConnectionGuard>,
}

pub struct ForwardError {
    pub error: ProxyError,
    pub provider: Option<Provider>,
}

/// 活跃连接 RAII guard
///
/// 构造时把 `ProxyStatus.active_connections` +1；Drop 时在 tokio runtime 上调度
/// 一个异步任务执行 -1，从而支持把 guard move 进流式 body future（stream 自然结束
/// 时 guard 与 future 一起 drop）。
///
/// 设计动机：之前在 `forward_with_retry` 出口处同步 -1，但流式响应的 body 实际
/// 在 `create_logged_passthrough_stream` 内还会继续 yield 字节流，导致 UI 的
/// `active_connections` 计数过早归零。RAII guard 让"减量"由 Rust 类型系统驱动，
/// 不需要每条出口路径都手动调用。
pub(crate) struct ActiveConnectionGuard {
    status: Arc<RwLock<ProxyStatus>>,
}

impl ActiveConnectionGuard {
    pub(crate) async fn acquire(status: Arc<RwLock<ProxyStatus>>) -> Self {
        {
            let mut s = status.write().await;
            s.active_connections = s.active_connections.saturating_add(1);
        }
        Self { status }
    }
}

impl Drop for ActiveConnectionGuard {
    fn drop(&mut self) {
        // Drop 不能 await：把减量操作调度到 tokio runtime
        let status = self.status.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let mut s = status.write().await;
                s.active_connections = s.active_connections.saturating_sub(1);
            });
        }
        // 没有 runtime 时静默丢失计数（仅 UI 展示用，可接受最终一致性）
    }
}

pub struct RequestForwarder {
    /// 共享的 ProviderRouter（持有熔断器状态）
    router: Arc<ProviderRouter>,
    status: Arc<RwLock<ProxyStatus>>,
    current_providers: Arc<RwLock<std::collections::HashMap<String, (String, String)>>>,
    gemini_shadow: Arc<GeminiShadowStore>,
    codex_chat_history: Arc<CodexChatHistoryStore>,
    /// 故障转移切换管理器
    failover_manager: Arc<FailoverSwitchManager>,
    /// AppHandle，用于发射事件和更新托盘
    app_handle: Option<tauri::AppHandle>,
    /// 请求开始时的"当前供应商 ID"（用于判断是否需要同步 UI/托盘）
    current_provider_id_at_start: String,
    /// 代理会话 ID（用于 Gemini Native shadow replay）
    session_id: String,
    /// Session ID 是否由客户端提供；生成值不能作为上游缓存身份。
    session_client_provided: bool,
    /// 整流器配置
    rectifier_config: RectifierConfig,
    /// 优化器配置
    optimizer_config: OptimizerConfig,
    /// Copilot 优化器配置
    copilot_optimizer_config: CopilotOptimizerConfig,
    /// 非流式请求超时（秒）
    non_streaming_timeout: std::time::Duration,
    /// 流式请求响应头等待超时（秒）
    streaming_first_byte_timeout: std::time::Duration,
    /// 单个客户端请求最多尝试的 provider 数。
    ///
    /// 由 `AppProxyConfig.max_retries` (UI: "请求失败时的重试次数, 0-10") 派生：
    /// `max_attempts = max_retries + 1`，所以 max_retries=0 表示仅尝试一家、
    /// max_retries=3（默认）表示最多 4 家。loop 同时受 providers.len() 自然限制。
    ///
    /// share 请求路径下，max_attempts 永远是 1（由 HandlerContext::create_forwarder
    /// 在传入时强制），不论 max_retries 配置或 failover 开关如何。
    max_attempts: usize,
    /// Share 请求绑定的 provider id。
    ///
    /// `Some` 时本次请求由 X-Share-Token 发起，forwarder 必须：
    /// - max_attempts 强制为 1（由调用方在 new 时传入）
    /// - 不写 `current_providers`（旁路托盘"当前 provider"显示）
    /// - 不 +1 `status.failover_count`、不调 `failover_manager.try_switch`
    ///
    /// 这些不变量靠 `maybe_record_current_provider` / `maybe_handle_failover_switch`
    /// 两个 helper 集中拦截，避免散在 forwarder 三处成功路径里。
    override_provider_id: Option<String>,
}

impl RequestForwarder {
    /// 预防式 media 降级：发送前对 text-only 模型把图片块替换为标记。
    ///
    /// 受 `enabled && request_media_fallback` 管辖；其中"启发式模型名单预测"
    /// 再受 `request_media_heuristic` 单独管辖（显式声明 text-only 始终生效）。
    /// 返回被替换的图片块数量（0 = 未触发或开关关闭）。
    fn apply_media_prevention(&self, body: &mut Value, provider: &Provider) -> usize {
        if !(self.rectifier_config.enabled && self.rectifier_config.request_media_fallback) {
            return 0;
        }
        let replaced_images = super::media_sanitizer::replace_images_for_text_only_model(
            body,
            provider,
            self.rectifier_config.request_media_heuristic,
        );
        if replaced_images > 0 {
            let model = body.get("model").and_then(Value::as_str).unwrap_or("");
            log::info!(
                "[Media] Replaced {replaced_images} image block(s) with {} for text-only provider={}, model={}",
                super::media_sanitizer::UNSUPPORTED_IMAGE_MARKER,
                provider.id,
                model
            );
        }
        replaced_images
    }

    /// 反应式 media 重试判定：上游因图片输入报错后，是否应替换图片块并对同一供应商重试一次。
    ///
    /// 受 `enabled && request_media_fallback` 管辖；不涉及 `request_media_heuristic`——
    /// 这里是上游"实测"错误后的纯恢复，不是预测，故启发式开关与它无关。
    fn media_retry_should_trigger(
        &self,
        adapter_name: &str,
        already_retried: bool,
        provider_body: &Value,
        error: &ProxyError,
    ) -> bool {
        adapter_name == "Claude"
            && self.rectifier_config.enabled
            && self.rectifier_config.request_media_fallback
            && !already_retried
            && super::media_sanitizer::contains_image_blocks(provider_body)
            && super::media_sanitizer::is_unsupported_image_error(error)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        router: Arc<ProviderRouter>,
        non_streaming_timeout: u64,
        status: Arc<RwLock<ProxyStatus>>,
        current_providers: Arc<RwLock<std::collections::HashMap<String, (String, String)>>>,
        gemini_shadow: Arc<GeminiShadowStore>,
        codex_chat_history: Arc<CodexChatHistoryStore>,
        failover_manager: Arc<FailoverSwitchManager>,
        app_handle: Option<tauri::AppHandle>,
        current_provider_id_at_start: String,
        session_id: String,
        session_client_provided: bool,
        streaming_first_byte_timeout: u64,
        _streaming_idle_timeout: u64,
        rectifier_config: RectifierConfig,
        optimizer_config: OptimizerConfig,
        copilot_optimizer_config: CopilotOptimizerConfig,
        max_retries: u32,
        override_provider_id: Option<String>,
    ) -> Self {
        // max_retries 是「失败后重试次数」语义，attempt 上限 = retries + 1。
        // saturating_add 防止 u32::MAX + 1 溢出。
        //
        // share 请求路径下 max_attempts 永远是 1：share 与 provider 1:1 绑定，
        // 绑定 provider 失败时直接 502/503 给上游，不漂到其他 provider。
        let max_attempts = if override_provider_id.is_some() {
            1
        } else {
            (max_retries as usize).saturating_add(1)
        };
        Self {
            router,
            status,
            current_providers,
            gemini_shadow,
            codex_chat_history,
            failover_manager,
            app_handle,
            current_provider_id_at_start,
            session_id,
            session_client_provided,
            rectifier_config,
            optimizer_config,
            copilot_optimizer_config,
            non_streaming_timeout: std::time::Duration::from_secs(non_streaming_timeout),
            streaming_first_byte_timeout: std::time::Duration::from_secs(
                streaming_first_byte_timeout,
            ),
            max_attempts,
            override_provider_id,
        }
    }

    /// 把"当前应用类型使用的 provider"记进全局表。
    ///
    /// share 请求绝不动这个状态——它会污染托盘显示与
    /// `settings::get_current_provider`，破坏"share 与全局当前 provider 解耦"
    /// 的契约。
    async fn maybe_record_current_provider(&self, app_type_str: &str, provider: &Provider) {
        if self.override_provider_id.is_some() {
            return;
        }
        let mut current_providers = self.current_providers.write().await;
        current_providers.insert(
            app_type_str.to_string(),
            (provider.id.clone(), provider.name.clone()),
        );
    }

    /// 响应成功后判断是否需要触发"故障转移到 X provider"切换 + +1 failover_count。
    ///
    /// share 请求 max_attempts=1，理论不会进入"切换到不同 provider"分支；
    /// 兜底加一次 guard，防止后续代码改动破坏不变量。
    async fn maybe_handle_failover_switch(
        &self,
        app_type_str: &str,
        provider: &Provider,
        status: &mut ProxyStatus,
    ) {
        if self.override_provider_id.is_some() {
            return;
        }
        let should_switch = self.current_provider_id_at_start.as_str() != provider.id.as_str();
        if !should_switch {
            return;
        }
        status.failover_count += 1;
        let fm = self.failover_manager.clone();
        let ah = self.app_handle.clone();
        let pid = provider.id.clone();
        let pname = provider.name.clone();
        let at = app_type_str.to_string();
        tokio::spawn(async move {
            let _ = fm.try_switch(ah.as_ref(), &at, &pid, &pname).await;
        });
    }

    async fn record_success_result(
        &self,
        provider_id: &str,
        app_type: &str,
        used_half_open_permit: bool,
    ) {
        if used_half_open_permit {
            if let Err(e) = self
                .router
                .record_result(provider_id, app_type, true, true, None)
                .await
            {
                log::warn!(
                    "[{app_type}] 记录 Provider 成功结果失败: provider_id={provider_id}, error={e}"
                );
            }
            return;
        }

        let router = self.router.clone();
        let provider_id = provider_id.to_string();
        let app_type = app_type.to_string();
        tokio::spawn(async move {
            if let Err(e) = router
                .record_result(&provider_id, &app_type, false, true, None)
                .await
            {
                log::warn!(
                    "[{app_type}] 异步记录 Provider 成功结果失败: provider_id={provider_id}, error={e}"
                );
            }
        });
    }

    /// 整流（thinking signature 或 budget）重试失败后的统一收尾。
    ///
    /// `None` 表示已记录熔断器、累积 `last_error`/`last_provider`，
    /// 调用方应 `continue` 让下一家 provider 继续故障转移；
    /// `Some(ForwardError)` 表示是客户端错误，没有 provider 能修复，
    /// 调用方应直接 `return` 把错误返回给客户端。
    #[allow(clippy::too_many_arguments)]
    async fn handle_rectifier_retry_failure(
        &self,
        retry_err: ProxyError,
        provider: &Provider,
        app_type_str: &str,
        used_half_open_permit: bool,
        rectifier_label: &str,
        last_error: &mut Option<ProxyError>,
        last_provider: &mut Option<Provider>,
    ) -> Option<ForwardError> {
        // Provider 错误：本家上游/网络确实出问题，下一家 provider 可能可用 → 继续故障转移。
        // 客户端错误：整流后请求仍违法，下一家也修不好 → 直接返回。
        let is_provider_error = match &retry_err {
            ProxyError::Timeout(_) | ProxyError::ForwardFailed(_) => true,
            ProxyError::UpstreamError { status, .. } => *status >= 500,
            _ => false,
        };

        if is_provider_error {
            let _ = self
                .router
                .record_result(
                    &provider.id,
                    app_type_str,
                    used_half_open_permit,
                    false,
                    Some(retry_err.to_string()),
                )
                .await;
            {
                let mut status = self.status.write().await;
                status.last_error = Some(format!(
                    "Provider {} {rectifier_label}重试失败: {}",
                    provider.name, retry_err
                ));
            }
            *last_error = Some(retry_err);
            *last_provider = Some(provider.clone());
            return None;
        }

        self.router
            .release_permit_neutral(&provider.id, app_type_str, used_half_open_permit)
            .await;
        let mut status = self.status.write().await;
        status.failed_requests += 1;
        status.last_error = Some(retry_err.to_string());
        if status.total_requests > 0 {
            status.success_rate =
                (status.success_requests as f32 / status.total_requests as f32) * 100.0;
        }
        Some(ForwardError {
            error: retry_err,
            provider: Some(provider.clone()),
        })
    }

    /// 转发请求（带故障转移）
    ///
    /// 这是 thin wrapper：在客户端请求维度记一次 `total_requests` / 调整
    /// `active_connections` / 刷新 `last_request_at`，无论 inner 走哪条出口路径，
    /// 出口处都会把 `active_connections` 回收。Per-attempt 维度（成功/失败/熔断
    /// 等）仍由 inner 内自行更新 `success_requests` / `failed_requests`。
    #[allow(clippy::too_many_arguments)]
    pub async fn forward_with_retry(
        &self,
        app_type: &AppType,
        method: http::Method,
        endpoint: &str,
        body: Value,
        headers: axum::http::HeaderMap,
        extensions: Extensions,
        providers: Vec<Provider>,
    ) -> Result<ForwardResult, ForwardError> {
        let guard = ActiveConnectionGuard::acquire(self.status.clone()).await;
        {
            let mut s = self.status.write().await;
            s.total_requests = s.total_requests.saturating_add(1);
            s.last_request_at = Some(chrono::Utc::now().to_rfc3339());
        }
        let result = self
            .forward_with_retry_inner(
                app_type, method, endpoint, body, headers, extensions, providers,
            )
            .await;
        // 把 guard 注入到 Ok 结果，让它随响应一起流转到 response_processor，
        // 在流式 body 的 future 内才真正 drop。
        // Err 路径：guard 在函数 scope 内随返回值落地时自动 drop。
        result.map(|mut fr| {
            fr.connection_guard = Some(guard);
            fr
        })
    }

    /// 实际转发逻辑（不包含客户端维度的入口/出口计数）
    ///
    /// # Arguments
    /// * `app_type` - 应用类型
    /// * `method` - 客户端请求的 HTTP 方法（透传给上游，支持 GET/POST 等）
    /// * `endpoint` - API 端点
    /// * `body` - 请求体
    /// * `headers` - 请求头
    /// * `providers` - 已选择的 Provider 列表（由 RequestContext 提供，避免重复调用 select_providers）
    #[allow(clippy::too_many_arguments)]
    async fn forward_with_retry_inner(
        &self,
        app_type: &AppType,
        method: http::Method,
        endpoint: &str,
        body: Value,
        headers: axum::http::HeaderMap,
        extensions: Extensions,
        providers: Vec<Provider>,
    ) -> Result<ForwardResult, ForwardError> {
        // 获取适配器
        let adapter = get_adapter(app_type);
        let app_type_str = app_type.as_str();

        if providers.is_empty() {
            return Err(ForwardError {
                error: ProxyError::NoAvailableProvider,
                provider: None,
            });
        }

        let mut last_error = None;
        let mut last_provider = None;
        let mut attempted_providers = 0usize;

        // 单 Provider 场景下跳过熔断器检查（故障转移关闭时）
        let bypass_circuit_breaker = providers.len() == 1;

        // 依次尝试每个供应商
        for provider in providers.iter() {
            // 整流器重试标记：每个 provider 独立持有，避免标记跨 provider 短路故障转移
            // —— 首家 provider 整流后被 5xx/timeout 击落时，下家仍能用整流后的请求体走整流流程
            let mut rectifier_retried = false;
            let mut budget_rectifier_retried = false;
            let mut media_rectifier_retried = false;

            // 上限检查：尊重用户在 AppProxyConfig.max_retries 上配置的「重试次数」。
            // 放在熔断器 allow 检查之前，避免在已经超限时还占用 HalfOpen 探测名额。
            if attempted_providers >= self.max_attempts {
                log::warn!(
                    "[{app_type_str}] 已达最大尝试次数上限 ({}/{}), 停止故障转移",
                    attempted_providers,
                    self.max_attempts
                );
                break;
            }

            // 发起请求前先获取熔断器放行许可（HalfOpen 会占用探测名额）
            // 单 Provider 场景下跳过此检查，避免熔断器阻塞所有请求
            let (allowed, used_half_open_permit) = if bypass_circuit_breaker {
                (true, false)
            } else {
                let permit = self
                    .router
                    .allow_provider_request(&provider.id, app_type_str)
                    .await;
                (permit.allowed, permit.used_half_open_permit)
            };

            if !allowed {
                continue;
            }

            // PRE-SEND 优化器：每个 provider 独立决定是否优化
            // clone body 以避免 Bedrock 优化字段泄漏到非 Bedrock provider（failover 场景）
            let mut provider_body =
                if self.optimizer_config.enabled && is_bedrock_provider(provider) {
                    let mut b = body.clone();
                    if self.optimizer_config.thinking_optimizer {
                        super::thinking_optimizer::optimize(&mut b, &self.optimizer_config);
                    }
                    if self.optimizer_config.cache_injection {
                        super::cache_injector::inject(&mut b, &self.optimizer_config);
                    }
                    b
                } else {
                    body.clone()
                };

            attempted_providers += 1;

            // 更新状态中的当前 Provider 信息（per-attempt 维度的标识）
            //
            // total_requests / last_request_at / active_connections 已由
            // forward_with_retry wrapper 在客户端请求维度统一处理，这里只刷
            // 新「正在尝试哪个 provider」的展示字段。
            {
                let mut status = self.status.write().await;
                status.current_provider = Some(provider.name.clone());
                status.current_provider_id = Some(provider.id.clone());
            }

            // 转发请求（每个 Provider 只尝试一次，重试由客户端控制）
            match self
                .forward(
                    app_type,
                    &method,
                    provider,
                    endpoint,
                    &provider_body,
                    &headers,
                    &extensions,
                    adapter.as_ref(),
                )
                .await
            {
                Ok((response, claude_api_format)) => {
                    // 成功：普通闭合熔断状态异步记录，避免阻塞流式首包返回；
                    // HalfOpen 探测仍同步等待，保证 permit 与熔断状态及时释放。
                    self.record_success_result(&provider.id, app_type_str, used_half_open_permit)
                        .await;

                    // 更新当前应用类型使用的 provider（share 请求旁路）
                    self.maybe_record_current_provider(app_type_str, provider)
                        .await;

                    // 更新成功统计
                    {
                        let mut status = self.status.write().await;
                        status.success_requests += 1;
                        status.last_error = None;
                        self.maybe_handle_failover_switch(app_type_str, provider, &mut status)
                            .await;
                        // 重新计算成功率
                        if status.total_requests > 0 {
                            status.success_rate = (status.success_requests as f32
                                / status.total_requests as f32)
                                * 100.0;
                        }
                    }

                    return Ok(ForwardResult {
                        response,
                        provider: provider.clone(),
                        claude_api_format,
                        connection_guard: None,
                    });
                }
                Err(e) => {
                    // 检测是否需要触发整流器（仅 Claude/ClaudeAuth 供应商）
                    let provider_type = ProviderType::from_app_type_and_config(app_type, provider);
                    let is_anthropic_provider = matches!(
                        provider_type,
                        ProviderType::Claude | ProviderType::ClaudeAuth
                    );
                    let mut signature_rectifier_non_retryable_client_error = false;

                    if self.media_retry_should_trigger(
                        adapter.name(),
                        media_rectifier_retried,
                        &provider_body,
                        &e,
                    ) {
                        let mut media_body = provider_body.clone();
                        let replaced_images =
                            super::media_sanitizer::replace_image_blocks_with_marker(
                                &mut media_body,
                            );

                        if replaced_images > 0 {
                            let _ = std::mem::replace(&mut media_rectifier_retried, true);
                            let model = media_body
                                .get("model")
                                .and_then(Value::as_str)
                                .unwrap_or("");
                            log::info!(
                                "[{app_type_str}] [Media] Upstream rejected image input; retrying provider={} model={} with {replaced_images} image block(s) replaced by {}",
                                provider.id,
                                model,
                                super::media_sanitizer::UNSUPPORTED_IMAGE_MARKER
                            );

                            match self
                                .forward(
                                    app_type,
                                    &method,
                                    provider,
                                    endpoint,
                                    &media_body,
                                    &headers,
                                    &extensions,
                                    adapter.as_ref(),
                                )
                                .await
                            {
                                Ok((response, claude_api_format)) => {
                                    log::info!(
                                        "[{app_type_str}] [Media] Unsupported-image retry succeeded"
                                    );
                                    self.record_success_result(
                                        &provider.id,
                                        app_type_str,
                                        used_half_open_permit,
                                    )
                                    .await;

                                    {
                                        let mut current_providers =
                                            self.current_providers.write().await;
                                        current_providers.insert(
                                            app_type_str.to_string(),
                                            (provider.id.clone(), provider.name.clone()),
                                        );
                                    }

                                    {
                                        let mut status = self.status.write().await;
                                        status.success_requests += 1;
                                        status.last_error = None;
                                        let should_switch =
                                            self.current_provider_id_at_start.as_str()
                                                != provider.id.as_str();
                                        if should_switch {
                                            status.failover_count += 1;
                                            let fm = self.failover_manager.clone();
                                            let ah = self.app_handle.clone();
                                            let pid = provider.id.clone();
                                            let pname = provider.name.clone();
                                            let at = app_type_str.to_string();

                                            tokio::spawn(async move {
                                                let _ = fm
                                                    .try_switch(ah.as_ref(), &at, &pid, &pname)
                                                    .await;
                                            });
                                        }
                                        if status.total_requests > 0 {
                                            status.success_rate = (status.success_requests as f32
                                                / status.total_requests as f32)
                                                * 100.0;
                                        }
                                    }

                                    return Ok(ForwardResult {
                                        response,
                                        provider: provider.clone(),
                                        claude_api_format,
                                        connection_guard: None,
                                    });
                                }
                                Err(retry_err) => {
                                    log::warn!(
                                        "[{app_type_str}] [Media] Unsupported-image retry still failed: {retry_err}"
                                    );
                                    if let Some(err) = self
                                        .handle_rectifier_retry_failure(
                                            retry_err,
                                            provider,
                                            app_type_str,
                                            used_half_open_permit,
                                            "media 降级",
                                            &mut last_error,
                                            &mut last_provider,
                                        )
                                        .await
                                    {
                                        return Err(err);
                                    }
                                    continue;
                                }
                            }
                        }
                    }

                    if is_anthropic_provider {
                        let error_message = extract_error_message(&e);
                        if should_rectify_thinking_signature(
                            error_message.as_deref(),
                            &self.rectifier_config,
                        ) {
                            // 已经重试过：直接返回错误（不可重试客户端错误）
                            if rectifier_retried {
                                log::warn!("[{app_type_str}] [RECT-005] 整流器已触发过，不再重试");
                                // 释放 HalfOpen permit（不记录熔断器，这是客户端兼容性问题）
                                self.router
                                    .release_permit_neutral(
                                        &provider.id,
                                        app_type_str,
                                        used_half_open_permit,
                                    )
                                    .await;
                                let mut status = self.status.write().await;
                                status.failed_requests += 1;
                                status.last_error = Some(e.to_string());
                                if status.total_requests > 0 {
                                    status.success_rate = (status.success_requests as f32
                                        / status.total_requests as f32)
                                        * 100.0;
                                }
                                return Err(ForwardError {
                                    error: e,
                                    provider: Some(provider.clone()),
                                });
                            }

                            // 首次触发：整流请求体
                            let rectified = rectify_anthropic_request(&mut provider_body);

                            // 整流未生效：继续尝试 budget 整流路径，避免误判后短路
                            if !rectified.applied {
                                log::warn!(
                                    "[{app_type_str}] [RECT-006] thinking 签名整流器触发但无可整流内容，继续检查 budget；若 budget 也未命中则按客户端错误返回"
                                );
                                signature_rectifier_non_retryable_client_error = true;
                            } else {
                                log::info!(
                                    "[{}] [RECT-001] thinking 签名整流器触发, 移除 {} thinking blocks, {} redacted_thinking blocks, {} signature fields",
                                    app_type_str,
                                    rectified.removed_thinking_blocks,
                                    rectified.removed_redacted_thinking_blocks,
                                    rectified.removed_signature_fields
                                );

                                // 标记已重试（当前逻辑下重试后必定 return，保留标记以备将来扩展）
                                let _ = std::mem::replace(&mut rectifier_retried, true);

                                // 使用同一供应商重试（不计入熔断器）
                                match self
                                    .forward(
                                        app_type,
                                        &method,
                                        provider,
                                        endpoint,
                                        &provider_body,
                                        &headers,
                                        &extensions,
                                        adapter.as_ref(),
                                    )
                                    .await
                                {
                                    Ok((response, claude_api_format)) => {
                                        log::info!("[{app_type_str}] [RECT-002] 整流重试成功");
                                        self.record_success_result(
                                            &provider.id,
                                            app_type_str,
                                            used_half_open_permit,
                                        )
                                        .await;

                                        // 更新当前应用类型使用的 provider（share 请求旁路）
                                        self.maybe_record_current_provider(app_type_str, provider)
                                            .await;

                                        // 更新成功统计
                                        {
                                            let mut status = self.status.write().await;
                                            status.success_requests += 1;
                                            status.last_error = None;
                                            self.maybe_handle_failover_switch(
                                                app_type_str,
                                                provider,
                                                &mut status,
                                            )
                                            .await;
                                            if status.total_requests > 0 {
                                                status.success_rate = (status.success_requests
                                                    as f32
                                                    / status.total_requests as f32)
                                                    * 100.0;
                                            }
                                        }

                                        return Ok(ForwardResult {
                                            response,
                                            provider: provider.clone(),
                                            claude_api_format,
                                            connection_guard: None,
                                        });
                                    }
                                    Err(retry_err) => {
                                        log::warn!(
                                            "[{app_type_str}] [RECT-003] 整流重试仍失败: {retry_err}"
                                        );
                                        if let Some(err) = self
                                            .handle_rectifier_retry_failure(
                                                retry_err,
                                                provider,
                                                app_type_str,
                                                used_half_open_permit,
                                                "整流",
                                                &mut last_error,
                                                &mut last_provider,
                                            )
                                            .await
                                        {
                                            return Err(err);
                                        }
                                        continue;
                                    }
                                }
                            }
                        }
                    }

                    // 检测是否需要触发 budget 整流器（仅 Claude/ClaudeAuth 供应商）
                    if is_anthropic_provider {
                        let error_message = extract_error_message(&e);
                        if should_rectify_thinking_budget(
                            error_message.as_deref(),
                            &self.rectifier_config,
                        ) {
                            // 已经重试过：直接返回错误（不可重试客户端错误）
                            if budget_rectifier_retried {
                                log::warn!(
                                    "[{app_type_str}] [RECT-013] budget 整流器已触发过，不再重试"
                                );
                                self.router
                                    .release_permit_neutral(
                                        &provider.id,
                                        app_type_str,
                                        used_half_open_permit,
                                    )
                                    .await;
                                let mut status = self.status.write().await;
                                status.failed_requests += 1;
                                status.last_error = Some(e.to_string());
                                if status.total_requests > 0 {
                                    status.success_rate = (status.success_requests as f32
                                        / status.total_requests as f32)
                                        * 100.0;
                                }
                                return Err(ForwardError {
                                    error: e,
                                    provider: Some(provider.clone()),
                                });
                            }

                            let budget_rectified = rectify_thinking_budget(&mut provider_body);
                            if !budget_rectified.applied {
                                log::warn!(
                                    "[{app_type_str}] [RECT-014] budget 整流器触发但无可整流内容，不做无意义重试"
                                );
                                self.router
                                    .release_permit_neutral(
                                        &provider.id,
                                        app_type_str,
                                        used_half_open_permit,
                                    )
                                    .await;
                                let mut status = self.status.write().await;
                                status.failed_requests += 1;
                                status.last_error = Some(e.to_string());
                                if status.total_requests > 0 {
                                    status.success_rate = (status.success_requests as f32
                                        / status.total_requests as f32)
                                        * 100.0;
                                }
                                return Err(ForwardError {
                                    error: e,
                                    provider: Some(provider.clone()),
                                });
                            }

                            log::info!(
                                "[{}] [RECT-010] thinking budget 整流器触发, before={:?}, after={:?}",
                                app_type_str,
                                budget_rectified.before,
                                budget_rectified.after
                            );

                            let _ = std::mem::replace(&mut budget_rectifier_retried, true);

                            // 使用同一供应商重试（不计入熔断器）
                            match self
                                .forward(
                                    app_type,
                                    &method,
                                    provider,
                                    endpoint,
                                    &provider_body,
                                    &headers,
                                    &extensions,
                                    adapter.as_ref(),
                                )
                                .await
                            {
                                Ok((response, claude_api_format)) => {
                                    log::info!("[{app_type_str}] [RECT-011] budget 整流重试成功");
                                    self.record_success_result(
                                        &provider.id,
                                        app_type_str,
                                        used_half_open_permit,
                                    )
                                    .await;

                                    self.maybe_record_current_provider(app_type_str, provider)
                                        .await;

                                    {
                                        let mut status = self.status.write().await;
                                        status.success_requests += 1;
                                        status.last_error = None;
                                        self.maybe_handle_failover_switch(
                                            app_type_str,
                                            provider,
                                            &mut status,
                                        )
                                        .await;
                                        if status.total_requests > 0 {
                                            status.success_rate = (status.success_requests as f32
                                                / status.total_requests as f32)
                                                * 100.0;
                                        }
                                    }

                                    return Ok(ForwardResult {
                                        response,
                                        provider: provider.clone(),
                                        claude_api_format,
                                        connection_guard: None,
                                    });
                                }
                                Err(retry_err) => {
                                    log::warn!(
                                        "[{app_type_str}] [RECT-012] budget 整流重试仍失败: {retry_err}"
                                    );
                                    if let Some(err) = self
                                        .handle_rectifier_retry_failure(
                                            retry_err,
                                            provider,
                                            app_type_str,
                                            used_half_open_permit,
                                            "budget 整流",
                                            &mut last_error,
                                            &mut last_provider,
                                        )
                                        .await
                                    {
                                        return Err(err);
                                    }
                                    continue;
                                }
                            }
                        }
                    }

                    if signature_rectifier_non_retryable_client_error {
                        self.router
                            .release_permit_neutral(
                                &provider.id,
                                app_type_str,
                                used_half_open_permit,
                            )
                            .await;
                        let mut status = self.status.write().await;
                        status.failed_requests += 1;
                        status.last_error = Some(e.to_string());
                        if status.total_requests > 0 {
                            status.success_rate = (status.success_requests as f32
                                / status.total_requests as f32)
                                * 100.0;
                        }
                        return Err(ForwardError {
                            error: e,
                            provider: Some(provider.clone()),
                        });
                    }

                    // 先分类错误，决定是否计入 provider 健康度
                    // —— NonRetryable / ClientAbort 是客户端层错误，无论换哪家 provider 都会被拒绝，
                    //    不应污染熔断器和数据库健康度（与 release_permit_neutral 同语义）。
                    let category = self.categorize_proxy_error(&e);

                    match category {
                        ErrorCategory::Retryable => {
                            // 可重试：真正的 provider 故障 → 记录失败并更新熔断器/DB 健康度
                            let _ = self
                                .router
                                .record_result(
                                    &provider.id,
                                    app_type_str,
                                    used_half_open_permit,
                                    false,
                                    Some(e.to_string()),
                                )
                                .await;

                            {
                                let mut status = self.status.write().await;
                                status.last_error =
                                    Some(format!("Provider {} 失败: {}", provider.name, e));
                            }

                            let (log_code, log_message) = build_retryable_failure_log(
                                &provider.name,
                                attempted_providers,
                                providers.len(),
                                &e,
                            );
                            log::warn!("[{app_type_str}] [{log_code}] {log_message}");

                            last_error = Some(e);
                            last_provider = Some(provider.clone());
                            // 继续尝试下一个供应商
                            continue;
                        }
                        ErrorCategory::NonRetryable | ErrorCategory::ClientAbort => {
                            // 不可重试：客户端层错误或客户端断连 → 不污染健康度，仅释放 HalfOpen permit
                            self.router
                                .release_permit_neutral(
                                    &provider.id,
                                    app_type_str,
                                    used_half_open_permit,
                                )
                                .await;
                            {
                                let mut status = self.status.write().await;
                                status.failed_requests += 1;
                                status.last_error = Some(e.to_string());
                                if status.total_requests > 0 {
                                    status.success_rate = (status.success_requests as f32
                                        / status.total_requests as f32)
                                        * 100.0;
                                }
                            }
                            return Err(ForwardError {
                                error: e,
                                provider: Some(provider.clone()),
                            });
                        }
                    }
                }
            }
        }

        if attempted_providers == 0 {
            // providers 列表非空，但全部被熔断器拒绝（典型：HalfOpen 探测名额被占用）
            {
                let mut status = self.status.write().await;
                status.failed_requests += 1;
                status.last_error = Some("所有供应商暂时不可用（熔断器限制）".to_string());
                if status.total_requests > 0 {
                    status.success_rate =
                        (status.success_requests as f32 / status.total_requests as f32) * 100.0;
                }
            }
            return Err(ForwardError {
                error: ProxyError::NoAvailableProvider,
                provider: None,
            });
        }

        // 所有供应商都失败了
        {
            let mut status = self.status.write().await;
            status.failed_requests += 1;
            status.last_error = Some("所有供应商都失败".to_string());
            if status.total_requests > 0 {
                status.success_rate =
                    (status.success_requests as f32 / status.total_requests as f32) * 100.0;
            }
        }

        if let Some((log_code, log_message)) =
            build_terminal_failure_log(attempted_providers, providers.len(), last_error.as_ref())
        {
            log::warn!("[{app_type_str}] [{log_code}] {log_message}");
        }

        Err(ForwardError {
            error: last_error.unwrap_or(ProxyError::MaxRetriesExceeded),
            provider: last_provider,
        })
    }

    /// 转发单个请求（使用适配器）
    #[allow(clippy::too_many_arguments)]
    async fn forward(
        &self,
        app_type: &AppType,
        _method: &http::Method,
        provider: &Provider,
        endpoint: &str,
        body: &Value,
        headers: &axum::http::HeaderMap,
        extensions: &Extensions,
        adapter: &dyn ProviderAdapter,
    ) -> Result<(ProxyResponse, Option<String>), ProxyError> {
        if matches!(app_type, AppType::Claude) && provider.is_deepseek_account_provider() {
            let response = super::providers::deepseek_claude::forward_deepseek_claude(
                self.app_handle.as_ref(),
                provider,
                body,
            )
            .await?;
            return Ok((response, Some("anthropic".to_string())));
        }

        if matches!(app_type, AppType::Claude) && provider.is_kiro_oauth_provider() {
            let response = super::providers::kiro_claude::forward_kiro_claude(
                self.app_handle.as_ref(),
                provider,
                body,
            )
            .await?;
            return Ok((response, Some("anthropic".to_string())));
        }

        if matches!(app_type, AppType::Claude) && provider.is_cursor_oauth_provider() {
            let response = super::providers::cursor_claude::forward_cursor_claude(
                self.app_handle.as_ref(),
                provider,
                body,
            )
            .await?;
            return Ok((response, Some("anthropic".to_string())));
        }

        if matches!(app_type, AppType::Codex) && provider.is_cursor_oauth_provider() {
            let response = super::providers::cursor_codex::forward_cursor_codex(
                self.app_handle.as_ref(),
                provider,
                endpoint,
                body,
            )
            .await?;
            return Ok((response, Some("openai".to_string())));
        }

        // Gemini Official/OAuth 对齐 Claude/Codex official：本地代理不要求用户配置
        // base_url，后续会直接改写到 Code Assist 内部接口。
        let mut base_url = extract_forward_base_url(app_type, provider, adapter)?;

        let is_full_url = provider
            .meta
            .as_ref()
            .and_then(|meta| meta.is_full_url)
            .unwrap_or(false);

        // GitHub Copilot API 使用 /chat/completions（无 /v1 前缀）
        let is_copilot = provider
            .meta
            .as_ref()
            .and_then(|m| m.provider_type.as_deref())
            == Some("github_copilot")
            || base_url.contains("githubcopilot.com");

        // 应用模型映射（独立于格式转换）
        // Claude Desktop proxy 模式必须先把 Desktop 可见的 claude-* route
        // 映射成真实上游模型名，并且未知 route 要直接报错，不能使用默认模型兜底。
        let mapped_body = if matches!(app_type, AppType::ClaudeDesktop) {
            crate::claude_desktop_config::map_proxy_request_model(body.clone(), provider)
                .map_err(|e| ProxyError::InvalidRequest(e.to_string()))?
        } else {
            let (mapped_body, _original_model, _mapped_model) =
                super::model_mapper::apply_model_mapping(body.clone(), provider);
            mapped_body
        };

        // 与 CCH 对齐：请求前不做 thinking 主动改写（仅保留兼容入口）
        let mut mapped_body = normalize_thinking_type(mapped_body);

        if is_copilot {
            mapped_body =
                super::providers::copilot_model_map::apply_copilot_model_normalization(mapped_body);
            self.apply_copilot_live_model_resolution(provider, &mut mapped_body)
                .await;
        } else {
            mapped_body =
                super::model_mapper::strip_one_m_suffix_for_upstream_from_body(mapped_body);
        }

        // --- Copilot 优化器：分类 + 请求体优化（在格式转换之前执行） ---
        // 注意：确定性 ID 也在此处计算，因为 mapped_body 在格式转换时会被 move
        //
        // 执行顺序（与 copilot-api 对齐）：
        //   1. 先在原始 body 上分类（保留 tool_result 语义，避免误判为 user）
        //   2. 再清洗孤立 tool_result（防止上游 API 报错）
        //   3. 再合并 tool_result + text（减少 premium 计费）
        let copilot_optimization = if is_copilot && self.copilot_optimizer_config.enabled {
            // 1. 在原始 body 上分类 — 必须在清洗/合并之前执行
            //    孤立 tool_result 仍保持 tool_result 类型，分类能正确识别为 agent
            let has_anthropic_beta = headers.contains_key("anthropic-beta");
            let classification = super::copilot_optimizer::classify_request(
                &mapped_body,
                has_anthropic_beta,
                self.copilot_optimizer_config.compact_detection,
                self.copilot_optimizer_config.subagent_detection,
            );

            log::debug!(
                "[Copilot] 优化器分类: initiator={}, is_warmup={}, is_compact={}, is_subagent={}",
                classification.initiator,
                classification.is_warmup,
                classification.is_compact,
                classification.is_subagent
            );

            // 2. 孤立 tool_result 清理 — 分类完成后再清洗
            //    防止上游 API 因不匹配的 tool_result 报错导致重试/重复计费
            mapped_body = super::copilot_optimizer::sanitize_orphan_tool_results(mapped_body);

            // 3. Tool result 合并 — 将 [tool_result, text] 变为 [tool_result(含text)]
            if self.copilot_optimizer_config.tool_result_merging {
                mapped_body = super::copilot_optimizer::merge_tool_results(mapped_body);
            }

            // 3.5. 主动剥离 thinking block — Copilot 走 OpenAI 兼容端点不识别该块
            //      避免上游拒绝后由 rectifier 反应式重试（首次请求已消耗 quota）
            if self.copilot_optimizer_config.strip_thinking {
                mapped_body = super::copilot_optimizer::strip_thinking_blocks(mapped_body);
            }

            // 4. Warmup 小模型降级
            if self.copilot_optimizer_config.warmup_downgrade && classification.is_warmup {
                log::info!(
                    "[Copilot] Warmup 请求降级到模型: {}",
                    self.copilot_optimizer_config.warmup_model
                );
                mapped_body["model"] =
                    serde_json::json!(&self.copilot_optimizer_config.warmup_model);
            }

            // 预计算确定性 Request ID（在 body 被 move 之前）
            // Session 提取优先级（与 session.rs extract_from_metadata 对齐）：
            //   1. metadata.user_id 中的 _session_ 后缀
            //   2. metadata.session_id（直接字段）
            //   3. raw metadata.user_id（整串 fallback）
            //   4. x-session-id header
            let metadata = body.get("metadata");
            let session_id = metadata
                .and_then(|m| m.get("user_id"))
                .and_then(|v| v.as_str())
                .and_then(super::session::parse_session_from_user_id)
                .or_else(|| {
                    metadata
                        .and_then(|m| m.get("session_id"))
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string())
                })
                .or_else(|| {
                    metadata
                        .and_then(|m| m.get("user_id"))
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string())
                })
                .or_else(|| {
                    headers
                        .get("x-session-id")
                        .and_then(|v| v.to_str().ok())
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string())
                })
                .unwrap_or_default();
            let det_request_id = if self.copilot_optimizer_config.deterministic_request_id {
                Some(super::copilot_optimizer::deterministic_request_id(
                    &mapped_body,
                    &session_id,
                ))
            } else {
                None
            };

            // 从 session ID 派生稳定的 interaction ID（同一主对话共享）
            let interaction_id =
                super::copilot_optimizer::deterministic_interaction_id(&session_id);

            Some((classification, det_request_id, interaction_id))
        } else {
            None
        };

        // GitHub Copilot 动态 endpoint 路由
        // 从 CopilotAuthManager 获取缓存的 API endpoint（支持企业版等非默认 endpoint）
        if is_copilot && !is_full_url {
            if let Some(app_handle) = &self.app_handle {
                let copilot_state = app_handle.state::<CopilotAuthState>();
                let copilot_auth = copilot_state.0.read().await;

                // 从 provider.meta 获取关联的 GitHub 账号 ID
                let account_id = provider
                    .meta
                    .as_ref()
                    .and_then(|m| m.managed_account_id_for("github_copilot"));

                let dynamic_endpoint = match &account_id {
                    Some(id) => copilot_auth.get_api_endpoint(id).await,
                    None => copilot_auth.get_default_api_endpoint().await,
                };

                // 只在动态 endpoint 与当前 base_url 不同时替换
                if dynamic_endpoint != base_url {
                    log::debug!(
                        "[Copilot] 使用动态 API endpoint: {} (原: {})",
                        dynamic_endpoint,
                        base_url
                    );
                    base_url = dynamic_endpoint;
                }
            }
        }
        let resolved_claude_api_format = if adapter.name() == "Claude" {
            Some(
                self.resolve_claude_api_format(provider, &mapped_body, is_copilot)
                    .await,
            )
        } else {
            None
        };
        if adapter.name() == "Claude" {
            if let Some(api_format) = resolved_claude_api_format.as_deref() {
                super::providers::normalize_anthropic_tool_thinking_history_for_provider(
                    &mut mapped_body,
                    provider,
                    api_format,
                );
                self.apply_media_prevention(&mut mapped_body, provider);
            }
        }
        let needs_transform = match resolved_claude_api_format.as_deref() {
            Some(api_format) => super::providers::claude_api_format_needs_transform(api_format),
            None => adapter.needs_transform(provider),
        };
        let codex_responses_to_chat = matches!(app_type, AppType::Codex)
            && super::providers::should_convert_codex_responses_to_chat(provider, endpoint);
        let (effective_endpoint, passthrough_query) = if codex_responses_to_chat {
            rewrite_codex_responses_endpoint_to_chat(endpoint)
        } else if needs_transform && adapter.name() == "Claude" {
            let api_format = resolved_claude_api_format
                .as_deref()
                .unwrap_or_else(|| super::providers::get_claude_api_format(provider));
            rewrite_claude_transform_endpoint(endpoint, api_format, is_copilot, &mapped_body)
        } else {
            (
                endpoint.to_string(),
                split_endpoint_and_query(endpoint)
                    .1
                    .map(ToString::to_string),
            )
        };
        let is_claude_oauth_provider = adapter.name() == "Claude"
            && provider
                .meta
                .as_ref()
                .and_then(|m| m.provider_type.as_deref())
                == Some("claude_oauth");

        let codex_chat_base_is_full_endpoint = codex_responses_to_chat
            && base_url
                .trim_end_matches('/')
                .to_ascii_lowercase()
                .ends_with("/chat/completions");

        let mut url = if matches!(resolved_claude_api_format.as_deref(), Some("gemini_native")) {
            super::gemini_url::resolve_gemini_native_url(
                &base_url,
                &effective_endpoint,
                is_full_url,
            )
        } else if is_full_url || codex_chat_base_is_full_endpoint {
            append_query_to_full_url(&base_url, passthrough_query.as_deref())
        } else {
            adapter.build_url(&base_url, &effective_endpoint)
        };
        if is_claude_oauth_provider {
            url = ensure_claude_oauth_beta_query(&url);
        }

        // 转换请求体（如果需要）
        let request_body = if codex_responses_to_chat {
            let mut mapped_body = mapped_body;
            let restored = self
                .codex_chat_history
                .enrich_request(&mut mapped_body)
                .await;
            if restored > 0 {
                log::debug!(
                    "[Codex] Restored {restored} cached function call(s) for Chat upstream"
                );
            }
            super::providers::apply_codex_chat_upstream_model(provider, &mut mapped_body);
            let reasoning_config =
                super::providers::resolve_codex_chat_reasoning_config(provider, &mapped_body);
            super::providers::transform_codex_chat::responses_to_chat_completions_with_reasoning(
                mapped_body,
                reasoning_config.as_ref(),
            )?
        } else if needs_transform {
            if adapter.name() == "Claude" {
                let api_format = resolved_claude_api_format
                    .as_deref()
                    .unwrap_or_else(|| super::providers::get_claude_api_format(provider));
                super::providers::transform_claude_request_for_api_format(
                    mapped_body,
                    provider,
                    api_format,
                    self.session_client_provided
                        .then_some(self.session_id.as_str()),
                    Some(self.gemini_shadow.as_ref()),
                )?
            } else {
                adapter.transform_request(mapped_body, provider)?
            }
        } else {
            mapped_body
        };

        // 过滤私有参数（以 `_` 开头的字段），防止内部信息泄露到上游
        // 默认使用空白名单，过滤所有 _ 前缀字段
        let mut filtered_body = prepare_upstream_request_body(request_body);
        let codex_oauth_upstream_session_id = self
            .session_client_provided
            .then(|| codex_oauth_upstream_session_id(&self.session_id))
            .flatten();
        if adapter.name() == "Codex" && provider.is_codex_official_with_managed_auth() {
            filtered_body = normalize_codex_oauth_responses_body(
                filtered_body,
                codex_oauth_upstream_session_id.as_deref(),
            );
        }
        if is_claude_oauth_provider {
            filtered_body = ensure_claude_oauth_billing_header_system(filtered_body);
            filtered_body = sign_claude_oauth_messages_body(filtered_body);
        }
        log_prompt_cache_trace(
            app_type,
            provider,
            &effective_endpoint,
            resolved_claude_api_format.as_deref(),
            &filtered_body,
            self.session_client_provided,
        );
        let request_is_streaming =
            is_streaming_request(&effective_endpoint, &filtered_body, headers);
        let force_identity_encoding =
            needs_transform || codex_responses_to_chat || request_is_streaming;

        let gemini_code_assist_model = if is_gemini_code_assist_provider(app_type, provider) {
            let (code_assist_url, code_assist_body, model) =
                build_gemini_code_assist_forward_request(&effective_endpoint, &filtered_body)?;
            url = code_assist_url;
            filtered_body = code_assist_body;
            Some(model)
        } else {
            None
        };
        let mut antigravity_project_id: Option<String>;
        if is_antigravity_oauth_provider(app_type, provider) {
            let (antigravity_url, antigravity_body) = build_antigravity_forward_request(
                &effective_endpoint,
                &filtered_body,
                self.session_id.as_str(),
            )?;
            url = antigravity_url;
            filtered_body = antigravity_body;
        }

        // OAuth 401 重试：如果上游返回 401 且本次请求使用了 OAuth 账号注入，
        // 作废该账号的缓存 access_token 后重试一次（仅一次，避免雪崩）。
        #[derive(Debug, Clone, Copy)]
        enum OAuthKind {
            Claude,
            Codex,
            Copilot,
            Gemini,
            Antigravity,
        }
        let mut oauth_retried = false;

        let response: ProxyResponse = loop {
            // Codex OAuth 需要注入的 ChatGPT-Account-Id（在动态 token 获取期间填充）
            let mut codex_oauth_account_id: Option<String> = None;
            let mut should_send_codex_oauth_session_headers = false;
            let mut gemini_oauth_access_token: Option<String> = None;
            let mut antigravity_oauth_access_token: Option<String> = None;
            antigravity_project_id = None;
            // 本次 attempt 实际使用的 OAuth 账号（用于 401 重试时精准作废缓存）
            let mut oauth_kind_used: Option<(OAuthKind, String)> = None;

            // 获取认证头（提前准备，用于内联替换）
            let extracted_auth = adapter.extract_auth(provider).or_else(|| {
                (is_gemini_code_assist_provider(app_type, provider)
                    || is_antigravity_oauth_provider(app_type, provider))
                .then(|| AuthInfo::with_access_token(String::new(), String::new()))
            });
            let mut auth_headers = if let Some(mut auth) = extracted_auth {
                // GitHub Copilot 特殊处理：从 CopilotAuthManager 获取真实 token
                if auth.strategy == AuthStrategy::GitHubCopilot {
                    if let Some(app_handle) = &self.app_handle {
                        let copilot_state = app_handle.state::<CopilotAuthState>();
                        let copilot_auth: tokio::sync::RwLockReadGuard<'_, CopilotAuthManager> =
                            copilot_state.0.read().await;

                        // 从 provider.meta 获取关联的 GitHub 账号 ID（多账号支持）
                        let account_id = provider
                            .meta
                            .as_ref()
                            .and_then(|m| m.managed_account_id_for("github_copilot"));

                        // 根据账号 ID 获取对应 token（向后兼容：无账号 ID 时使用第一个账号）
                        let token_result = match &account_id {
                            Some(id) => {
                                log::debug!("[Copilot] 使用指定账号 {id} 获取 token");
                                copilot_auth.get_valid_token_for_account(id).await
                            }
                            None => {
                                log::debug!("[Copilot] 使用默认账号获取 token");
                                copilot_auth.get_valid_token().await
                            }
                        };

                        match token_result {
                            Ok(token) => {
                                auth = AuthInfo::new(token, AuthStrategy::GitHubCopilot);
                                let resolved = match account_id.clone() {
                                    Some(id) => Some(id),
                                    None => copilot_auth.default_account_id().await,
                                };
                                if let Some(id) = resolved {
                                    oauth_kind_used = Some((OAuthKind::Copilot, id));
                                }
                                log::debug!(
                                    "[Copilot] 成功获取 Copilot token (account={})",
                                    account_id.as_deref().unwrap_or("default")
                                );
                            }
                            Err(e) => {
                                log::error!(
                                    "[Copilot] 获取 Copilot token 失败 (account={}): {e}",
                                    account_id.as_deref().unwrap_or("default")
                                );
                                return Err(ProxyError::AuthError(format!(
                                    "GitHub Copilot 认证失败: {e}"
                                )));
                            }
                        }
                    } else {
                        log::error!("[Copilot] AppHandle 不可用");
                        return Err(ProxyError::AuthError(
                            "GitHub Copilot 认证不可用（无 AppHandle）".to_string(),
                        ));
                    }
                }

                // Codex OAuth 特殊处理：从 CodexOAuthManager 获取真实 access_token
                if auth.strategy == AuthStrategy::CodexOAuth {
                    if let Some(app_handle) = &self.app_handle {
                        let codex_state = app_handle.state::<CodexOAuthState>();
                        let codex_auth: tokio::sync::RwLockReadGuard<'_, CodexOAuthManager> =
                            codex_state.0.read().await;

                        // 从 provider.meta 获取关联的 ChatGPT 账号 ID
                        let account_id = provider
                            .meta
                            .as_ref()
                            .and_then(|m| m.managed_account_id_for("codex_oauth"));

                        let token_result = match &account_id {
                            Some(id) => {
                                log::debug!("[CodexOAuth] 使用指定账号 {id} 获取 token");
                                codex_auth.get_valid_token_for_account(id).await
                            }
                            None => {
                                log::debug!("[CodexOAuth] 使用默认账号获取 token");
                                codex_auth.get_valid_token().await
                            }
                        };

                        match token_result {
                            Ok(token) => {
                                auth = AuthInfo::new(token, AuthStrategy::CodexOAuth);
                                should_send_codex_oauth_session_headers = true;
                                // 解析使用的 account_id（用于注入 ChatGPT-Account-Id header）
                                codex_oauth_account_id = match account_id {
                                    Some(id) => Some(id),
                                    None => codex_auth.default_account_id().await,
                                };
                                if let Some(ref id) = codex_oauth_account_id {
                                    oauth_kind_used = Some((OAuthKind::Codex, id.clone()));
                                }
                                log::debug!(
                                    "[CodexOAuth] 成功获取 access_token (account={})",
                                    codex_oauth_account_id.as_deref().unwrap_or("default")
                                );
                            }
                            Err(e) => {
                                log::error!("[CodexOAuth] 获取 access_token 失败: {e}");
                                return Err(ProxyError::AuthError(format!(
                                    "Codex OAuth 认证失败: {e}"
                                )));
                            }
                        }
                    } else {
                        log::error!("[CodexOAuth] AppHandle 不可用");
                        return Err(ProxyError::AuthError(
                            "Codex OAuth 认证不可用（无 AppHandle）".to_string(),
                        ));
                    }
                }

                // Claude OAuth: 从 ClaudeOAuthManager 获取真实 access_token
                if auth.strategy == AuthStrategy::ClaudeOAuth {
                    if let Some(app_handle) = &self.app_handle {
                        let claude_state = app_handle.state::<ClaudeOAuthState>();
                        let claude_auth = claude_state.0.read().await;

                        let account_id = provider
                            .meta
                            .as_ref()
                            .and_then(|m| m.managed_account_id_for("claude_oauth"));

                        match if let Some(ref acc_id) = account_id {
                            claude_auth.get_valid_token_for_account(acc_id).await
                        } else {
                            claude_auth.get_valid_token().await
                        } {
                            Ok(token) => {
                                auth.api_key = token.clone();
                                auth.access_token = Some(token);
                                let resolved = match account_id.clone() {
                                    Some(id) => Some(id),
                                    None => claude_auth.default_account_id().await,
                                };
                                if let Some(id) = resolved {
                                    oauth_kind_used = Some((OAuthKind::Claude, id));
                                }
                                log::debug!(
                                    "[ClaudeOAuth] 成功获取 access_token (account={})",
                                    account_id.as_deref().unwrap_or("default")
                                );
                            }
                            Err(e) => {
                                log::error!("[ClaudeOAuth] 获取 access_token 失败: {e}");
                                return Err(ProxyError::AuthError(format!(
                                    "Claude OAuth 认证失败: {e}"
                                )));
                            }
                        }
                    } else {
                        log::error!("[ClaudeOAuth] AppHandle 不可用");
                        return Err(ProxyError::AuthError(
                            "Claude OAuth 认证不可用（无 AppHandle）".to_string(),
                        ));
                    }
                }

                // Antigravity OAuth: 从 AntigravityOAuthManager 获取 access_token 和 project_id。
                if auth.strategy == AuthStrategy::GoogleOAuth
                    && is_antigravity_oauth_provider(app_type, provider)
                {
                    if let Some(app_handle) = &self.app_handle {
                        let antigravity_state = app_handle.state::<AntigravityOAuthState>();
                        let antigravity_auth = antigravity_state.0.read().await;

                        let account_id = provider
                            .meta
                            .as_ref()
                            .and_then(|m| m.managed_account_id_for("antigravity_oauth"));

                        let resolved_account_id = match account_id {
                            Some(id) => id,
                            None => {
                                antigravity_auth.default_account_id().await.ok_or_else(|| {
                                    ProxyError::AuthError(
                                        "Antigravity OAuth 认证失败: 未找到可用账号".to_string(),
                                    )
                                })?
                            }
                        };

                        let token = antigravity_auth
                            .get_valid_token_for_account(&resolved_account_id)
                            .await
                            .map_err(|e| {
                                ProxyError::AuthError(format!("Antigravity OAuth 认证失败: {e}"))
                            })?;
                        let project_id = antigravity_auth
                            .project_id_for_account(&resolved_account_id)
                            .await
                            .map_err(|e| {
                                ProxyError::AuthError(format!(
                                    "Antigravity OAuth project 读取失败: {e}"
                                ))
                            })?;

                        auth.api_key = token.clone();
                        auth.access_token = Some(token.clone());
                        antigravity_oauth_access_token = Some(token);
                        antigravity_project_id = Some(project_id);
                        oauth_kind_used =
                            Some((OAuthKind::Antigravity, resolved_account_id.clone()));
                    } else {
                        log::error!("[AntigravityOAuth] AppHandle 不可用");
                        return Err(ProxyError::AuthError(
                            "Antigravity OAuth 认证不可用（无 AppHandle）".to_string(),
                        ));
                    }
                }

                // Google Gemini OAuth: 从 GeminiOAuthManager 获取真实 access_token。
                // 本地代理端口转发到 Google Official 时不能沿用客户端传入的
                // x-goog-api-key，需要在这里按当前绑定账号动态注入 OAuth token。
                if auth.strategy == AuthStrategy::GoogleOAuth
                    && is_gemini_code_assist_provider(app_type, provider)
                {
                    if let Some(app_handle) = &self.app_handle {
                        let gemini_state = app_handle.state::<GeminiOAuthState>();
                        let gemini_auth = gemini_state.0.read().await;

                        let account_id = provider
                            .meta
                            .as_ref()
                            .and_then(|m| m.managed_account_id_for("google_gemini_oauth"));

                        let (token, resolved_account_id) = match account_id {
                            Some(id) => {
                                log::debug!("[GeminiOAuth] 使用指定账号 {id} 获取 token");
                                (
                                    gemini_auth.get_valid_token_for_account(&id).await.map_err(
                                        |e| {
                                            ProxyError::AuthError(format!(
                                                "Google Gemini OAuth 认证失败: {e}"
                                            ))
                                        },
                                    )?,
                                    Some(id),
                                )
                            }
                            None => {
                                let Some(id) = gemini_auth.default_account_id().await else {
                                    return Err(ProxyError::AuthError(
                                        "Google Gemini OAuth 认证失败: 未找到可用账号".to_string(),
                                    ));
                                };
                                log::debug!("[GeminiOAuth] 使用默认账号 {id} 获取 token");
                                (
                                    gemini_auth.get_valid_token_for_account(&id).await.map_err(
                                        |e| {
                                            ProxyError::AuthError(format!(
                                                "Google Gemini OAuth 认证失败: {e}"
                                            ))
                                        },
                                    )?,
                                    Some(id),
                                )
                            }
                        };

                        auth.api_key = token.clone();
                        auth.access_token = Some(token.clone());
                        gemini_oauth_access_token = Some(token);
                        if let Some(id) = resolved_account_id {
                            oauth_kind_used = Some((OAuthKind::Gemini, id));
                        }
                    } else {
                        log::error!("[GeminiOAuth] AppHandle 不可用");
                        return Err(ProxyError::AuthError(
                            "Google Gemini OAuth 认证不可用（无 AppHandle）".to_string(),
                        ));
                    }
                }

                adapter.get_auth_headers(&auth)?
            } else {
                Vec::new()
            };

            maybe_add_share_auth_header(&mut auth_headers, &base_url);
            if let Some(model) = gemini_code_assist_model.as_deref() {
                upsert_header(
                    &mut auth_headers,
                    http::HeaderName::from_static("user-agent"),
                    http::HeaderValue::from_str(&gemini_cli_user_agent(model)).map_err(|e| {
                        ProxyError::Internal(format!("Invalid Gemini CLI User-Agent: {e}"))
                    })?,
                );
                upsert_header(
                    &mut auth_headers,
                    http::HeaderName::from_static("x-goog-api-client"),
                    http::HeaderValue::from_static(GEMINI_CLI_API_CLIENT_HEADER),
                );
            }

            if is_antigravity_oauth_provider(app_type, provider) {
                upsert_header(
                    &mut auth_headers,
                    http::HeaderName::from_static("user-agent"),
                    http::HeaderValue::from_str(&antigravity_user_agent()).map_err(|e| {
                        ProxyError::Internal(format!("Invalid Antigravity User-Agent: {e}"))
                    })?,
                );
                upsert_header(
                    &mut auth_headers,
                    http::HeaderName::from_static("x-request-source"),
                    http::HeaderValue::from_static("local"),
                );
                if self.session_client_provided {
                    upsert_header(
                        &mut auth_headers,
                        http::HeaderName::from_static("x-machine-session-id"),
                        http::HeaderValue::from_str(&self.session_id).map_err(|e| {
                            ProxyError::Internal(format!("Invalid Antigravity session header: {e}"))
                        })?,
                    );
                }
                auth_headers
                    .retain(|(name, _)| !name.as_str().eq_ignore_ascii_case("x-goog-api-client"));
            }

            // 注入 Codex OAuth 的 ChatGPT-Account-Id header（如果有 account_id）
            if let Some(ref account_id) = codex_oauth_account_id {
                if let Ok(hv) = http::HeaderValue::from_str(account_id) {
                    auth_headers.push((http::HeaderName::from_static("chatgpt-account-id"), hv));
                }
            }

            let codex_oauth_session_headers = if should_send_codex_oauth_session_headers {
                codex_oauth_upstream_session_id
                    .as_deref()
                    .map(build_codex_oauth_session_headers)
                    .unwrap_or_default()
            } else {
                Vec::new()
            };

            // --- Copilot 优化器：动态 header 注入 ---
            if let Some((ref classification, ref det_request_id, ref interaction_id)) =
                copilot_optimization
            {
                for (name, value) in auth_headers.iter_mut() {
                    match name.as_str() {
                        "x-initiator" if self.copilot_optimizer_config.request_classification => {
                            *value = http::HeaderValue::from_static(classification.initiator);
                        }
                        "x-interaction-type" if classification.is_subagent => {
                            // 子代理请求：conversation-subagent 不计 premium interaction
                            *value = http::HeaderValue::from_static("conversation-subagent");
                        }
                        "x-request-id" | "x-agent-task-id" => {
                            if let Some(ref det_id) = det_request_id {
                                if let Ok(hv) = http::HeaderValue::from_str(det_id) {
                                    *value = hv;
                                }
                            }
                        }
                        _ => {}
                    }
                }

                // x-interaction-id：仅在有 session 时注入（不在 get_auth_headers 中）
                if let Some(ref iid) = interaction_id {
                    if let Ok(hv) = http::HeaderValue::from_str(iid) {
                        auth_headers.push((http::HeaderName::from_static("x-interaction-id"), hv));
                    }
                }

                if classification.is_subagent {
                    log::info!(
                    "[Copilot] 子代理请求: x-initiator=agent, x-interaction-type=conversation-subagent"
                );
                }
            }

            // Copilot 指纹头名（由 get_auth_headers 注入，需在原始头中去重）
            let copilot_fingerprint_headers: &[&str] = if is_copilot {
                &[
                    "user-agent",
                    "editor-version",
                    "editor-plugin-version",
                    "copilot-integration-id",
                    "x-github-api-version",
                    "openai-intent",
                    // 新增 headers
                    "x-initiator",
                    "x-interaction-type",
                    "x-interaction-id",
                    "x-vscode-user-agent-library-version",
                    "x-request-id",
                    "x-agent-task-id",
                ]
            } else {
                &[]
            };

            // 预计算上游 host 值（用于在原位替换 host header）
            let upstream_host = url
                .parse::<http::Uri>()
                .ok()
                .and_then(|u| u.authority().map(|a| a.to_string()));

            let should_send_anthropic_headers = adapter.name() == "Claude"
                && matches!(resolved_claude_api_format.as_deref(), Some("anthropic"));

            // 预计算 anthropic-beta 值（仅 Claude）
            let anthropic_beta_value = if should_send_anthropic_headers {
                Some(build_anthropic_beta_value(
                    headers,
                    is_claude_oauth_provider,
                ))
            } else {
                None
            };

            // ============================================================
            // 构建有序 HeaderMap — 内联替换，保持客户端原始顺序
            // ============================================================
            let mut ordered_headers = http::HeaderMap::new();
            let mut saw_auth = false;
            let mut saw_accept_encoding = false;
            let mut saw_anthropic_beta = false;
            let mut saw_anthropic_version = false;

            for (key, value) in headers {
                let key_str = key.as_str();

                // --- host — 原位替换为上游 host（保持客户端原始位置） ---
                if key_str.eq_ignore_ascii_case("host") {
                    if let Some(ref host_val) = upstream_host {
                        if let Ok(hv) = http::HeaderValue::from_str(host_val) {
                            ordered_headers.append(key.clone(), hv);
                        }
                    }
                    continue;
                }

                // --- 连接 / 追踪 / CDN 类 — 无条件跳过 ---
                if matches!(
                    key_str,
                    "content-length"
                        | "transfer-encoding"
                        | "x-forwarded-host"
                        | "x-forwarded-port"
                        | "x-forwarded-proto"
                        | "forwarded"
                        | "cf-connecting-ip"
                        | "cf-ipcountry"
                        | "cf-ray"
                        | "cf-visitor"
                        | "true-client-ip"
                        | "fastly-client-ip"
                        | "x-azure-clientip"
                        | "x-azure-fdid"
                        | "x-azure-ref"
                        | "akamai-origin-hop"
                        | "x-akamai-config-log-detail"
                        | "x-request-id"
                        | "x-correlation-id"
                        | "x-trace-id"
                        | "x-amzn-trace-id"
                        | "x-b3-traceid"
                        | "x-b3-spanid"
                        | "x-b3-parentspanid"
                        | "x-b3-sampled"
                        | "traceparent"
                        | "tracestate"
                ) {
                    continue;
                }

                // --- 认证类 — 用 adapter 提供的认证头替换（在原始位置） ---
                if key_str.eq_ignore_ascii_case("authorization")
                    || key_str.eq_ignore_ascii_case("x-api-key")
                    || key_str.eq_ignore_ascii_case("x-goog-api-key")
                    || key_str.eq_ignore_ascii_case("x-cc-switch-user-email")
                {
                    if !saw_auth {
                        saw_auth = true;
                        for (ah_name, ah_value) in &auth_headers {
                            ordered_headers.append(ah_name.clone(), ah_value.clone());
                        }
                    }
                    continue;
                }

                // --- accept-encoding — transform / SSE 路径强制 identity，其余保留原值 ---
                if key_str.eq_ignore_ascii_case("accept-encoding") {
                    if !saw_accept_encoding {
                        saw_accept_encoding = true;
                        if force_identity_encoding {
                            ordered_headers.append(
                                http::header::ACCEPT_ENCODING,
                                http::HeaderValue::from_static("identity"),
                            );
                        } else {
                            ordered_headers.append(key.clone(), value.clone());
                        }
                    }
                    continue;
                }

                // --- anthropic-beta — 用重建值替换（确保含 claude-code 标记） ---
                if key_str.eq_ignore_ascii_case("anthropic-beta") {
                    if !saw_anthropic_beta {
                        saw_anthropic_beta = true;
                        if let Some(ref beta_val) = anthropic_beta_value {
                            if let Ok(hv) = http::HeaderValue::from_str(beta_val) {
                                ordered_headers.append("anthropic-beta", hv);
                            }
                        }
                    }
                    continue;
                }

                // --- anthropic-version — 透传客户端值 ---
                if key_str.eq_ignore_ascii_case("anthropic-version") {
                    if should_send_anthropic_headers {
                        saw_anthropic_version = true;
                        ordered_headers.append(key.clone(), value.clone());
                    }
                    continue;
                }

                // --- Copilot 指纹头 — 跳过（由 auth_headers 提供） ---
                if copilot_fingerprint_headers
                    .iter()
                    .any(|h| key_str.eq_ignore_ascii_case(h))
                {
                    continue;
                }

                // --- 默认：透传 ---
                ordered_headers.append(key.clone(), value.clone());
            }

            // 如果原始请求中没有认证头，在末尾追加
            if !saw_auth && !auth_headers.is_empty() {
                for (ah_name, ah_value) in &auth_headers {
                    ordered_headers.append(ah_name.clone(), ah_value.clone());
                }
            }

            // transform / SSE 路径在缺失时补 identity；普通透传不主动补 accept-encoding
            if !saw_accept_encoding && force_identity_encoding {
                ordered_headers.append(
                    http::header::ACCEPT_ENCODING,
                    http::HeaderValue::from_static("identity"),
                );
            }

            // 如果原始请求中没有 anthropic-beta 且有值需要添加，追加
            if !saw_anthropic_beta {
                if let Some(ref beta_val) = anthropic_beta_value {
                    if let Ok(hv) = http::HeaderValue::from_str(beta_val) {
                        ordered_headers.append("anthropic-beta", hv);
                    }
                }
            }

            // anthropic-version：仅在缺失时补充默认值
            if should_send_anthropic_headers && !saw_anthropic_version {
                ordered_headers.append(
                    "anthropic-version",
                    http::HeaderValue::from_static("2023-06-01"),
                );
            }

            // Codex OAuth 反代尽量对齐官方 Codex CLI 的会话路由信号。
            // 只发送客户端提供的 session_id；生成的 UUID 每次不同，反而会破坏前缀缓存。
            for (name, value) in codex_oauth_session_headers {
                ordered_headers.insert(name, value);
            }

            let mut outbound_body = filtered_body.clone();
            if is_gemini_code_assist_provider(app_type, provider)
                && outbound_body.get("project").is_none()
            {
                let token = gemini_oauth_access_token.as_deref().ok_or_else(|| {
                    ProxyError::AuthError(
                        "Google Gemini OAuth 认证失败: 未获取到 access token".to_string(),
                    )
                })?;
                if let Some(project_id) = load_gemini_code_assist_project_for_forward(token).await?
                {
                    outbound_body["project"] = serde_json::json!(project_id);
                }
            }
            if is_antigravity_oauth_provider(app_type, provider) {
                if antigravity_oauth_access_token.is_none() {
                    return Err(ProxyError::AuthError(
                        "Antigravity OAuth 认证失败: 未获取到 access token".to_string(),
                    ));
                }
                let project_id = antigravity_project_id.as_deref().ok_or_else(|| {
                    ProxyError::AuthError(
                        "Antigravity OAuth 认证失败: 未获取到 project id".to_string(),
                    )
                })?;
                outbound_body["project"] = serde_json::json!(project_id);
                if outbound_body
                    .get("model")
                    .and_then(|value| value.as_str())
                    .map(|model| model.to_ascii_lowercase().contains("claude"))
                    .unwrap_or(false)
                {
                    ordered_headers.insert(
                        "anthropic-beta",
                        http::HeaderValue::from_static(
                            "claude-code-20250219,interleaved-thinking-2025-05-14,fine-grained-tool-streaming-2025-05-14",
                        ),
                    );
                }
            }

            // 序列化请求体
            let body_bytes = serde_json::to_vec(&outbound_body).map_err(|e| {
                ProxyError::Internal(format!("Failed to serialize request body: {e}"))
            })?;

            // 确保 content-type 存在
            if !ordered_headers.contains_key(http::header::CONTENT_TYPE) {
                ordered_headers.insert(
                    http::header::CONTENT_TYPE,
                    http::HeaderValue::from_static("application/json"),
                );
            }

            reject_proxy_placeholder_for_managed_account_upstream(&url, &ordered_headers)?;

            // 输出请求信息日志
            let tag = adapter.name();
            let request_model = outbound_body
                .get("model")
                .and_then(|v| v.as_str())
                .unwrap_or("<none>");
            log::info!("[{tag}] >>> 请求 URL: {url} (model={request_model})");
            if log::log_enabled!(log::Level::Debug) {
                if let Ok(body_str) = serde_json::to_string(&outbound_body) {
                    log::debug!(
                        "[{tag}] >>> 请求体内容 ({}字节): {}",
                        body_str.len(),
                        body_str
                    );
                }
            }

            // 确定超时
            let timeout = if self.non_streaming_timeout.is_zero() {
                std::time::Duration::from_secs(600) // 默认 600 秒
            } else {
                self.non_streaming_timeout
            };

            // 获取全局代理 URL
            let upstream_proxy_url: Option<String> = super::http_client::get_current_proxy_url();

            // SOCKS5 代理不支持 CONNECT 隧道，需要用 reqwest
            let is_socks_proxy = upstream_proxy_url
                .as_deref()
                .map(|u| u.starts_with("socks5"))
                .unwrap_or(false);

            let uri: http::Uri = url
                .parse()
                .map_err(|e| ProxyError::ForwardFailed(format!("Invalid URL '{url}': {e}")))?;
            // 发送请求
            let response = if is_socks_proxy {
                // SOCKS5 代理：只能走 reqwest（不支持 header case 保留）
                log::debug!("[Forwarder] Using reqwest for SOCKS5 proxy");
                let client = super::http_client::get();
                let mut request = client.post(&url);
                if !self.non_streaming_timeout.is_zero() {
                    request = request.timeout(self.non_streaming_timeout);
                }
                for (key, value) in &ordered_headers {
                    request = request.header(key, value);
                }
                let reqwest_resp = request
                    .body(body_bytes)
                    .send()
                    .await
                    .map_err(map_reqwest_send_error)?;
                ProxyResponse::Reqwest(reqwest_resp)
            } else {
                // HTTP 代理或直连：走 hyper raw write（保持 header 大小写）
                // 如果有 HTTP 代理，hyper_client 会用 CONNECT 隧道穿过代理
                super::hyper_client::send_request(
                    uri,
                    http::Method::POST,
                    ordered_headers,
                    extensions.clone(),
                    body_bytes,
                    timeout,
                    upstream_proxy_url.as_deref(),
                )
                .await?
            };

            // 检查响应状态
            let status = response.status();

            if status.is_success() {
                break response;
            }

            let status_code = status.as_u16();

            // OAuth 401 单次重试：作废缓存 token 并重新走完整认证注入流程
            if status_code == 401 && !oauth_retried {
                if let Some((kind, account_id)) = oauth_kind_used.clone() {
                    if let Some(app_handle) = &self.app_handle {
                        log::warn!(
                        "[OAuthRetry] 上游返回 401 (kind={:?}, account={account_id})，作废缓存 token 后重试一次",
                        kind
                    );
                        match kind {
                            OAuthKind::Claude => {
                                let state = app_handle.state::<ClaudeOAuthState>();
                                state
                                    .0
                                    .read()
                                    .await
                                    .invalidate_cached_token(&account_id)
                                    .await;
                            }
                            OAuthKind::Codex => {
                                let state = app_handle.state::<CodexOAuthState>();
                                state
                                    .0
                                    .read()
                                    .await
                                    .invalidate_cached_token(&account_id)
                                    .await;
                            }
                            OAuthKind::Copilot => {
                                let state = app_handle.state::<CopilotAuthState>();
                                state
                                    .0
                                    .read()
                                    .await
                                    .invalidate_cached_token(&account_id)
                                    .await;
                            }
                            OAuthKind::Gemini => {
                                let state = app_handle.state::<GeminiOAuthState>();
                                state
                                    .0
                                    .read()
                                    .await
                                    .invalidate_cached_token(&account_id)
                                    .await;
                            }
                            OAuthKind::Antigravity => {
                                let state = app_handle.state::<AntigravityOAuthState>();
                                state
                                    .0
                                    .read()
                                    .await
                                    .invalidate_cached_token(&account_id)
                                    .await;
                            }
                        }
                        oauth_retried = true;
                        // 消费响应体，释放底层连接
                        let _ = response.bytes().await;
                        continue;
                    }
                }
            }

            let body_text = String::from_utf8(response.bytes().await?.to_vec()).ok();
            return Err(ProxyError::UpstreamError {
                status: status_code,
                body: body_text,
            });
        };

        let response = self
            .prepare_success_response_for_failover(response, request_is_streaming)
            .await?;

        Ok((response, resolved_claude_api_format))
    }

    /// 故障转移开启时，成功不能只看上游响应头。
    ///
    /// - 非流式：先把完整 body 读到内存，读超时/连接中断会回到 retry loop 尝试下一家。
    /// - 流式：至少等首个 chunk 到达，避免上游返回 200 后一直不吐 SSE 时被误记成功。
    async fn prepare_success_response_for_failover(
        &self,
        response: ProxyResponse,
        request_is_streaming: bool,
    ) -> Result<ProxyResponse, ProxyError> {
        if request_is_streaming {
            return self.prime_streaming_response(response).await;
        }

        if self.non_streaming_timeout.is_zero() {
            return Ok(response);
        }

        let status = response.status();
        let headers = response.headers().clone();
        let body_timeout = self.non_streaming_timeout;
        let body = tokio::time::timeout(body_timeout, response.bytes())
            .await
            .map_err(|_| {
                ProxyError::Timeout(format!(
                    "响应体读取超时: {}s（上游发完响应头后 body 未到达）",
                    body_timeout.as_secs()
                ))
            })??;

        Ok(ProxyResponse::buffered(status, headers, body))
    }

    async fn prime_streaming_response(
        &self,
        response: ProxyResponse,
    ) -> Result<ProxyResponse, ProxyError> {
        if self.streaming_first_byte_timeout.is_zero() {
            return Ok(response);
        }

        let status = response.status();
        let headers = response.headers().clone();
        let timeout = self.streaming_first_byte_timeout;
        let mut stream = Box::pin(response.bytes_stream());

        let first = tokio::time::timeout(timeout, stream.next())
            .await
            .map_err(|_| {
                ProxyError::Timeout(format!(
                    "流式响应首包超时: {}s（上游已返回响应头但未返回数据）",
                    timeout.as_secs()
                ))
            })?;

        let Some(first) = first else {
            return Err(ProxyError::ForwardFailed(
                "流式响应在首包到达前结束".to_string(),
            ));
        };

        let first =
            first.map_err(|e| ProxyError::ForwardFailed(format!("读取流式响应首包失败: {e}")))?;

        let replay = futures::stream::once(async move { Ok(first) }).chain(stream);
        Ok(ProxyResponse::streamed(status, headers, replay))
    }

    async fn resolve_claude_api_format(
        &self,
        provider: &Provider,
        body: &Value,
        is_copilot: bool,
    ) -> String {
        if provider.is_antigravity_oauth_provider() {
            return "gemini_native".to_string();
        }
        if !is_copilot {
            return super::providers::get_claude_api_format(provider).to_string();
        }

        let model = body.get("model").and_then(|value| value.as_str());
        if let Some(model_id) = model {
            if self
                .is_copilot_openai_vendor_model(provider, model_id)
                .await
            {
                return "openai_responses".to_string();
            }
        }

        "openai_chat".to_string()
    }

    /// 用 Copilot live `/models` 列表确认 model ID 真实可用，找不到时按 family 降级。
    /// 命中缓存后是同步的；首次请求或 5 min 缓存过期后会触发一次 HTTP。
    async fn apply_copilot_live_model_resolution(
        &self,
        provider: &Provider,
        body: &mut serde_json::Value,
    ) {
        let Some(model_id) = body.get("model").and_then(|v| v.as_str()) else {
            return;
        };
        let model_id = model_id.to_string();

        let Some(app_handle) = &self.app_handle else {
            return;
        };
        let copilot_state = app_handle.state::<CopilotAuthState>();
        let copilot_auth = copilot_state.0.read().await;
        let account_id = provider
            .meta
            .as_ref()
            .and_then(|m| m.managed_account_id_for("github_copilot"));

        let models_result = match account_id.as_deref() {
            Some(id) => copilot_auth.fetch_models_for_account(id).await,
            None => copilot_auth.fetch_models().await,
        };

        let models = match models_result {
            Ok(m) => m,
            Err(err) => {
                log::debug!("[Copilot] live model list unavailable, skip resolution: {err}");
                return;
            }
        };

        if let Some(resolved) =
            super::providers::copilot_model_map::resolve_against_models(&model_id, &models)
        {
            log::info!("[Copilot] live-model resolve: {model_id} → {resolved}");
            body["model"] = serde_json::Value::String(resolved);
        }
    }

    async fn is_copilot_openai_vendor_model(&self, provider: &Provider, model_id: &str) -> bool {
        let Some(app_handle) = &self.app_handle else {
            log::debug!("[Copilot] AppHandle unavailable, fallback to chat/completions");
            return false;
        };

        let copilot_state = app_handle.state::<CopilotAuthState>();
        let copilot_auth = copilot_state.0.read().await;
        let account_id = provider
            .meta
            .as_ref()
            .and_then(|m| m.managed_account_id_for("github_copilot"));

        let vendor_result = match account_id.as_deref() {
            Some(id) => {
                copilot_auth
                    .get_model_vendor_for_account(id, model_id)
                    .await
            }
            None => copilot_auth.get_model_vendor(model_id).await,
        };

        match vendor_result {
            Ok(Some(vendor)) => vendor.eq_ignore_ascii_case("openai"),
            Ok(None) => {
                log::debug!(
                    "[Copilot] Model vendor unavailable for {model_id}, fallback to chat/completions"
                );
                false
            }
            Err(err) => {
                log::warn!(
                    "[Copilot] Failed to resolve model vendor for {model_id}, fallback to chat/completions: {err}"
                );
                false
            }
        }
    }

    fn categorize_proxy_error(&self, error: &ProxyError) -> ErrorCategory {
        match error {
            // 网络和上游错误：都应该尝试下一个供应商
            ProxyError::Timeout(_) => ErrorCategory::Retryable,
            ProxyError::ForwardFailed(_) => ErrorCategory::Retryable,
            ProxyError::ProviderUnhealthy(_) => ErrorCategory::Retryable,
            // 上游 HTTP 错误：按状态码分桶。
            //
            // 客户端请求自身有问题的状态码无论换哪个 provider 都会被拒绝，
            // 继续轮询只会放大错误率、污染熔断器健康度、浪费配额：
            //   400 Bad Request / 422 Unprocessable Entity   ← 请求体格式或语义错误
            //   405 Method Not Allowed / 406 Not Acceptable  ← 方法或 Accept 错误
            //   413 Payload Too Large / 414 URI Too Long     ← 客户端构造超限
            //   415 Unsupported Media Type                    ← Content-Type 错误
            //   501 Not Implemented                           ← 上游协议确实不支持
            //
            // 其他 4xx（401/403/404/408/409/429/451 等）和全部 5xx 都保留
            // Retryable —— 换一家 provider 可能持有不同的 key、配额、地域或模型映射。
            ProxyError::UpstreamError { status, .. } => match *status {
                400 | 405 | 406 | 413 | 414 | 415 | 422 | 501 => ErrorCategory::NonRetryable,
                _ => ErrorCategory::Retryable,
            },
            // Provider 级配置/转换问题：换一个 Provider 可能就能成功
            ProxyError::ConfigError(_) => ErrorCategory::Retryable,
            ProxyError::TransformError(_) => ErrorCategory::Retryable,
            ProxyError::AuthError(_) => ErrorCategory::Retryable,
            ProxyError::StreamIdleTimeout(_) => ErrorCategory::Retryable,
            // 无可用供应商：所有供应商都试过了，无法重试
            ProxyError::NoAvailableProvider => ErrorCategory::NonRetryable,
            // 其他错误（数据库/内部错误等）：不是换供应商能解决的问题
            _ => ErrorCategory::NonRetryable,
        }
    }
}

/// 从 ProxyError 中提取错误消息
fn extract_error_message(error: &ProxyError) -> Option<String> {
    match error {
        ProxyError::UpstreamError { body, .. } => body.clone(),
        _ => Some(error.to_string()),
    }
}

/// 检测 Provider 是否为 Bedrock（通过 CLAUDE_CODE_USE_BEDROCK 环境变量判断）
fn is_bedrock_provider(provider: &Provider) -> bool {
    provider
        .settings_config
        .get("env")
        .and_then(|e| e.get("CLAUDE_CODE_USE_BEDROCK"))
        .and_then(|v| v.as_str())
        .map(|v| v == "1")
        .unwrap_or(false)
}

fn build_retryable_failure_log(
    provider_name: &str,
    attempted_providers: usize,
    total_providers: usize,
    error: &ProxyError,
) -> (&'static str, String) {
    let error_summary = summarize_proxy_error(error);

    if total_providers <= 1 {
        (
            log_fwd::SINGLE_PROVIDER_FAILED,
            format!("Provider {provider_name} 请求失败: {error_summary}"),
        )
    } else {
        (
            log_fwd::PROVIDER_FAILED_RETRY,
            format!(
                "Provider {provider_name} 失败，继续尝试下一个 ({attempted_providers}/{total_providers}): {error_summary}"
            ),
        )
    }
}

fn build_terminal_failure_log(
    attempted_providers: usize,
    total_providers: usize,
    last_error: Option<&ProxyError>,
) -> Option<(&'static str, String)> {
    if total_providers <= 1 {
        return None;
    }

    let error_summary = last_error
        .map(summarize_proxy_error)
        .unwrap_or_else(|| "未知错误".to_string());

    Some((
        log_fwd::ALL_PROVIDERS_FAILED,
        format!(
            "已尝试 {attempted_providers}/{total_providers} 个 Provider，均失败。最后错误: {error_summary}"
        ),
    ))
}

fn summarize_proxy_error(error: &ProxyError) -> String {
    match error {
        ProxyError::UpstreamError { status, body } => {
            let body_summary = body
                .as_deref()
                .map(summarize_upstream_body)
                .filter(|summary| !summary.is_empty());

            match body_summary {
                Some(summary) => format!("上游 HTTP {status}: {summary}"),
                None => format!("上游 HTTP {status}"),
            }
        }
        ProxyError::Timeout(message) => {
            format!("请求超时: {}", summarize_text_for_log(message, 180))
        }
        ProxyError::ForwardFailed(message) => {
            format!("请求转发失败: {}", summarize_text_for_log(message, 180))
        }
        ProxyError::TransformError(message) => {
            format!("响应转换失败: {}", summarize_text_for_log(message, 180))
        }
        ProxyError::ConfigError(message) => {
            format!("配置错误: {}", summarize_text_for_log(message, 180))
        }
        ProxyError::AuthError(message) => {
            format!("认证失败: {}", summarize_text_for_log(message, 180))
        }
        _ => summarize_text_for_log(&error.to_string(), 180),
    }
}

fn summarize_upstream_body(body: &str) -> String {
    if let Ok(json_body) = serde_json::from_str::<Value>(body) {
        if let Some(message) = extract_json_error_message(&json_body) {
            return summarize_text_for_log(&message, 180);
        }

        if let Ok(compact_json) = serde_json::to_string(&json_body) {
            return summarize_text_for_log(&compact_json, 180);
        }
    }

    summarize_text_for_log(body, 180)
}

fn extract_json_error_message(body: &Value) -> Option<String> {
    let candidates = [
        body.pointer("/error/message"),
        body.pointer("/message"),
        body.pointer("/detail"),
        body.pointer("/error"),
    ];

    candidates
        .into_iter()
        .flatten()
        .find_map(|value| value.as_str().map(ToString::to_string))
}

fn is_gemini_code_assist_provider(app_type: &AppType, provider: &Provider) -> bool {
    matches!(app_type, AppType::Gemini)
        && (provider.is_google_gemini_oauth_provider()
            || provider.is_google_gemini_official_with_managed_auth())
}

fn is_antigravity_oauth_provider(app_type: &AppType, provider: &Provider) -> bool {
    matches!(
        app_type,
        AppType::Claude | AppType::ClaudeDesktop | AppType::Gemini
    ) && provider.is_antigravity_oauth_provider()
}

fn extract_forward_base_url(
    app_type: &AppType,
    provider: &Provider,
    adapter: &dyn ProviderAdapter,
) -> Result<String, ProxyError> {
    if is_gemini_code_assist_provider(app_type, provider) {
        return Ok(GEMINI_CODE_ASSIST_BASE_URL.to_string());
    }
    if is_antigravity_oauth_provider(app_type, provider) {
        return Ok(ANTIGRAVITY_BASE_URL.to_string());
    }

    adapter.extract_base_url(provider)
}

fn build_gemini_code_assist_forward_request(
    endpoint: &str,
    request_body: &Value,
) -> Result<(String, Value, String), ProxyError> {
    let model = extract_gemini_model_from_endpoint(endpoint)
        .or_else(|| {
            request_body
                .get("model")
                .and_then(|value| value.as_str())
                .map(ToString::to_string)
        })
        .map(|model| super::gemini_url::normalize_gemini_model_id(&model).to_string())
        .map(|model| normalize_gemini_code_assist_model(&model).to_string())
        .filter(|model| !model.trim().is_empty())
        .ok_or_else(|| {
            ProxyError::ConfigError(
                "Google Gemini OAuth 反代缺少模型名，无法构造 Code Assist 请求".to_string(),
            )
        })?;

    let is_stream = endpoint.contains("streamGenerateContent")
        || endpoint.contains("alt=sse")
        || request_body
            .get("stream")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
    let action = if is_stream {
        "streamGenerateContent"
    } else {
        "generateContent"
    };
    let url = if is_stream {
        format!("{GEMINI_CODE_ASSIST_BASE_URL}/v1internal:{action}?alt=sse")
    } else {
        format!("{GEMINI_CODE_ASSIST_BASE_URL}/v1internal:{action}")
    };

    let mut inner_request = request_body.clone();
    if let Some(obj) = inner_request.as_object_mut() {
        obj.remove("model");
        obj.remove("project");
        obj.remove("stream");
    }

    let body = serde_json::json!({
        "model": model,
        "request": inner_request,
    });

    Ok((url, body, model))
}

pub(crate) fn build_antigravity_forward_request(
    endpoint: &str,
    request_body: &Value,
    session_id: &str,
) -> Result<(String, Value), ProxyError> {
    let model = extract_gemini_model_from_endpoint(endpoint)
        .or_else(|| {
            request_body
                .get("model")
                .and_then(|value| value.as_str())
                .map(ToString::to_string)
        })
        .map(|model| super::gemini_url::normalize_gemini_model_id(&model).to_string())
        .map(|model| crate::services::antigravity_models::normalize_antigravity_model_id(&model))
        .filter(|model| !model.trim().is_empty())
        .ok_or_else(|| {
            ProxyError::ConfigError("Antigravity OAuth 反代缺少模型名，无法构造请求".to_string())
        })?;

    let is_stream = endpoint.contains("streamGenerateContent")
        || endpoint.contains("alt=sse")
        || request_body
            .get("stream")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
    let action = if is_stream {
        "streamGenerateContent"
    } else {
        "generateContent"
    };
    let url = if is_stream {
        format!("{ANTIGRAVITY_BASE_URL}/v1internal:{action}?alt=sse")
    } else {
        format!("{ANTIGRAVITY_BASE_URL}/v1internal:{action}")
    };

    let mut inner_request = sanitize_antigravity_request(request_body.clone());
    if let Some(obj) = inner_request.as_object_mut() {
        obj.remove("model");
        obj.remove("project");
        obj.remove("stream");
        obj.remove("safetySettings");
        obj.entry("sessionId")
            .or_insert_with(|| serde_json::json!(session_id));
    }

    let body = serde_json::json!({
        "model": model,
        "userAgent": "antigravity",
        "requestType": "agent",
        "requestId": antigravity_request_id(),
        "enabledCreditTypes": ["GOOGLE_ONE_AI"],
        "request": inner_request,
    });

    Ok((url, body))
}

fn sanitize_antigravity_request(mut request: Value) -> Value {
    clamp_antigravity_max_output_tokens(&mut request);
    sanitize_antigravity_contents(&mut request);
    sanitize_antigravity_tools(&mut request);
    request
}

fn antigravity_request_id() -> String {
    let timestamp_ms = chrono::Utc::now().timestamp_millis();
    let random = uuid::Uuid::new_v4().simple().to_string();
    format!("agent/{}/{}", timestamp_ms, &random[..8])
}

fn clamp_antigravity_max_output_tokens(request: &mut Value) {
    let Some(max_tokens) = request
        .get_mut("generationConfig")
        .and_then(|value| value.get_mut("maxOutputTokens"))
    else {
        return;
    };
    if max_tokens
        .as_i64()
        .map(|value| value > ANTIGRAVITY_MAX_OUTPUT_TOKENS)
        .unwrap_or(false)
    {
        *max_tokens = serde_json::json!(ANTIGRAVITY_MAX_OUTPUT_TOKENS);
    }
}

fn sanitize_antigravity_contents(request: &mut Value) {
    let Some(contents) = request
        .get_mut("contents")
        .and_then(|value| value.as_array_mut())
    else {
        return;
    };

    for content in contents {
        let has_function_response = content
            .get("parts")
            .and_then(|value| value.as_array())
            .map(|parts| {
                parts
                    .iter()
                    .any(|part| part.get("functionResponse").is_some())
            })
            .unwrap_or(false);
        if has_function_response {
            content["role"] = serde_json::json!("user");
        }

        let Some(parts) = content
            .get_mut("parts")
            .and_then(|value| value.as_array_mut())
        else {
            continue;
        };
        parts.retain(|part| {
            let has_function_call = part.get("functionCall").is_some();
            let has_text = part.get("text").is_some();
            if part.get("thought").and_then(|value| value.as_bool()) == Some(true)
                && !has_function_call
            {
                return false;
            }
            if part.get("thoughtSignature").is_some() && !has_function_call && !has_text {
                return false;
            }
            true
        });
    }
}

fn sanitize_antigravity_tools(request: &mut Value) {
    let Some(tools) = request
        .get_mut("tools")
        .and_then(|value| value.as_array_mut())
    else {
        return;
    };

    let mut declarations = Vec::new();
    for group in tools.iter_mut() {
        let Some(functions) = group
            .get_mut("functionDeclarations")
            .and_then(|value| value.as_array_mut())
        else {
            continue;
        };
        for function in functions.iter_mut() {
            if let Some(name) = function
                .get("name")
                .and_then(|value| value.as_str())
                .map(ToString::to_string)
            {
                function["name"] = serde_json::json!(sanitize_antigravity_function_name(&name));
            }
            if function.get("parameters").is_none() {
                function["parameters"] = serde_json::json!({
                    "type": "object",
                    "properties": {
                        "reason": {
                            "type": "string",
                            "description": "Brief explanation"
                        }
                    },
                    "required": ["reason"]
                });
            }
            declarations.push(function.clone());
        }
    }

    if declarations.is_empty() {
        request.as_object_mut().map(|obj| obj.remove("tools"));
        return;
    }

    *tools = vec![serde_json::json!({ "functionDeclarations": declarations })];
    request["toolConfig"] = serde_json::json!({
        "functionCallingConfig": { "mode": "VALIDATED" }
    });
}

fn sanitize_antigravity_function_name(name: &str) -> String {
    let mut result = String::with_capacity(name.len().min(64));
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | ':' | '-') {
            result.push(ch);
        } else {
            result.push('_');
        }
        if result.len() >= 64 {
            break;
        }
    }
    if !result
        .chars()
        .next()
        .map(|ch| ch.is_ascii_alphabetic() || ch == '_')
        .unwrap_or(false)
    {
        result.insert(0, '_');
        result.truncate(64);
    }
    if result.is_empty() {
        "_unknown".to_string()
    } else {
        result
    }
}

fn normalize_gemini_code_assist_model(model: &str) -> &str {
    match model {
        "gemini-3-flash-preview" => "gemini-3-flash",
        "gemini-3.1-flash-lite-preview" => "gemini-3.1-flash-lite",
        other => other,
    }
}

fn extract_gemini_model_from_endpoint(endpoint: &str) -> Option<String> {
    let path = endpoint.split('?').next().unwrap_or(endpoint);
    let marker = "/models/";
    let start = path.find(marker)? + marker.len();
    let model_and_action = &path[start..];
    let model = model_and_action
        .split(':')
        .next()
        .unwrap_or(model_and_action);
    let model = model.trim_matches('/');
    (!model.is_empty()).then(|| model.to_string())
}

fn gemini_cli_user_agent(model: &str) -> String {
    let model = if model.trim().is_empty() {
        "unknown"
    } else {
        model
    };
    format!(
        "GeminiCLI/{GEMINI_CLI_VERSION}/{model} ({}; {})",
        gemini_cli_platform(),
        gemini_cli_arch()
    )
}

pub(crate) fn antigravity_user_agent() -> String {
    format!(
        "antigravity/1.107.0 {}/{}",
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

fn gemini_cli_platform() -> &'static str {
    match std::env::consts::OS {
        "windows" => "win32",
        other => other,
    }
}

fn gemini_cli_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "x86" => "x86",
        other => other,
    }
}

fn upsert_header(
    headers: &mut Vec<(http::HeaderName, http::HeaderValue)>,
    name: http::HeaderName,
    value: http::HeaderValue,
) {
    if let Some((_, existing)) = headers
        .iter_mut()
        .find(|(candidate, _)| candidate.as_str().eq_ignore_ascii_case(name.as_str()))
    {
        *existing = value;
        return;
    }
    headers.push((name, value));
}

async fn load_gemini_code_assist_project_for_forward(
    token: &str,
) -> Result<Option<String>, ProxyError> {
    let response = super::http_client::get()
        .post(format!(
            "{GEMINI_CODE_ASSIST_BASE_URL}/v1internal:loadCodeAssist"
        ))
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .header("accept", "application/json")
        .header("user-agent", gemini_cli_user_agent("unknown"))
        .header("x-goog-api-client", GEMINI_CLI_API_CLIENT_HEADER)
        .json(&serde_json::json!({
            "metadata": {
                "ideType": "IDE_UNSPECIFIED",
                "platform": "PLATFORM_UNSPECIFIED",
                "pluginType": "GEMINI"
            }
        }))
        .send()
        .await
        .map_err(|e| ProxyError::ForwardFailed(format!("loadCodeAssist 请求失败: {e}")))?;

    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| ProxyError::ForwardFailed(format!("loadCodeAssist 响应读取失败: {e}")))?;
    if !status.is_success() {
        return Err(ProxyError::UpstreamError {
            status: status.as_u16(),
            body: Some(body),
        });
    }

    let value: Value = serde_json::from_str(&body)
        .map_err(|e| ProxyError::ForwardFailed(format!("loadCodeAssist 响应解析失败: {e}")))?;
    Ok(value
        .get("cloudaicompanionProject")
        .and_then(extract_gemini_code_assist_project_id))
}

fn extract_gemini_code_assist_project_id(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Object(obj) => obj
            .get("id")
            .or_else(|| obj.get("projectId"))
            .and_then(|v| v.as_str())
            .map(String::from),
        _ => None,
    }
}

fn split_endpoint_and_query(endpoint: &str) -> (&str, Option<&str>) {
    endpoint
        .split_once('?')
        .map_or((endpoint, None), |(path, query)| (path, Some(query)))
}

fn strip_beta_query(query: Option<&str>) -> Option<String> {
    let filtered = query.map(|query| {
        query
            .split('&')
            .filter(|pair| !pair.is_empty() && !pair.starts_with("beta="))
            .collect::<Vec<_>>()
            .join("&")
    });

    match filtered.as_deref() {
        Some("") | None => None,
        Some(_) => filtered,
    }
}

fn is_claude_messages_path(path: &str) -> bool {
    matches!(path, "/v1/messages" | "/claude/v1/messages")
}

fn rewrite_codex_responses_endpoint_to_chat(endpoint: &str) -> (String, Option<String>) {
    let (_path, query) = split_endpoint_and_query(endpoint);
    let passthrough_query = query.map(ToString::to_string);
    let target_path = "/chat/completions";
    let rewritten = match passthrough_query.as_deref() {
        Some(query) if !query.is_empty() => format!("{target_path}?{query}"),
        _ => target_path.to_string(),
    };

    (rewritten, passthrough_query)
}

fn rewrite_claude_transform_endpoint(
    endpoint: &str,
    api_format: &str,
    is_copilot: bool,
    body: &Value,
) -> (String, Option<String>) {
    let (path, query) = split_endpoint_and_query(endpoint);
    let passthrough_query = if is_claude_messages_path(path) {
        strip_beta_query(query)
    } else {
        query.map(ToString::to_string)
    };

    if !is_claude_messages_path(path) {
        return (endpoint.to_string(), passthrough_query);
    }

    if api_format == "gemini_native" {
        let model =
            super::providers::transform_gemini::extract_gemini_model(body).unwrap_or("unknown");
        // Accept both bare ids (`gemini-2.5-pro`) and the resource-name
        // form (`models/gemini-2.5-pro`) that Gemini SDKs emit. See
        // `normalize_gemini_model_id` for rationale.
        let model = super::gemini_url::normalize_gemini_model_id(model);
        let is_stream = body
            .get("stream")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        let target_path = if is_stream {
            format!("/v1beta/models/{model}:streamGenerateContent")
        } else {
            format!("/v1beta/models/{model}:generateContent")
        };

        let rewritten_query = merge_query_params(
            passthrough_query.as_deref(),
            if is_stream { Some("alt=sse") } else { None },
        );

        let rewritten = match rewritten_query.as_deref() {
            Some(query) if !query.is_empty() => format!("{target_path}?{query}"),
            _ => target_path,
        };

        return (rewritten, rewritten_query);
    }

    let target_path = if is_copilot && api_format == "openai_responses" {
        "/v1/responses"
    } else if is_copilot {
        "/chat/completions"
    } else if api_format == "openai_responses" {
        "/v1/responses"
    } else {
        "/v1/chat/completions"
    };

    let rewritten = match passthrough_query.as_deref() {
        Some(query) if !query.is_empty() => format!("{target_path}?{query}"),
        _ => target_path.to_string(),
    };

    (rewritten, passthrough_query)
}

fn merge_query_params(base_query: Option<&str>, extra_param: Option<&str>) -> Option<String> {
    let mut params: Vec<String> = base_query
        .into_iter()
        .flat_map(|query| query.split('&'))
        .filter(|pair| !pair.is_empty())
        .filter(|pair| !pair.starts_with("alt="))
        .map(ToString::to_string)
        .collect();

    if let Some(extra_param) = extra_param {
        params.push(extra_param.to_string());
    }

    if params.is_empty() {
        None
    } else {
        Some(params.join("&"))
    }
}

fn append_query_to_full_url(base_url: &str, query: Option<&str>) -> String {
    match query {
        Some(query) if !query.is_empty() => {
            if base_url.contains('?') {
                format!("{base_url}&{query}")
            } else {
                format!("{base_url}?{query}")
            }
        }
        _ => base_url.to_string(),
    }
}

fn ensure_claude_oauth_beta_query(url: &str) -> String {
    let (base, query) = split_endpoint_and_query(url);
    match query {
        Some(query) if !query.is_empty() => {
            if query.split('&').any(|part| part == "beta=true") {
                url.to_string()
            } else {
                format!("{base}?beta=true&{query}")
            }
        }
        _ => format!("{base}?beta=true"),
    }
}

fn sign_claude_oauth_messages_body(mut body: Value) -> Value {
    const CCH_SEED: u64 = 0x6E52736AC806831E;
    static CCH_PATTERN: OnceLock<Regex> = OnceLock::new();

    let Some(system) = body.get("system").and_then(|value| value.as_array()) else {
        return body;
    };
    let Some(first_block) = system.first() else {
        return body;
    };
    let Some(text) = first_block.get("text").and_then(|value| value.as_str()) else {
        return body;
    };
    if !text.starts_with("x-anthropic-billing-header:") {
        return body;
    }

    let pattern = CCH_PATTERN
        .get_or_init(|| Regex::new(r"\bcch=([0-9a-f]{5});").expect("valid Claude OAuth cch regex"));
    if !pattern.is_match(text) {
        return body;
    }

    let unsigned_text = pattern.replace(text, "cch=00000;").to_string();
    body["system"][0]["text"] = Value::String(unsigned_text.clone());

    let Ok(unsigned_body) = serde_json::to_vec(&body) else {
        return body;
    };

    let mut hasher = XxHash64::with_seed(CCH_SEED);
    hasher.write(&unsigned_body);
    let cch = format!("{:05x}", hasher.finish() & 0xFFFFF);
    let signed_text = pattern
        .replace(&unsigned_text, format!("cch={cch};"))
        .to_string();
    body["system"][0]["text"] = Value::String(signed_text);
    body
}

/// Claude OAuth (claude.ai 网关) 要求 `system` 首块以 `x-anthropic-billing-header:`
/// 开头，且包含 `cch=XXXXX;` 签名（由 `sign_claude_oauth_messages_body` 填充）。
/// 普通 Anthropic API key / OpenRouter / Kiro 等不经过 claude.ai 网关，不需要这个块。
///
/// 这个函数处理 4 种输入形态，令用户按 Anthropic 官方文档写的 body 也能透明通过
/// Claude OAuth provider——就像直连 `api.anthropic.com` 一样，不需要手动补 billing header：
///
/// 1. 没有 `system` 键 → 注入 `[billing_block]`
/// 2. `system: "字符串"` (旧 Anthropic API 形式) → 转 array 并前置 billing_block
/// 3. `system: [{...}]` 但首块**不是** billing-header → 前置 billing_block
/// 4. `system: [{text: "x-anthropic-billing-header:..."}]` (真实 claude-cli 流量) → 原样不动
///
/// 情形 4 保证现有 claude-cli 通过 share URL 调用的行为完全不变——签名流水线
/// (`sign_claude_oauth_messages_body`) 收到的 body 和以前一样，hash 结果也一样。
fn ensure_claude_oauth_billing_header_system(mut body: Value) -> Value {
    const BILLING_PREFIX: &str = "x-anthropic-billing-header:";
    const BILLING_BLOCK_TEXT: &str =
        "x-anthropic-billing-header: cc_version=2.1.119.47e; cc_entrypoint=sdk-cli; cch=00000;\n\nYou are Claude Code, Anthropic's official CLI for Claude.";

    // 情形 4：首块已有 billing header → 不动（保留 claude-cli 真实 cch 让后续签名重算）。
    if body
        .get("system")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|b| b.get("text"))
        .and_then(|t| t.as_str())
        .is_some_and(|t| t.starts_with(BILLING_PREFIX))
    {
        return body;
    }

    let billing_block = serde_json::json!({"type": "text", "text": BILLING_BLOCK_TEXT});

    let existing_system = body
        .as_object_mut()
        .and_then(|o| o.remove("system"));

    let mut blocks: Vec<Value> = match existing_system {
        // 情形 2：旧式字符串 system → 转成 array block
        Some(Value::String(s)) if !s.is_empty() => {
            vec![serde_json::json!({"type": "text", "text": s})]
        }
        // 空字符串 / null 视同无 system（情形 1）
        Some(Value::String(_)) | Some(Value::Null) | None => Vec::new(),
        // 情形 3：已是 array 但首块没有 billing header
        Some(Value::Array(arr)) => arr,
        // 其它非预期类型（object 等）忽略
        _ => Vec::new(),
    };

    blocks.insert(0, billing_block);
    body["system"] = Value::Array(blocks);
    body
}

fn build_anthropic_beta_value(headers: &axum::http::HeaderMap, is_claude_oauth: bool) -> String {
    const CLAUDE_CODE_BETA: &str = "claude-code-20250219";
    const CLAUDE_OAUTH_BETA: &str = "oauth-2025-04-20";
    const INTERLEAVED_THINKING_BETA: &str = "interleaved-thinking-2025-05-14";

    let mut betas = vec![CLAUDE_CODE_BETA.to_string()];
    if is_claude_oauth {
        betas.push(CLAUDE_OAUTH_BETA.to_string());
    }

    if let Some(beta) = headers
        .get("anthropic-beta")
        .and_then(|value| value.to_str().ok())
    {
        for item in beta
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
        {
            if !betas.iter().any(|existing| existing == item) {
                betas.push(item.to_string());
            }
        }
    }

    if is_claude_oauth && !betas.iter().any(|item| item == INTERLEAVED_THINKING_BETA) {
        betas.push(INTERLEAVED_THINKING_BETA.to_string());
    }

    betas.join(",")
}

fn normalize_codex_oauth_responses_body(mut body: Value, prompt_cache_key: Option<&str>) -> Value {
    body["store"] = Value::Bool(false);
    body["stream"] = Value::Bool(true);

    if body.get("prompt_cache_key").is_none() {
        if let Some(key) = prompt_cache_key
            .map(str::trim)
            .filter(|key| !key.is_empty())
        {
            body["prompt_cache_key"] = Value::String(key.to_string());
        }
    }

    match body.get_mut("include") {
        Some(Value::Array(include)) => {
            let required = Value::String("reasoning.encrypted_content".to_string());
            if !include.iter().any(|item| item == &required) {
                include.push(required);
            }
        }
        _ => {
            body["include"] = Value::Array(vec![Value::String(
                "reasoning.encrypted_content".to_string(),
            )]);
        }
    }

    if body.get("instructions").is_none() {
        body["instructions"] = Value::String(String::new());
    }
    if body.get("tools").is_none() {
        body["tools"] = Value::Array(Vec::new());
    }
    if body.get("parallel_tool_calls").is_none() {
        body["parallel_tool_calls"] = Value::Bool(false);
    }

    if let Some(obj) = body.as_object_mut() {
        for field in CODEX_OAUTH_UNSUPPORTED_RESPONSES_FIELDS {
            obj.remove(*field);
        }
    }

    body
}

const CODEX_OAUTH_UNSUPPORTED_RESPONSES_FIELDS: &[&str] = &[
    "max_output_tokens",
    "temperature",
    "top_p",
    "frequency_penalty",
    "presence_penalty",
    "logit_bias",
    "logprobs",
    "top_logprobs",
    "n",
    "stop",
    "response_format",
    "seed",
    "stream_options",
    "user",
];

fn codex_oauth_upstream_session_id(session_id: &str) -> Option<String> {
    let session_id = session_id.trim();
    if session_id.is_empty() {
        return None;
    }

    let session_id = session_id.strip_prefix("codex_").unwrap_or(session_id);
    let session_id = session_id.trim();
    if session_id.is_empty() {
        None
    } else {
        Some(session_id.to_string())
    }
}

fn maybe_add_share_auth_header(
    auth_headers: &mut Vec<(http::HeaderName, http::HeaderValue)>,
    base_url: &str,
) {
    if !crate::tunnel::config::is_share_tunnel_url(base_url) {
        return;
    }

    let already_has_share_header = auth_headers
        .iter()
        .any(|(name, _)| name.as_str().eq_ignore_ascii_case("x-api-key"));
    if already_has_share_header {
        return;
    }

    let bearer_value = auth_headers
        .iter()
        .find(|(name, _)| name.as_str().eq_ignore_ascii_case("authorization"))
        .and_then(|(_, value)| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|value| !value.is_empty());

    if let Some(token) = bearer_value {
        if let Ok(header_value) = http::HeaderValue::from_str(token) {
            auth_headers.push((http::HeaderName::from_static("x-api-key"), header_value));
        }
    }
}

fn build_codex_oauth_session_headers(
    session_id: &str,
) -> Vec<(http::HeaderName, http::HeaderValue)> {
    let session_id = session_id.trim();
    if session_id.is_empty() {
        return Vec::new();
    }

    let mut headers = Vec::new();
    if let Ok(value) = http::HeaderValue::from_str(session_id) {
        headers.push((http::HeaderName::from_static("session_id"), value.clone()));
        headers.push((http::HeaderName::from_static("x-client-request-id"), value));
    }

    let window_id = format!("{session_id}:0");
    if let Ok(value) = http::HeaderValue::from_str(&window_id) {
        headers.push((http::HeaderName::from_static("x-codex-window-id"), value));
    }

    headers
}

fn reject_proxy_placeholder_for_managed_account_upstream(
    url: &str,
    headers: &http::HeaderMap,
) -> Result<(), ProxyError> {
    if !is_managed_account_upstream_url(url) || !headers_contain_proxy_placeholder(headers) {
        return Ok(());
    }

    Err(ProxyError::AuthError(
        "Managed account proxy auth was not resolved; PROXY_MANAGED must not be sent upstream"
            .to_string(),
    ))
}

fn is_managed_account_upstream_url(url: &str) -> bool {
    let Ok(uri) = url.parse::<http::Uri>() else {
        return false;
    };

    let Some(host) = uri.host().map(str::to_ascii_lowercase) else {
        return false;
    };

    host == "githubcopilot.com"
        || host.ends_with(".githubcopilot.com")
        || (host == "chatgpt.com" && uri.path().starts_with("/backend-api/codex"))
}

fn headers_contain_proxy_placeholder(headers: &http::HeaderMap) -> bool {
    headers.values().any(|value| {
        value
            .to_str()
            .map(|value| value.contains(PROXY_AUTH_PLACEHOLDER))
            .unwrap_or(false)
    })
}

#[cfg(test)]
fn should_preserve_exact_header_case(
    adapter_name: &str,
    provider: &Provider,
    resolved_claude_api_format: Option<&str>,
    is_copilot: bool,
) -> bool {
    if matches!(adapter_name, "Codex" | "Gemini") {
        return false;
    }

    if is_copilot || provider.is_codex_oauth() {
        return false;
    }

    matches!(resolved_claude_api_format, None | Some("anthropic"))
}

fn is_streaming_request(endpoint: &str, body: &Value, headers: &axum::http::HeaderMap) -> bool {
    if body
        .get("stream")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
    {
        return true;
    }

    if endpoint.contains("streamGenerateContent") || endpoint.contains("alt=sse") {
        return true;
    }

    headers
        .get(axum::http::header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .map(|accept| accept.contains("text/event-stream"))
        .unwrap_or(false)
}

#[cfg(test)]
fn should_force_identity_encoding(
    endpoint: &str,
    body: &Value,
    headers: &axum::http::HeaderMap,
) -> bool {
    is_streaming_request(endpoint, body, headers)
}

fn map_reqwest_send_error(error: reqwest::Error) -> ProxyError {
    if error.is_timeout() {
        ProxyError::Timeout(format!("请求超时: {error}"))
    } else if error.is_connect() {
        ProxyError::ForwardFailed(format!("连接失败: {error}"))
    } else {
        ProxyError::ForwardFailed(error.to_string())
    }
}

fn summarize_text_for_log(text: &str, max_chars: usize) -> String {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = normalized.trim();

    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }

    let truncated: String = trimmed.chars().take(max_chars).collect();
    let truncated = truncated.trim_end();
    format!("{truncated}...")
}

fn prepare_upstream_request_body(request_body: Value) -> Value {
    canonicalize_value(filter_private_params_with_whitelist(request_body, &[]))
}

fn log_prompt_cache_trace(
    app_type: &AppType,
    provider: &Provider,
    endpoint: &str,
    api_format: Option<&str>,
    body: &Value,
    session_client_provided: bool,
) {
    if !log::log_enabled!(log::Level::Debug) {
        return;
    }

    let prompt_cache_key = body
        .get("prompt_cache_key")
        .and_then(|value| value.as_str())
        .map(|key| format!("present(len={})", key.len()))
        .unwrap_or_else(|| "absent".to_string());
    let store = body
        .get("store")
        .map(value_for_log)
        .unwrap_or_else(|| "absent".to_string());
    let stream = body
        .get("stream")
        .map(value_for_log)
        .unwrap_or_else(|| "absent".to_string());

    log::debug!(
        "[CacheTrace] app={}, provider={}, endpoint={}, api_format={}, session_client_provided={}, prompt_cache_key={}, store={}, stream={}, instructions_hash={}, tools_hash={}, input_hash={}, include_hash={}, body_hash={}",
        app_type.as_str(),
        provider.id,
        endpoint,
        api_format.unwrap_or("native"),
        session_client_provided,
        prompt_cache_key,
        store,
        stream,
        short_value_hash(body.get("instructions")),
        short_value_hash(body.get("tools")),
        short_value_hash(body.get("input")),
        short_value_hash(body.get("include")),
        short_value_hash(Some(body)),
    );
}

fn value_for_log(value: &Value) -> String {
    match value {
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => value.clone(),
        Value::Null => "null".to_string(),
        Value::Array(values) => format!("array(len={})", values.len()),
        Value::Object(values) => format!("object(len={})", values.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::Database;
    use axum::http::header::{HeaderValue, ACCEPT};
    use axum::http::HeaderMap;
    use bytes::Bytes;
    use http::StatusCode;
    use serde_json::json;
    use std::collections::HashMap;
    use std::time::Duration;

    fn find_header_value<'a>(
        headers: &'a [(http::HeaderName, http::HeaderValue)],
        name: &str,
    ) -> Option<&'a str> {
        headers
            .iter()
            .find(|(header_name, _)| header_name.as_str().eq_ignore_ascii_case(name))
            .and_then(|(_, value)| value.to_str().ok())
    }

    fn test_provider_with_type(provider_type: Option<&str>) -> Provider {
        Provider {
            id: "provider-1".to_string(),
            name: "Provider 1".to_string(),
            settings_config: json!({}),
            website_url: None,
            category: None,
            created_at: None,
            sort_index: None,
            notes: None,
            meta: provider_type.map(|value| crate::provider::ProviderMeta {
                provider_type: Some(value.to_string()),
                ..Default::default()
            }),
            icon: None,
            icon_color: None,
            in_failover_queue: false,
        }
    }

    fn test_forwarder(
        non_streaming_timeout: Duration,
        streaming_first_byte_timeout: Duration,
    ) -> RequestForwarder {
        let db = Arc::new(Database::memory().expect("memory db"));

        RequestForwarder {
            router: Arc::new(ProviderRouter::new(db.clone())),
            status: Arc::new(RwLock::new(ProxyStatus::default())),
            current_providers: Arc::new(RwLock::new(HashMap::new())),
            gemini_shadow: Arc::new(GeminiShadowStore::new()),
            codex_chat_history: Arc::new(CodexChatHistoryStore::default()),
            failover_manager: Arc::new(FailoverSwitchManager::new(db)),
            app_handle: None,
            current_provider_id_at_start: String::new(),
            session_id: String::new(),
            session_client_provided: false,
            rectifier_config: RectifierConfig::default(),
            optimizer_config: OptimizerConfig::default(),
            copilot_optimizer_config: CopilotOptimizerConfig::default(),
            non_streaming_timeout,
            streaming_first_byte_timeout,
            max_attempts: 1,
            override_provider_id: None,
        }
    }

    #[test]
    fn new_with_override_forces_single_attempt_regardless_of_max_retries() {
        let db = Arc::new(Database::memory().expect("memory db"));
        let forwarder = RequestForwarder::new(
            Arc::new(ProviderRouter::new(db.clone())),
            0,
            Arc::new(RwLock::new(ProxyStatus::default())),
            Arc::new(RwLock::new(HashMap::new())),
            Arc::new(GeminiShadowStore::new()),
            Arc::new(CodexChatHistoryStore::default()),
            Arc::new(FailoverSwitchManager::new(db)),
            None,
            String::new(),
            String::new(),
            false,
            0,
            0,
            RectifierConfig::default(),
            OptimizerConfig::default(),
            CopilotOptimizerConfig::default(),
            5, // 试图请求 6 次
            Some("share-bound-provider".to_string()),
        );

        // share 路径下，max_retries 配多大都被压回 1：share 与 provider 是 1:1，
        // 失败时直接 5xx，绝不漂到其他 provider。
        assert_eq!(forwarder.max_attempts, 1);
        assert_eq!(
            forwarder.override_provider_id.as_deref(),
            Some("share-bound-provider")
        );
    }

    #[test]
    fn new_without_override_honors_max_retries() {
        let db = Arc::new(Database::memory().expect("memory db"));
        let forwarder = RequestForwarder::new(
            Arc::new(ProviderRouter::new(db.clone())),
            0,
            Arc::new(RwLock::new(ProxyStatus::default())),
            Arc::new(RwLock::new(HashMap::new())),
            Arc::new(GeminiShadowStore::new()),
            Arc::new(CodexChatHistoryStore::default()),
            Arc::new(FailoverSwitchManager::new(db)),
            None,
            String::new(),
            String::new(),
            false,
            0,
            0,
            RectifierConfig::default(),
            OptimizerConfig::default(),
            CopilotOptimizerConfig::default(),
            3,
            None,
        );

        assert_eq!(forwarder.max_attempts, 4);
        assert!(forwarder.override_provider_id.is_none());
    }

    #[tokio::test]
    async fn maybe_record_current_provider_skips_when_override() {
        let forwarder = test_forwarder(Duration::from_secs(0), Duration::from_secs(0));
        let mut share_forwarder = forwarder;
        share_forwarder.override_provider_id = Some("bound".to_string());

        let provider = test_provider_with_type(None);
        share_forwarder
            .maybe_record_current_provider("claude", &provider)
            .await;

        let map = share_forwarder.current_providers.read().await;
        assert!(
            map.is_empty(),
            "share 请求绝不写 current_providers；实际 = {map:?}"
        );
    }

    #[tokio::test]
    async fn maybe_record_current_provider_writes_when_no_override() {
        let forwarder = test_forwarder(Duration::from_secs(0), Duration::from_secs(0));
        assert!(forwarder.override_provider_id.is_none());

        let provider = test_provider_with_type(None);
        forwarder
            .maybe_record_current_provider("claude", &provider)
            .await;

        let map = forwarder.current_providers.read().await;
        assert_eq!(
            map.get("claude"),
            Some(&("provider-1".to_string(), "Provider 1".to_string()))
        );
    }

    #[test]
    fn single_provider_retryable_log_uses_single_provider_code() {
        let error = ProxyError::UpstreamError {
            status: 429,
            body: Some(r#"{"error":{"message":"rate limit exceeded"}}"#.to_string()),
        };

        let (code, message) = build_retryable_failure_log("PackyCode-response", 1, 1, &error);

        assert_eq!(code, log_fwd::SINGLE_PROVIDER_FAILED);
        assert!(message.contains("Provider PackyCode-response 请求失败"));
        assert!(message.contains("上游 HTTP 429"));
        assert!(message.contains("rate limit exceeded"));
        assert!(!message.contains("切换下一个"));
    }

    #[test]
    fn multi_provider_retryable_log_keeps_failover_wording() {
        let error = ProxyError::Timeout("upstream timed out after 30s".to_string());

        let (code, message) = build_retryable_failure_log("primary", 1, 3, &error);

        assert_eq!(code, log_fwd::PROVIDER_FAILED_RETRY);
        assert!(message.contains("继续尝试下一个 (1/3)"));
        assert!(message.contains("请求超时"));
    }

    #[test]
    fn single_provider_has_no_terminal_all_failed_log() {
        assert!(build_terminal_failure_log(1, 1, None).is_none());
    }

    #[test]
    fn multi_provider_terminal_log_contains_last_error_summary() {
        let error = ProxyError::ForwardFailed("connection reset by peer".to_string());

        let (code, message) =
            build_terminal_failure_log(2, 2, Some(&error)).expect("expected terminal log");

        assert_eq!(code, log_fwd::ALL_PROVIDERS_FAILED);
        assert!(message.contains("已尝试 2/2 个 Provider，均失败"));
        assert!(message.contains("connection reset by peer"));
    }

    #[test]
    fn summarize_upstream_body_prefers_json_message() {
        let body = json!({
            "error": {
                "message": "invalid_request_error: unsupported field"
            },
            "request_id": "req_123"
        });

        let summary = summarize_upstream_body(&body.to_string());

        assert_eq!(summary, "invalid_request_error: unsupported field");
    }

    #[test]
    fn summarize_text_for_log_collapses_whitespace_and_truncates() {
        let summary = summarize_text_for_log("line1\n\n line2   line3", 12);

        assert_eq!(summary, "line1 line2...");
    }

    #[test]
    fn anthropic_beta_for_claude_oauth_includes_oauth_marker() {
        let headers = HeaderMap::new();

        let beta = build_anthropic_beta_value(&headers, true);

        assert_eq!(
            beta,
            "claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14"
        );
    }

    #[test]
    fn anthropic_beta_for_claude_oauth_merges_existing_markers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "anthropic-beta",
            HeaderValue::from_static("custom-beta,claude-code-20250219"),
        );

        let beta = build_anthropic_beta_value(&headers, true);

        assert_eq!(
            beta,
            "claude-code-20250219,oauth-2025-04-20,custom-beta,interleaved-thinking-2025-05-14"
        );
    }

    #[test]
    fn anthropic_beta_without_oauth_keeps_legacy_default() {
        let headers = HeaderMap::new();

        let beta = build_anthropic_beta_value(&headers, false);

        assert_eq!(beta, "claude-code-20250219");
    }

    // ── ensure_claude_oauth_billing_header_system tests ──────────────────────

    const BILLING_PREFIX_FOR_TEST: &str = "x-anthropic-billing-header:";

    #[test]
    fn inject_billing_header_when_no_system() {
        let body = json!({"model": "claude-opus-4-7", "max_tokens": 16, "messages": []});
        let result = ensure_claude_oauth_billing_header_system(body);
        let system = result["system"].as_array().expect("system must be array");
        assert_eq!(system.len(), 1);
        assert!(system[0]["text"]
            .as_str()
            .unwrap_or("")
            .starts_with(BILLING_PREFIX_FOR_TEST));
    }

    #[test]
    fn inject_billing_header_prepends_when_string_system() {
        let body = json!({"model": "x", "max_tokens": 1, "system": "Be helpful.", "messages": []});
        let result = ensure_claude_oauth_billing_header_system(body);
        let system = result["system"].as_array().expect("system must be array");
        // billing block at [0], original string wrapped at [1]
        assert_eq!(system.len(), 2);
        assert!(system[0]["text"]
            .as_str()
            .unwrap_or("")
            .starts_with(BILLING_PREFIX_FOR_TEST));
        assert_eq!(system[1]["text"].as_str().unwrap_or(""), "Be helpful.");
    }

    #[test]
    fn inject_billing_header_prepends_when_array_system_has_no_billing_block() {
        let body = json!({
            "model": "x",
            "max_tokens": 1,
            "system": [{"type": "text", "text": "Custom instructions."}],
            "messages": []
        });
        let result = ensure_claude_oauth_billing_header_system(body);
        let system = result["system"].as_array().expect("system must be array");
        assert_eq!(system.len(), 2);
        assert!(system[0]["text"]
            .as_str()
            .unwrap_or("")
            .starts_with(BILLING_PREFIX_FOR_TEST));
        assert_eq!(system[1]["text"].as_str().unwrap_or(""), "Custom instructions.");
    }

    /// 真实 claude-cli 流量：system 首块已有 billing header → 原样不动，
    /// 保证 sign_claude_oauth_messages_body 收到和以前一样的 body。
    #[test]
    fn inject_billing_header_noop_when_billing_block_already_present() {
        let original_text = "x-anthropic-billing-header: cc_version=2.1; cch=abcde;\n\nYou are Claude Code.";
        let body = json!({
            "model": "x",
            "max_tokens": 1,
            "system": [{"type": "text", "text": original_text}],
            "messages": []
        });
        let result = ensure_claude_oauth_billing_header_system(body);
        let system = result["system"].as_array().expect("system must be array");
        // 没有多出来的块
        assert_eq!(system.len(), 1);
        assert_eq!(system[0]["text"].as_str().unwrap_or(""), original_text);
    }

    #[test]
    fn canonical_json_sorts_object_keys_for_cache_trace_hashes() {
        let left = json!({
            "tools": [
                {
                    "parameters": {
                        "properties": {
                            "a": {"type": "number"}
                        },
                        "type": "object"
                    },
                    "name": "lookup"
                }
            ]
        });
        let right = json!({
            "tools": [
                {
                    "name": "lookup",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "a": {"type": "number"},
                            "b": {"type": "string"}
                        }
                    }
                }
            ]
        });

        assert_eq!(
            crate::proxy::json_canonical::canonical_json_string(&left),
            crate::proxy::json_canonical::canonical_json_string(&right)
        );
        assert_eq!(
            short_value_hash(Some(&left)),
            short_value_hash(Some(&right))
        );
    }

    #[test]
    fn prepare_upstream_request_body_filters_private_fields_and_canonicalizes_order() {
        let body = json!({
            "z": 1,
            "_internal": "drop",
            "tools": [
                {
                    "name": "lookup",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "_id": {
                                "_private_note": "drop",
                                "type": "string"
                            },
                            "b": {"type": "number"},
                            "a": {"type": "string"}
                        }
                    }
                }
            ],
            "a": 2
        });

        let prepared = prepare_upstream_request_body(body);

        assert!(prepared.get("_internal").is_none());
        assert!(prepared["tools"][0]["parameters"]["properties"]
            .get("_id")
            .is_some());
        assert!(prepared["tools"][0]["parameters"]["properties"]["_id"]
            .get("_private_note")
            .is_none());
        assert_eq!(
            serde_json::to_string(&prepared).unwrap(),
            r#"{"a":2,"tools":[{"name":"lookup","parameters":{"properties":{"_id":{"type":"string"},"a":{"type":"string"},"b":{"type":"number"}},"type":"object"}}],"z":1}"#
        );
    }

    #[tokio::test]
    async fn non_streaming_success_is_buffered_before_marking_provider_successful() {
        let forwarder = test_forwarder(Duration::from_secs(1), Duration::from_secs(1));
        let response = ProxyResponse::streamed(
            StatusCode::OK,
            HeaderMap::new(),
            futures::stream::once(async {
                tokio::time::sleep(Duration::from_millis(10)).await;
                Ok::<Bytes, std::io::Error>(Bytes::from_static(b"{\"ok\":true}"))
            }),
        );

        let prepared = forwarder
            .prepare_success_response_for_failover(response, false)
            .await
            .expect("response should be buffered");

        assert_eq!(
            prepared.bytes().await.unwrap(),
            Bytes::from_static(b"{\"ok\":true}")
        );
    }

    #[tokio::test]
    async fn non_streaming_body_read_error_is_retryable_before_success_record() {
        let forwarder = test_forwarder(Duration::from_secs(1), Duration::from_secs(1));
        let response = ProxyResponse::streamed(
            StatusCode::OK,
            HeaderMap::new(),
            futures::stream::once(async {
                Err::<Bytes, std::io::Error>(std::io::Error::other("body boom"))
            }),
        );

        let err = match forwarder
            .prepare_success_response_for_failover(response, false)
            .await
        {
            Ok(_) => panic!("body read errors should fail the attempt"),
            Err(err) => err,
        };

        assert!(matches!(err, ProxyError::ForwardFailed(_)));
    }

    #[tokio::test]
    async fn streaming_success_primes_first_chunk_and_replays_it() {
        let forwarder = test_forwarder(Duration::from_secs(1), Duration::from_secs(1));
        let response = ProxyResponse::streamed(
            StatusCode::OK,
            HeaderMap::new(),
            futures::stream::iter(vec![
                Ok::<Bytes, std::io::Error>(Bytes::from_static(b"first")),
                Ok::<Bytes, std::io::Error>(Bytes::from_static(b"second")),
            ]),
        );

        let prepared = forwarder
            .prepare_success_response_for_failover(response, true)
            .await
            .expect("stream should be primed");

        assert_eq!(
            prepared.bytes().await.unwrap(),
            Bytes::from_static(b"firstsecond")
        );
    }

    #[tokio::test]
    async fn streaming_first_chunk_error_is_retryable_before_success_record() {
        let forwarder = test_forwarder(Duration::from_secs(1), Duration::from_secs(1));
        let response = ProxyResponse::streamed(
            StatusCode::OK,
            HeaderMap::new(),
            futures::stream::once(async {
                Err::<Bytes, std::io::Error>(std::io::Error::other("first chunk boom"))
            }),
        );

        let err = match forwarder
            .prepare_success_response_for_failover(response, true)
            .await
        {
            Ok(_) => panic!("first chunk errors should fail the attempt"),
            Err(err) => err,
        };

        assert!(matches!(err, ProxyError::ForwardFailed(_)));
    }

    #[test]
    fn codex_oauth_session_headers_match_codex_cache_identity() {
        let headers = build_codex_oauth_session_headers("session-123");
        let mut map = HeaderMap::new();
        for (name, value) in headers {
            map.insert(name, value);
        }

        assert_eq!(
            map.get("session_id"),
            Some(&HeaderValue::from_static("session-123"))
        );
        assert_eq!(
            map.get("x-client-request-id"),
            Some(&HeaderValue::from_static("session-123"))
        );
        assert_eq!(
            map.get("x-codex-window-id"),
            Some(&HeaderValue::from_static("session-123:0"))
        );
    }

    #[test]
    fn managed_account_upstream_rejects_proxy_managed_placeholder_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            HeaderValue::from_static("Bearer PROXY_MANAGED"),
        );

        let err = reject_proxy_placeholder_for_managed_account_upstream(
            "https://api.githubcopilot.com/chat/completions",
            &headers,
        )
        .expect_err("placeholder should be rejected before upstream");

        assert!(matches!(
            err,
            ProxyError::AuthError(message) if message.contains("PROXY_MANAGED")
        ));
    }

    #[test]
    fn codex_oauth_upstream_rejects_proxy_managed_placeholder_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            HeaderValue::from_static("Bearer PROXY_MANAGED"),
        );

        let err = reject_proxy_placeholder_for_managed_account_upstream(
            "https://chatgpt.com/backend-api/codex/responses",
            &headers,
        )
        .expect_err("placeholder should be rejected before upstream");

        assert!(matches!(
            err,
            ProxyError::AuthError(message) if message.contains("PROXY_MANAGED")
        ));
    }

    #[test]
    fn non_managed_upstream_allows_proxy_managed_placeholder_guard() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            HeaderValue::from_static("Bearer PROXY_MANAGED"),
        );

        reject_proxy_placeholder_for_managed_account_upstream(
            "https://api.example.com/v1/messages",
            &headers,
        )
        .expect("guard is scoped to managed-account upstreams");
    }

    #[test]
    fn exact_header_case_preserved_for_native_claude_only() {
        let provider = test_provider_with_type(None);

        assert!(should_preserve_exact_header_case(
            "Claude",
            &provider,
            Some("anthropic"),
            false
        ));
        assert!(!should_preserve_exact_header_case(
            "Claude",
            &provider,
            Some("openai_responses"),
            false
        ));
        assert!(!should_preserve_exact_header_case(
            "Codex", &provider, None, false
        ));
        assert!(!should_preserve_exact_header_case(
            "Gemini", &provider, None, false
        ));
    }

    #[test]
    fn exact_header_case_skipped_for_codex_oauth_and_copilot() {
        let codex_oauth = test_provider_with_type(Some("codex_oauth"));
        let copilot = test_provider_with_type(Some("github_copilot"));

        assert!(!should_preserve_exact_header_case(
            "Claude",
            &codex_oauth,
            Some("openai_responses"),
            false
        ));
        assert!(!should_preserve_exact_header_case(
            "Claude",
            &copilot,
            Some("openai_chat"),
            true
        ));
    }

    #[test]
    fn rewrite_claude_transform_endpoint_strips_beta_for_chat_completions() {
        let (endpoint, passthrough_query) = rewrite_claude_transform_endpoint(
            "/v1/messages?beta=true&foo=bar",
            "openai_chat",
            false,
            &json!({ "model": "gpt-5.4" }),
        );

        assert_eq!(endpoint, "/v1/chat/completions?foo=bar");
        assert_eq!(passthrough_query.as_deref(), Some("foo=bar"));
    }

    #[test]
    fn rewrite_claude_transform_endpoint_strips_beta_for_responses() {
        let (endpoint, passthrough_query) = rewrite_claude_transform_endpoint(
            "/claude/v1/messages?beta=true&x-id=1",
            "openai_responses",
            false,
            &json!({ "model": "gpt-5.4" }),
        );

        assert_eq!(endpoint, "/v1/responses?x-id=1");
        assert_eq!(passthrough_query.as_deref(), Some("x-id=1"));
    }

    #[test]
    fn rewrite_codex_responses_endpoint_to_chat_preserves_query() {
        let (endpoint, passthrough_query) =
            rewrite_codex_responses_endpoint_to_chat("/v1/responses?foo=bar");

        assert_eq!(endpoint, "/chat/completions?foo=bar");
        assert_eq!(passthrough_query.as_deref(), Some("foo=bar"));
    }

    #[test]
    fn rewrite_codex_responses_compact_endpoint_to_chat_preserves_query() {
        let (endpoint, passthrough_query) =
            rewrite_codex_responses_endpoint_to_chat("/v1/responses/compact?foo=bar");

        assert_eq!(endpoint, "/chat/completions?foo=bar");
        assert_eq!(passthrough_query.as_deref(), Some("foo=bar"));
    }

    #[test]
    fn rewrite_claude_transform_endpoint_uses_copilot_path() {
        let (endpoint, passthrough_query) = rewrite_claude_transform_endpoint(
            "/v1/messages?beta=true&x-id=1",
            "anthropic",
            true,
            &json!({ "model": "claude-sonnet-4-6" }),
        );

        assert_eq!(endpoint, "/chat/completions?x-id=1");
        assert_eq!(passthrough_query.as_deref(), Some("x-id=1"));
    }

    #[test]
    fn rewrite_claude_transform_endpoint_uses_copilot_responses_path() {
        let (endpoint, passthrough_query) = rewrite_claude_transform_endpoint(
            "/v1/messages?beta=true&x-id=1",
            "openai_responses",
            true,
            &json!({ "model": "gpt-5.4" }),
        );

        assert_eq!(endpoint, "/v1/responses?x-id=1");
        assert_eq!(passthrough_query.as_deref(), Some("x-id=1"));
    }

    #[test]
    fn rewrite_claude_transform_endpoint_maps_gemini_generate_content() {
        let (endpoint, passthrough_query) = rewrite_claude_transform_endpoint(
            "/v1/messages?beta=true&x-id=1",
            "gemini_native",
            false,
            &json!({ "model": "gemini-2.5-pro" }),
        );

        assert_eq!(
            endpoint,
            "/v1beta/models/gemini-2.5-pro:generateContent?x-id=1"
        );
        assert_eq!(passthrough_query.as_deref(), Some("x-id=1"));
    }

    /// Regression: body.model arriving as the resource-name form
    /// `models/gemini-2.5-pro` must not produce a doubled
    /// `/v1beta/models/models/...` path.
    #[test]
    fn rewrite_claude_transform_endpoint_strips_gemini_model_resource_prefix() {
        let (endpoint, _) = rewrite_claude_transform_endpoint(
            "/v1/messages",
            "gemini_native",
            false,
            &json!({ "model": "models/gemini-2.5-pro" }),
        );

        assert_eq!(endpoint, "/v1beta/models/gemini-2.5-pro:generateContent");
    }

    #[test]
    fn rewrite_claude_transform_endpoint_maps_gemini_streaming() {
        let (endpoint, passthrough_query) = rewrite_claude_transform_endpoint(
            "/v1/messages?beta=true",
            "gemini_native",
            false,
            &json!({ "model": "gemini-2.5-flash", "stream": true }),
        );

        assert_eq!(
            endpoint,
            "/v1beta/models/gemini-2.5-flash:streamGenerateContent?alt=sse"
        );
        assert_eq!(passthrough_query.as_deref(), Some("alt=sse"));
    }

    #[test]
    fn build_gemini_code_assist_forward_request_wraps_native_stream() {
        let (url, body, model) = build_gemini_code_assist_forward_request(
            "/v1beta/models/gemini-3-flash-preview:streamGenerateContent?alt=sse",
            &json!({
                "contents": [{
                    "role": "user",
                    "parts": [{ "text": "ping" }]
                }],
                "stream": true
            }),
        )
        .expect("build code assist request");

        assert_eq!(
            url,
            "https://cloudcode-pa.googleapis.com/v1internal:streamGenerateContent?alt=sse"
        );
        assert_eq!(model, "gemini-3-flash");
        assert_eq!(body["model"], "gemini-3-flash");
        assert_eq!(body["request"]["contents"][0]["parts"][0]["text"], "ping");
        assert!(body["request"].get("stream").is_none());
    }

    #[test]
    fn build_gemini_code_assist_forward_request_strips_resource_model_prefix() {
        let (_, body, model) = build_gemini_code_assist_forward_request(
            "/v1beta/models/models/gemini-2.5-flash:generateContent",
            &json!({ "contents": [] }),
        )
        .expect("build code assist request");

        assert_eq!(model, "gemini-2.5-flash");
        assert_eq!(body["model"], "gemini-2.5-flash");
    }

    #[test]
    fn build_antigravity_forward_request_wraps_cloud_code_envelope() {
        let (url, body) = build_antigravity_forward_request(
            "/v1beta/models/gemini-3-pro-preview:streamGenerateContent?alt=sse",
            &json!({
                "contents": [{
                    "role": "model",
                    "parts": [
                        { "thought": true, "text": "hidden" },
                        { "text": "visible" }
                    ]
                }],
                "generationConfig": { "maxOutputTokens": 20000 },
                "tools": [
                    {
                        "functionDeclarations": [
                            { "name": "1 bad name!", "parameters": { "type": "object" } }
                        ]
                    }
                ],
                "stream": true
            }),
            "session-1",
        )
        .expect("build antigravity request");

        assert_eq!(
            url,
            "https://daily-cloudcode-pa.googleapis.com/v1internal:streamGenerateContent?alt=sse"
        );
        assert_eq!(body["model"], "gemini-3-pro-preview");
        assert_eq!(body["userAgent"], "antigravity");
        assert_eq!(body["requestType"], "agent");
        assert!(body["requestId"]
            .as_str()
            .is_some_and(|id| id.starts_with("agent/")));
        assert_eq!(body["enabledCreditTypes"][0], "GOOGLE_ONE_AI");
        assert_eq!(body["request"]["sessionId"], "session-1");
        assert_eq!(
            body["request"]["generationConfig"]["maxOutputTokens"],
            ANTIGRAVITY_MAX_OUTPUT_TOKENS
        );
        assert_eq!(
            body["request"]["contents"][0]["parts"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            body["request"]["tools"][0]["functionDeclarations"][0]["name"],
            "_1_bad_name_"
        );
        assert_eq!(
            body["request"]["toolConfig"]["functionCallingConfig"]["mode"],
            "VALIDATED"
        );
        assert!(body["request"].get("stream").is_none());
        assert!(body.get("project").is_none());
    }

    #[test]
    fn build_antigravity_forward_request_normalizes_claude_plugin_aliases() {
        let (_, body) = build_antigravity_forward_request(
            "/v1beta/models/claude-4.6-sonnet-thinking:streamGenerateContent?alt=sse",
            &json!({ "contents": [] }),
            "session-1",
        )
        .expect("build antigravity request");

        assert_eq!(body["model"], "claude-sonnet-4-6-thinking");
    }

    #[test]
    fn gemini_official_forward_base_url_defaults_to_code_assist() {
        use crate::provider::{AuthBinding, AuthBindingSource, Provider, ProviderMeta};

        let provider = Provider {
            id: "gemini-official".to_string(),
            name: "Google Official".to_string(),
            settings_config: json!({}),
            website_url: None,
            category: Some("official".to_string()),
            created_at: None,
            sort_index: None,
            notes: None,
            meta: Some(ProviderMeta {
                auth_binding: Some(AuthBinding {
                    source: AuthBindingSource::ManagedAccount,
                    auth_provider: Some("google_gemini_oauth".to_string()),
                    account_id: Some("acct-1".to_string()),
                }),
                ..Default::default()
            }),
            icon: None,
            icon_color: None,
            in_failover_queue: false,
        };
        let adapter = crate::proxy::providers::GeminiAdapter::new();

        let base_url = extract_forward_base_url(&AppType::Gemini, &provider, &adapter)
            .expect("gemini official should not require configured base_url");

        assert_eq!(base_url, GEMINI_CODE_ASSIST_BASE_URL);
    }

    #[test]
    fn append_query_to_full_url_preserves_existing_query_string() {
        let url = append_query_to_full_url("https://relay.example/api?foo=bar", Some("x-id=1"));

        assert_eq!(url, "https://relay.example/api?foo=bar&x-id=1");
    }

    #[test]
    fn build_gemini_native_url_uses_origin_when_base_ends_with_v1beta() {
        let url = crate::proxy::gemini_url::build_gemini_native_url(
            "https://generativelanguage.googleapis.com/v1beta",
            "/v1beta/models/gemini-2.5-pro:generateContent",
        );

        assert_eq!(
            url,
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-pro:generateContent"
        );
    }

    #[test]
    fn build_gemini_native_url_uses_origin_when_base_already_contains_models_prefix() {
        let url = crate::proxy::gemini_url::build_gemini_native_url(
            "https://generativelanguage.googleapis.com/v1beta/models",
            "/v1beta/models/gemini-2.5-flash:streamGenerateContent?alt=sse",
        );

        assert_eq!(
            url,
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:streamGenerateContent?alt=sse"
        );
    }

    #[test]
    fn resolve_gemini_native_url_keeps_opaque_full_url_as_is() {
        let url = crate::proxy::gemini_url::resolve_gemini_native_url(
            "https://relay.example/custom/generate-content",
            "/v1beta/models/gemini-2.5-flash:streamGenerateContent?alt=sse",
            true,
        );

        assert_eq!(url, "https://relay.example/custom/generate-content?alt=sse");
    }

    #[test]
    fn normalize_codex_oauth_responses_body_adds_required_chatgpt_fields() {
        let body = json!({
            "model": "gpt-5.4",
            "input": [{ "role": "user", "content": "Who are you?" }],
            "stream": false
        });

        let normalized = normalize_codex_oauth_responses_body(body, None);

        assert_eq!(normalized["store"], json!(false));
        assert_eq!(normalized["stream"], json!(true));
        assert_eq!(
            normalized["include"],
            json!(["reasoning.encrypted_content"])
        );
        assert_eq!(normalized["instructions"], json!(""));
        assert_eq!(normalized["tools"], json!([]));
        assert_eq!(normalized["parallel_tool_calls"], json!(false));
    }

    #[test]
    fn normalize_codex_oauth_responses_body_preserves_existing_include_entries() {
        let body = json!({
            "model": "gpt-5.4",
            "input": "ping",
            "include": ["file_search_call.results"],
            "instructions": "Use short answers",
            "tools": [{ "type": "web_search_preview" }],
            "parallel_tool_calls": true
        });

        let normalized = normalize_codex_oauth_responses_body(body, None);
        let include = normalized["include"].as_array().unwrap();

        assert!(include.contains(&json!("file_search_call.results")));
        assert!(include.contains(&json!("reasoning.encrypted_content")));
        assert_eq!(normalized["instructions"], json!("Use short answers"));
        assert_eq!(
            normalized["tools"],
            json!([{ "type": "web_search_preview" }])
        );
        assert_eq!(normalized["parallel_tool_calls"], json!(true));
    }

    #[test]
    fn normalize_codex_oauth_responses_body_strips_unsupported_fields() {
        let body = json!({
            "model": "gpt-5.4",
            "input": "ping",
            "max_output_tokens": 16,
            "temperature": 0.7,
            "top_p": 0.9,
            "frequency_penalty": 0,
            "presence_penalty": 0,
            "logit_bias": {"42": -100},
            "n": 2,
            "stop": ["END"],
            "response_format": {"type": "json_object"},
            "seed": 123,
            "stream_options": {"include_usage": true},
            "user": "user-1"
        });

        let normalized = normalize_codex_oauth_responses_body(body, None);

        for field in [
            "max_output_tokens",
            "temperature",
            "top_p",
            "frequency_penalty",
            "presence_penalty",
            "logit_bias",
            "n",
            "stop",
            "response_format",
            "seed",
            "stream_options",
            "user",
        ] {
            assert!(normalized.get(field).is_none(), "{field}");
        }
    }

    #[test]
    fn normalize_codex_oauth_responses_body_injects_prompt_cache_key() {
        let body = json!({
            "model": "gpt-5.4",
            "input": "ping"
        });

        let normalized = normalize_codex_oauth_responses_body(body, Some("session-123"));

        assert_eq!(normalized["prompt_cache_key"], json!("session-123"));
    }

    #[test]
    fn normalize_codex_oauth_responses_body_preserves_existing_prompt_cache_key() {
        let body = json!({
            "model": "gpt-5.4",
            "input": "ping",
            "prompt_cache_key": "client-key"
        });

        let normalized = normalize_codex_oauth_responses_body(body, Some("session-123"));

        assert_eq!(normalized["prompt_cache_key"], json!("client-key"));
    }

    #[test]
    fn normalize_codex_oauth_responses_body_does_not_inject_without_session() {
        let body = json!({
            "model": "gpt-5.4",
            "input": "ping"
        });

        let normalized = normalize_codex_oauth_responses_body(body, None);

        assert!(normalized.get("prompt_cache_key").is_none());
    }

    #[test]
    fn codex_oauth_upstream_session_id_strips_internal_prefix() {
        assert_eq!(
            codex_oauth_upstream_session_id("codex_736fc774-8efb-4f67-b8ab-771fc2afe205")
                .as_deref(),
            Some("736fc774-8efb-4f67-b8ab-771fc2afe205")
        );
        assert_eq!(
            codex_oauth_upstream_session_id("  session-123  ").as_deref(),
            Some("session-123")
        );
        assert_eq!(codex_oauth_upstream_session_id("codex_"), None);
        assert_eq!(codex_oauth_upstream_session_id(""), None);
    }

    #[test]
    fn share_urls_copy_bearer_token_into_x_api_key() {
        let mut auth_headers = vec![(
            http::HeaderName::from_static("authorization"),
            HeaderValue::from_static("Bearer share-token-123"),
        )];

        maybe_add_share_auth_header(&mut auth_headers, "https://alpha.jptokenswitch.cc/v1");

        assert_eq!(
            find_header_value(&auth_headers, "authorization"),
            Some("Bearer share-token-123")
        );
        assert_eq!(
            find_header_value(&auth_headers, "x-api-key"),
            Some("share-token-123")
        );
    }

    #[test]
    fn share_urls_do_not_duplicate_existing_x_api_key() {
        let mut auth_headers = vec![
            (
                http::HeaderName::from_static("authorization"),
                HeaderValue::from_static("Bearer share-token-123"),
            ),
            (
                http::HeaderName::from_static("x-api-key"),
                HeaderValue::from_static("share-token-123"),
            ),
        ];

        maybe_add_share_auth_header(&mut auth_headers, "https://alpha.jptokenswitch.cc/v1");

        let x_api_key_count = auth_headers
            .iter()
            .filter(|(name, _)| name.as_str().eq_ignore_ascii_case("x-api-key"))
            .count();
        assert_eq!(x_api_key_count, 1);
    }

    #[test]
    fn force_identity_for_stream_flag_requests() {
        let headers = HeaderMap::new();

        assert!(should_force_identity_encoding(
            "/v1/responses",
            &json!({ "stream": true }),
            &headers
        ));
    }

    #[test]
    fn force_identity_for_gemini_stream_endpoints() {
        let headers = HeaderMap::new();

        assert!(should_force_identity_encoding(
            "/v1beta/models/gemini-2.5-pro:streamGenerateContent?alt=sse",
            &json!({ "model": "gemini-2.5-pro" }),
            &headers
        ));
    }

    #[test]
    fn streaming_request_detects_gemini_sse_without_body_stream_flag() {
        let headers = HeaderMap::new();

        assert!(is_streaming_request(
            "/v1beta/models/gemini-2.5-pro:streamGenerateContent?alt=sse",
            &json!({ "model": "gemini-2.5-pro" }),
            &headers
        ));
    }

    #[test]
    fn force_identity_for_sse_accept_header() {
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));

        assert!(should_force_identity_encoding(
            "/v1/responses",
            &json!({ "model": "gpt-5" }),
            &headers
        ));
    }

    #[test]
    fn non_streaming_requests_allow_automatic_compression() {
        let headers = HeaderMap::new();

        assert!(!should_force_identity_encoding(
            "/v1/responses",
            &json!({ "model": "gpt-5" }),
            &headers
        ));
    }

    // ==================== Copilot 动态 endpoint 路由相关测试 ====================

    /// 验证 is_copilot 检测逻辑：通过 provider_type 判断
    #[test]
    fn copilot_detection_via_provider_type() {
        use crate::provider::{Provider, ProviderMeta};

        let provider = Provider {
            id: "test".to_string(),
            name: "Test Copilot".to_string(),
            settings_config: serde_json::json!({}),
            website_url: None,
            category: None,
            created_at: None,
            sort_index: None,
            notes: None,
            meta: Some(ProviderMeta {
                provider_type: Some("github_copilot".to_string()),
                ..Default::default()
            }),
            icon: None,
            icon_color: None,
            in_failover_queue: false,
        };

        let is_copilot = provider
            .meta
            .as_ref()
            .and_then(|m| m.provider_type.as_deref())
            == Some("github_copilot");

        assert!(is_copilot, "应该通过 provider_type 检测为 Copilot");
    }

    /// 验证 is_copilot 检测逻辑：通过 base_url 判断
    #[test]
    fn copilot_detection_via_base_url() {
        let base_url = "https://api.githubcopilot.com";
        let is_copilot = base_url.contains("githubcopilot.com");
        assert!(is_copilot, "应该通过 base_url 检测为 Copilot");

        let non_copilot_url = "https://api.anthropic.com";
        let is_not_copilot = non_copilot_url.contains("githubcopilot.com");
        assert!(!is_not_copilot, "非 Copilot URL 不应被检测为 Copilot");
    }

    /// 验证企业版 endpoint（不包含 githubcopilot.com）场景下 is_copilot 仍然正确
    #[test]
    fn copilot_detection_for_enterprise_endpoint() {
        use crate::provider::{Provider, ProviderMeta};

        // 企业版场景：provider_type 是 github_copilot，但 base_url 可能是企业内部域名
        let provider = Provider {
            id: "enterprise".to_string(),
            name: "Enterprise Copilot".to_string(),
            settings_config: serde_json::json!({}),
            website_url: None,
            category: None,
            created_at: None,
            sort_index: None,
            notes: None,
            meta: Some(ProviderMeta {
                provider_type: Some("github_copilot".to_string()),
                ..Default::default()
            }),
            icon: None,
            icon_color: None,
            in_failover_queue: false,
        };

        let enterprise_base_url = "https://copilot-api.corp.example.com";

        // is_copilot 应该通过 provider_type 检测成功，即使 base_url 不包含 githubcopilot.com
        let is_copilot = provider
            .meta
            .as_ref()
            .and_then(|m| m.provider_type.as_deref())
            == Some("github_copilot")
            || enterprise_base_url.contains("githubcopilot.com");

        assert!(
            is_copilot,
            "企业版 Copilot 应该通过 provider_type 被正确检测"
        );
    }

    /// 验证动态 endpoint 替换条件
    #[test]
    fn dynamic_endpoint_replacement_conditions() {
        // 条件：is_copilot && !is_full_url
        let test_cases = [
            (true, false, true, "Copilot + 非 full_url 应该替换"),
            (true, true, false, "Copilot + full_url 不应替换"),
            (false, false, false, "非 Copilot 不应替换"),
            (false, true, false, "非 Copilot + full_url 不应替换"),
        ];

        for (is_copilot, is_full_url, should_replace, desc) in test_cases {
            let will_replace = is_copilot && !is_full_url;
            assert_eq!(will_replace, should_replace, "{desc}");
        }
    }

    // ===== P3: forwarder 层 media 开关回归测试 =====
    // 验证 gate 在 forwarder 这一层的"接线"，而非 media_sanitizer 纯函数本身。

    fn forwarder_with_rectifier(config: RectifierConfig) -> RequestForwarder {
        let mut fwd = test_forwarder(Duration::from_secs(1), Duration::from_secs(1));
        fwd.rectifier_config = config;
        fwd
    }

    fn provider_with_settings(settings_config: Value) -> Provider {
        let mut p = test_provider_with_type(Some("anthropic"));
        p.settings_config = settings_config;
        p
    }

    fn body_with_image(model: &str) -> Value {
        json!({
            "model": model,
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "abc" } }
                ]
            }]
        })
    }

    fn image_unsupported_error() -> ProxyError {
        ProxyError::UpstreamError {
            status: 400,
            body: Some(
                r#"{"error":{"message":"This model does not support image input"}}"#.to_string(),
            ),
        }
    }
    #[test]
    fn prevention_replaces_when_all_switches_on_and_model_in_heuristic_list() {
        let fwd = forwarder_with_rectifier(RectifierConfig::default());
        let provider = provider_with_settings(json!({}));
        let mut body = body_with_image("deepseek-v4-pro");

        let replaced = fwd.apply_media_prevention(&mut body, &provider);

        assert_eq!(replaced, 1, "默认全开 + 名单内模型应预替换");
        assert_eq!(body["messages"][0]["content"][0]["type"], "text");
    }

    #[test]
    fn prevention_skipped_when_media_fallback_off() {
        // 关闭 request_media_fallback：即使名单命中也不预替换。
        let fwd = forwarder_with_rectifier(RectifierConfig {
            request_media_fallback: false,
            ..RectifierConfig::default()
        });
        let provider = provider_with_settings(json!({}));
        let mut body = body_with_image("deepseek-v4-pro");

        let replaced = fwd.apply_media_prevention(&mut body, &provider);

        assert_eq!(replaced, 0);
        assert_eq!(body["messages"][0]["content"][0]["type"], "image");
    }

    #[test]
    fn prevention_skipped_when_master_switch_off() {
        let fwd = forwarder_with_rectifier(RectifierConfig {
            enabled: false,
            ..RectifierConfig::default()
        });
        let provider = provider_with_settings(json!({}));
        let mut body = body_with_image("deepseek-v4-pro");

        assert_eq!(fwd.apply_media_prevention(&mut body, &provider), 0);
        assert_eq!(body["messages"][0]["content"][0]["type"], "image");
    }

    #[test]
    fn prevention_heuristic_off_skips_list_but_keeps_explicit_text_only() {
        // 关闭 request_media_heuristic：名单预测失效，但显式声明 text-only 仍预替换。
        let fwd = forwarder_with_rectifier(RectifierConfig {
            request_media_heuristic: false,
            ..RectifierConfig::default()
        });

        // (a) 名单内模型、无显式声明 → 不再预替换
        let bare_provider = provider_with_settings(json!({}));
        let mut list_body = body_with_image("deepseek-v4-pro");
        assert_eq!(
            fwd.apply_media_prevention(&mut list_body, &bare_provider),
            0,
            "heuristic 关闭后名单模型不应被预替换"
        );
        assert_eq!(list_body["messages"][0]["content"][0]["type"], "image");

        // (b) 显式声明 text-only → 仍预替换（声明驱动，不受 heuristic 开关影响）
        let declared_provider = provider_with_settings(json!({
            "models": [ { "id": "some-text-model", "input": ["text"] } ]
        }));
        let mut declared_body = body_with_image("some-text-model");
        assert_eq!(
            fwd.apply_media_prevention(&mut declared_body, &declared_provider),
            1,
            "显式 text-only 即使关闭 heuristic 也应预替换"
        );
        assert_eq!(declared_body["messages"][0]["content"][0]["type"], "text");
    }

    #[test]
    fn reactive_triggers_when_all_switches_on() {
        let fwd = forwarder_with_rectifier(RectifierConfig::default());
        let body = body_with_image("any-model");
        assert!(fwd.media_retry_should_trigger("Claude", false, &body, &image_unsupported_error()));
    }

    #[test]
    fn reactive_skipped_when_media_fallback_off() {
        // 关闭 request_media_fallback：上游报图片错误也不触发兜底重试。
        let fwd = forwarder_with_rectifier(RectifierConfig {
            request_media_fallback: false,
            ..RectifierConfig::default()
        });
        let body = body_with_image("any-model");
        assert!(!fwd.media_retry_should_trigger(
            "Claude",
            false,
            &body,
            &image_unsupported_error()
        ));
    }

    #[test]
    fn reactive_skipped_when_master_switch_off() {
        let fwd = forwarder_with_rectifier(RectifierConfig {
            enabled: false,
            ..RectifierConfig::default()
        });
        let body = body_with_image("any-model");
        assert!(!fwd.media_retry_should_trigger(
            "Claude",
            false,
            &body,
            &image_unsupported_error()
        ));
    }

    #[test]
    fn reactive_unaffected_by_heuristic_switch() {
        // 关闭 request_media_heuristic 不影响反应式兜底——它是上游实测错误后的恢复，不是预测。
        let fwd = forwarder_with_rectifier(RectifierConfig {
            request_media_heuristic: false,
            ..RectifierConfig::default()
        });
        let body = body_with_image("any-model");
        assert!(fwd.media_retry_should_trigger("Claude", false, &body, &image_unsupported_error()));
    }
}
