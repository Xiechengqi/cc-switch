//! 请求上下文模块
//!
//! 提供请求生命周期的上下文管理，封装通用初始化逻辑

use crate::app_config::AppType;
use crate::provider::Provider;
use crate::proxy::{
    extract_session_id,
    forwarder::RequestForwarder,
    server::ProxyState,
    share_guard::{check_share_token, ShareGuardResult},
    types::{AppProxyConfig, CopilotOptimizerConfig, OptimizerConfig, RectifierConfig},
    ProxyError,
};
use axum::http::HeaderMap;
use std::time::Instant;

/// 流式超时配置
#[derive(Debug, Clone, Copy)]
pub struct StreamingTimeoutConfig {
    /// 首字节超时（秒），0 表示禁用
    pub first_byte_timeout: u64,
    /// 静默期超时（秒），0 表示禁用
    pub idle_timeout: u64,
}

/// 请求上下文
///
/// 贯穿整个请求生命周期，包含：
/// - 计时信息
/// - 应用级代理配置（per-app）
/// - 选中的 Provider 列表（用于故障转移）
/// - 请求模型名称
/// - 日志标签
/// - Session ID（用于日志关联）
pub struct RequestContext {
    /// 请求开始时间
    pub start_time: Instant,
    /// 应用级代理配置（per-app，包含重试次数和超时配置）
    pub app_config: AppProxyConfig,
    /// 选中的 Provider（故障转移链的第一个）
    pub provider: Provider,
    /// 完整的 Provider 列表（用于故障转移）
    providers: Vec<Provider>,
    /// 请求开始时的"当前供应商"（用于判断是否需要同步 UI/托盘）
    ///
    /// 这里使用本地 settings 的设备级 current provider。
    /// 代理模式下如果实际使用的 provider 与此不一致，会触发切换以确保 UI 始终准确。
    pub current_provider_id: String,
    /// 请求中的模型名称
    pub request_model: String,
    /// 请求体是否明确要求流式响应。
    ///
    /// 部分官方/反代端点会返回 SSE 文本但遗漏 `content-type:
    /// text/event-stream`，因此响应处理不能只依赖响应头判断流式。
    pub request_is_streaming: bool,
    /// 日志标签（如 "Claude"、"Codex"、"Gemini"）
    pub tag: &'static str,
    /// 应用类型字符串（如 "claude"、"codex"、"gemini"）
    pub app_type_str: &'static str,
    /// 应用类型（预留，目前通过 app_type_str 使用）
    #[allow(dead_code)]
    pub app_type: AppType,
    /// Session ID（从客户端请求提取或新生成）
    pub session_id: String,
    /// Session ID 是否由客户端提供。生成的 UUID 不能作为上游缓存 key，否则每个请求都会换 key。
    pub session_client_provided: bool,
    /// 整流器配置
    pub rectifier_config: RectifierConfig,
    /// 优化器配置
    pub optimizer_config: OptimizerConfig,
    /// Copilot 优化器配置
    pub copilot_optimizer_config: CopilotOptimizerConfig,
    /// 共享 token 请求：如果本次请求由 X-Share-Token 发起，记录 share_id 用于用量回写
    pub share_id: Option<String>,
    /// 共享 token 请求对应的 share 名称，用于请求明细落库
    pub share_name: Option<String>,
    /// 由 cc-switch-router 透传的请求 ID，用于让 live ticker 与 share request log
    /// 共享同一个 request identity。
    pub incoming_request_id: Option<String>,
}

impl RequestContext {
    /// 创建请求上下文
    ///
    /// # Arguments
    /// * `state` - 代理服务器状态
    /// * `body` - 请求体 JSON
    /// * `headers` - 请求头（用于提取 Session ID）
    /// * `app_type` - 应用类型
    /// * `tag` - 日志标签
    /// * `app_type_str` - 应用类型字符串
    ///
    /// # Errors
    /// 返回 `ProxyError` 如果 Provider 选择失败
    pub async fn new(
        state: &ProxyState,
        body: &serde_json::Value,
        headers: &HeaderMap,
        app_type: AppType,
        tag: &'static str,
        app_type_str: &'static str,
    ) -> Result<Self, ProxyError> {
        let start_time = Instant::now();

        // 从数据库读取应用级代理配置（per-app）
        let app_config = state
            .db
            .get_proxy_config_for_app(app_type_str)
            .await
            .map_err(|e| ProxyError::DatabaseError(e.to_string()))?;

        // 从数据库读取整流器配置
        let rectifier_config = state.db.get_rectifier_config().unwrap_or_default();
        let optimizer_config = state.db.get_optimizer_config().unwrap_or_default();
        let copilot_optimizer_config = state.db.get_copilot_optimizer_config().unwrap_or_default();

        let current_provider_id =
            crate::settings::get_current_provider(&app_type).unwrap_or_default();

        // 从请求体提取模型名称
        let request_model = body
            .get("model")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown")
            .to_string();
        let request_is_streaming = body
            .get("stream")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);

        // 提取 Session ID
        let session_result = extract_session_id(headers, body, app_type_str);
        let session_id = session_result.session_id.clone();

        log::debug!(
            "[{}] Session ID: {} (from {:?}, client_provided: {})",
            tag,
            session_id,
            session_result.source,
            session_result.client_provided
        );

        // 使用共享的 ProviderRouter 选择 Provider（熔断器状态跨请求保持）
        // 注意：只在这里调用一次，结果传递给 forwarder，避免重复消耗 HalfOpen 名额
        let providers = state
            .provider_router
            .select_providers(app_type_str)
            .await
            .map_err(|e| match e {
                crate::error::AppError::AllProvidersCircuitOpen => {
                    ProxyError::AllProvidersCircuitOpen
                }
                crate::error::AppError::NoProvidersConfigured => ProxyError::NoProvidersConfigured,
                _ => ProxyError::DatabaseError(e.to_string()),
            })?;

        let provider = providers
            .first()
            .cloned()
            .ok_or(ProxyError::NoAvailableProvider)?;

        log::debug!(
            "[{}] Provider: {}, model: {}, failover chain: {} providers, session: {}",
            tag,
            provider.name,
            request_model,
            providers.len(),
            session_id
        );

        let incoming_request_id = headers
            .get("x-cc-switch-request-id")
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);

        let mut this = Self {
            start_time,
            app_config,
            provider,
            providers,
            current_provider_id,
            request_model,
            request_is_streaming,
            tag,
            app_type_str,
            app_type,
            session_id,
            session_client_provided: session_result.client_provided,
            rectifier_config,
            optimizer_config,
            copilot_optimizer_config,
            share_id: None,
            share_name: None,
            incoming_request_id,
        };

        // 共享 Token 拦截：若请求带 X-Share-Token，则校验设备级分享并记录 share_id
        this.try_apply_share(state, headers)?;

        Ok(this)
    }

    /// 验证 X-Share-Token 并记录 share_id。
    ///
    /// - 无 header：保持原始的 provider 路由（正常本地代理）。
    /// - header 存在且有效：
    ///   * 这是设备级分享，直接沿用本地代理当前的 app/provider 路由。
    ///   * 仅记录 share_id，用于额度统计和审计。
    /// - header 存在但无效/过期/耗尽：返回 `ProxyError::AuthError`。
    fn try_apply_share(
        &mut self,
        state: &ProxyState,
        headers: &HeaderMap,
    ) -> Result<(), ProxyError> {
        match check_share_token(&state.db, headers) {
            ShareGuardResult::NotShareRequest => Ok(()),
            ShareGuardResult::Rejected(_code, msg) => Err(ProxyError::AuthError(msg)),
            ShareGuardResult::Valid(share) => {
                self.share_id = Some(share.id.clone());
                self.share_name = Some(share.name.clone());
                crate::proxy::share_guard::record_share_access(&state.db, &share.id);
                log::info!(
                    "[{}] 共享请求 share_id={} app={} provider={}",
                    self.tag,
                    share.id,
                    self.app_type_str,
                    self.provider.name
                );
                Ok(())
            }
        }
    }

    /// 从 URI 提取模型名称（Gemini 专用）
    ///
    /// Gemini API 的模型名称在 URI 中，格式如：
    /// `/v1beta/models/gemini-pro:generateContent`
    pub fn with_model_from_uri(mut self, uri: &axum::http::Uri) -> Self {
        let endpoint = uri
            .path_and_query()
            .map(|pq| pq.as_str())
            .unwrap_or(uri.path());

        self.request_model = extract_gemini_model_from_endpoint(endpoint)
            .unwrap_or("unknown")
            .to_string();

        self
    }

    /// 创建 RequestForwarder
    ///
    /// 使用共享的 ProviderRouter，确保熔断器状态跨请求保持
    ///
    /// 配置生效规则：
    /// - 故障转移开启：超时配置正常生效（0 表示禁用超时）
    /// - 故障转移关闭：超时配置不生效（全部传入 0）
    pub fn create_forwarder(&self, state: &ProxyState) -> RequestForwarder {
        let (non_streaming_timeout, first_byte_timeout, idle_timeout) =
            if self.app_config.auto_failover_enabled {
                // 故障转移开启：使用配置的值（0 = 禁用超时）
                (
                    self.app_config.non_streaming_timeout as u64,
                    self.app_config.streaming_first_byte_timeout as u64,
                    self.app_config.streaming_idle_timeout as u64,
                )
            } else {
                // 故障转移关闭：不启用超时配置
                log::debug!(
                    "[{}] Failover disabled, timeout configs are bypassed",
                    self.tag
                );
                (0, 0, 0)
            };

        RequestForwarder::new(
            state.provider_router.clone(),
            non_streaming_timeout,
            state.status.clone(),
            state.current_providers.clone(),
            state.gemini_shadow.clone(),
            state.failover_manager.clone(),
            state.app_handle.clone(),
            self.current_provider_id.clone(),
            self.session_id.clone(),
            self.session_client_provided,
            first_byte_timeout,
            idle_timeout,
            self.rectifier_config.clone(),
            self.optimizer_config.clone(),
            self.copilot_optimizer_config.clone(),
        )
    }

    /// 获取 Provider 列表（用于故障转移）
    ///
    /// 返回在创建上下文时已选择的 providers，避免重复调用 select_providers()
    pub fn get_providers(&self) -> Vec<Provider> {
        self.providers.clone()
    }

    /// 计算请求延迟（毫秒）
    #[inline]
    pub fn latency_ms(&self) -> u64 {
        self.start_time.elapsed().as_millis() as u64
    }

    /// 获取流式超时配置
    ///
    /// 配置生效规则：
    /// - 故障转移开启：返回配置的值（0 表示禁用超时检查）
    /// - 故障转移关闭：返回 0（禁用超时检查）
    #[inline]
    pub fn streaming_timeout_config(&self) -> StreamingTimeoutConfig {
        if self.app_config.auto_failover_enabled {
            // 故障转移开启：使用配置的值（0 = 禁用超时）
            StreamingTimeoutConfig {
                first_byte_timeout: self.app_config.streaming_first_byte_timeout as u64,
                idle_timeout: self.app_config.streaming_idle_timeout as u64,
            }
        } else {
            // 故障转移关闭：禁用流式超时检查
            StreamingTimeoutConfig {
                first_byte_timeout: 0,
                idle_timeout: 0,
            }
        }
    }
}

fn extract_gemini_model_from_endpoint(endpoint: &str) -> Option<&str> {
    let path = endpoint.split('?').next().unwrap_or(endpoint);
    let mut segments = path.split('/').filter(|segment| !segment.is_empty());

    while let Some(segment) = segments.next() {
        if segment == "models" {
            return segments
                .next()
                .and_then(|model| model.split(':').next())
                .filter(|model| !model.is_empty());
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::extract_gemini_model_from_endpoint;

    #[test]
    fn extracts_gemini_model_from_native_stream_path() {
        assert_eq!(
            extract_gemini_model_from_endpoint(
                "/v1beta/models/gemini-2.5-flash:streamGenerateContent?alt=sse"
            ),
            Some("gemini-2.5-flash")
        );
    }

    #[test]
    fn extracts_gemini_model_from_prefixed_native_path() {
        assert_eq!(
            extract_gemini_model_from_endpoint(
                "/gemini/v1beta/models/gemini-2.5-flash-lite:generateContent"
            ),
            Some("gemini-2.5-flash-lite")
        );
    }

    #[test]
    fn returns_none_when_gemini_model_is_absent() {
        assert_eq!(extract_gemini_model_from_endpoint("/v1beta/models"), None);
        assert_eq!(extract_gemini_model_from_endpoint("/health"), None);
    }
}
