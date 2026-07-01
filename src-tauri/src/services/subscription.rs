//! 官方订阅额度查询服务
//!
//! 读取 CLI 工具的已有 OAuth 凭据，查询官方订阅额度。
//! 第一层：仅读取凭据，不实现登录/刷新。

use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

use std::collections::HashMap;

use crate::config;

// ── 数据类型 ──────────────────────────────────────────────

/// 凭据状态
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialStatus {
    Valid,
    Expired,
    NotFound,
    ParseError,
}

/// 单个限速窗口（如 5小时会话、7天周期）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaTier {
    /// 窗口标识：five_hour, seven_day, seven_day_opus, seven_day_sonnet 等
    pub name: String,
    /// 使用百分比 0–100
    pub utilization: f64,
    /// ISO 8601 重置时间
    pub resets_at: Option<String>,
    /// 原始已用量（Kiro Credits 等非时间窗口额度使用）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used: Option<f64>,
    /// 原始额度上限（Kiro Credits 等非时间窗口额度使用）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<f64>,
    /// 原始额度单位
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// ZenMux: 已用额度（USD）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub used_value_usd: Option<f64>,
    /// ZenMux: 窗口上限（USD）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_value_usd: Option<f64>,
}

/// 超额使用信息
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtraUsage {
    pub is_enabled: bool,
    pub monthly_limit: Option<f64>,
    pub used_credits: Option<f64>,
    pub utilization: Option<f64>,
    pub currency: Option<String>,
}

/// 订阅到期时间的语义来源。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionExpiresKind {
    /// 真实订阅到期时间。
    Subscription,
    /// 账期结束时间，不一定代表订阅终止。
    BillingPeriod,
    /// 用量窗口重置时间，不应标记为订阅到期。
    QuotaPeriod,
    /// 当前供应商暂未暴露明确语义。
    Unknown,
}

/// 账号级订阅摘要。不同于 `QuotaTier::resets_at`，这里描述订阅本身。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_kind: Option<SubscriptionExpiresKind>,
}

/// 额度查询失败的结构化分类
///
/// 用于前端区分"真正过期 vs 临时限流 vs 网络错误"，从而决定是重试 / 退避 / 登录引导。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum QuotaFailure {
    /// HTTP 401/403：凭据真的失效，需要重新登录
    Unauthorized,
    /// HTTP 429：被上游限流，`retry_after_ms` 可能来自 Retry-After 头
    RateLimited { retry_after_ms: i64 },
    /// HTTP 5xx 或其它非 2xx：上游暂时性问题
    Upstream { status: u16 },
    /// 网络层错误：timeout / connect / DNS
    Network { detail: String },
    /// 响应体解析失败
    Parse,
}

impl QuotaFailure {
    fn from_reqwest(err: &reqwest::Error) -> Self {
        let kind = if err.is_timeout() {
            "timeout"
        } else if err.is_connect() {
            "connect"
        } else if err.is_decode() {
            "decode"
        } else {
            "other"
        };
        QuotaFailure::Network {
            detail: kind.to_string(),
        }
    }
}

/// 解析 HTTP `Retry-After` 头，支持两种格式：
/// - 秒整数（例如 `120`）
/// - HTTP-date（例如 `Fri, 31 Dec 2026 23:59:59 GMT`）
///
/// 返回距离现在的毫秒数，若格式不识别返回 None。
fn parse_retry_after_ms(value: &str) -> Option<i64> {
    let trimmed = value.trim();
    if let Ok(secs) = trimmed.parse::<i64>() {
        if secs >= 0 {
            return Some(secs.saturating_mul(1000));
        }
    }
    if let Ok(date) = chrono::DateTime::parse_from_rfc2822(trimmed) {
        let now = chrono::Utc::now();
        let diff = date.with_timezone(&chrono::Utc) - now;
        let ms = diff.num_milliseconds();
        if ms > 0 {
            return Some(ms);
        }
        return Some(0);
    }
    None
}

fn classify_http_failure(status: reqwest::StatusCode, retry_after: Option<&str>) -> QuotaFailure {
    match status.as_u16() {
        401 | 403 => QuotaFailure::Unauthorized,
        429 => QuotaFailure::RateLimited {
            retry_after_ms: retry_after.and_then(parse_retry_after_ms).unwrap_or(60_000),
        },
        s if (500..600).contains(&s) => QuotaFailure::Upstream { status: s },
        s => QuotaFailure::Upstream { status: s },
    }
}

fn credential_status_for_failure(failure: &QuotaFailure) -> CredentialStatus {
    match failure {
        QuotaFailure::Unauthorized => CredentialStatus::Expired,
        _ => CredentialStatus::Valid,
    }
}

/// 订阅额度查询结果
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionQuota {
    pub tool: String,
    pub credential_status: CredentialStatus,
    pub credential_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription: Option<SubscriptionInfo>,
    pub success: bool,
    pub tiers: Vec<QuotaTier>,
    pub extra_usage: Option<ExtraUsage>,
    pub error: Option<String>,
    pub queried_at: Option<i64>,
    /// 结构化失败分类；`success == true` 时为 None
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub failure: Option<QuotaFailure>,
}

impl SubscriptionQuota {
    pub(crate) fn not_found(tool: &str) -> Self {
        Self {
            tool: tool.to_string(),
            credential_status: CredentialStatus::NotFound,
            credential_message: None,
            subscription: None,
            success: false,
            tiers: vec![],
            extra_usage: None,
            error: None,
            queried_at: None,
            failure: None,
        }
    }

    pub(crate) fn error(tool: &str, status: CredentialStatus, message: String) -> Self {
        Self {
            tool: tool.to_string(),
            credential_status: status,
            credential_message: Some(message.clone()),
            subscription: None,
            success: false,
            tiers: vec![],
            extra_usage: None,
            error: Some(message),
            queried_at: Some(now_millis()),
            failure: None,
        }
    }

    pub(crate) fn failure(tool: &str, failure: QuotaFailure, message: String) -> Self {
        let status = credential_status_for_failure(&failure);
        Self {
            tool: tool.to_string(),
            credential_status: status,
            credential_message: Some(message.clone()),
            subscription: None,
            success: false,
            tiers: vec![],
            extra_usage: None,
            error: Some(message),
            queried_at: Some(now_millis()),
            failure: Some(failure),
        }
    }
}

// ── Claude 凭据读取 ──────────────────────────────────────

/// Claude OAuth 凭据文件中的嵌套结构
#[derive(Deserialize)]
struct ClaudeOAuthEntry {
    #[serde(rename = "accessToken")]
    access_token: Option<String>,
    #[serde(rename = "expiresAt")]
    expires_at: Option<serde_json::Value>,
}

/// 读取 Claude OAuth 凭据
///
/// 按优先级尝试以下来源：
/// 1. macOS Keychain (service: "Claude Code-credentials")
/// 2. 凭据文件 ~/.claude/.credentials.json
///
/// JSON 格式（两种 key 都兼容）：
/// {"claudeAiOauth": {"accessToken": "...", "expiresAt": ...}}
/// {"claude.ai_oauth": {"accessToken": "...", "expiresAt": ...}}
fn read_claude_credentials() -> (Option<String>, CredentialStatus, Option<String>) {
    // 来源 1: macOS Keychain
    #[cfg(target_os = "macos")]
    {
        if let Some(result) = read_claude_credentials_from_keychain() {
            return result;
        }
    }

    // 来源 2: 凭据文件
    read_claude_credentials_from_file()
}

/// 从 macOS Keychain 读取 Claude 凭据
#[cfg(target_os = "macos")]
fn read_claude_credentials_from_keychain(
) -> Option<(Option<String>, CredentialStatus, Option<String>)> {
    let output = std::process::Command::new("security")
        .args([
            "find-generic-password",
            "-s",
            "Claude Code-credentials",
            "-w",
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None; // Keychain 中无此条目，回退到文件
    }

    let json_str = String::from_utf8(output.stdout).ok()?;
    let json_str = json_str.trim();
    if json_str.is_empty() {
        return None;
    }

    Some(parse_claude_credentials_json(json_str))
}

/// 从文件读取 Claude 凭据
fn read_claude_credentials_from_file() -> (Option<String>, CredentialStatus, Option<String>) {
    let cred_path = config::get_claude_config_dir().join(".credentials.json");

    if !cred_path.exists() {
        return (None, CredentialStatus::NotFound, None);
    }

    let content = match std::fs::read_to_string(&cred_path) {
        Ok(c) => c,
        Err(e) => {
            return (
                None,
                CredentialStatus::ParseError,
                Some(format!("Failed to read credentials file: {e}")),
            );
        }
    };

    parse_claude_credentials_json(&content)
}

/// 解析 Claude 凭据 JSON（Keychain 和文件共用）
fn parse_claude_credentials_json(
    content: &str,
) -> (Option<String>, CredentialStatus, Option<String>) {
    let parsed: serde_json::Value = match serde_json::from_str(content) {
        Ok(v) => v,
        Err(e) => {
            return (
                None,
                CredentialStatus::ParseError,
                Some(format!("Failed to parse credentials JSON: {e}")),
            );
        }
    };

    // 兼容两种 key 名
    let entry_value = parsed
        .get("claudeAiOauth")
        .or_else(|| parsed.get("claude.ai_oauth"));

    let entry_value = match entry_value {
        Some(v) => v,
        None => {
            return (
                None,
                CredentialStatus::ParseError,
                Some("No OAuth entry found in credentials".to_string()),
            );
        }
    };

    let entry: ClaudeOAuthEntry = match serde_json::from_value(entry_value.clone()) {
        Ok(e) => e,
        Err(e) => {
            return (
                None,
                CredentialStatus::ParseError,
                Some(format!("Failed to parse OAuth entry: {e}")),
            );
        }
    };

    let access_token = match entry.access_token {
        Some(t) if !t.is_empty() => t,
        _ => {
            return (
                None,
                CredentialStatus::ParseError,
                Some("accessToken is empty or missing".to_string()),
            );
        }
    };

    // 检查 token 是否过期
    if let Some(expires_at) = entry.expires_at {
        if is_token_expired(&expires_at) {
            return (
                Some(access_token),
                CredentialStatus::Expired,
                Some("OAuth token has expired".to_string()),
            );
        }
    }

    (Some(access_token), CredentialStatus::Valid, None)
}

/// 判断 token 是否过期，兼容 Unix 时间戳（秒/毫秒）和 ISO 字符串
fn is_token_expired(expires_at: &serde_json::Value) -> bool {
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    match expires_at {
        serde_json::Value::Number(n) => {
            if let Some(ts) = n.as_u64() {
                // 区分秒和毫秒（毫秒级时间戳大于 1e12）
                let ts_secs = if ts > 1_000_000_000_000 {
                    ts / 1000
                } else {
                    ts
                };
                ts_secs < now_secs
            } else {
                false
            }
        }
        serde_json::Value::String(s) => {
            // 尝试解析 ISO 8601 格式
            if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
                (dt.timestamp() as u64) < now_secs
            } else if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f")
            {
                (dt.and_utc().timestamp() as u64) < now_secs
            } else {
                false // 无法解析时不视为过期
            }
        }
        _ => false,
    }
}

// ── Claude API 查询 ──────────────────────────────────────

/// Claude OAuth 用量 API 响应中的单个窗口
#[derive(Deserialize)]
struct ApiUsageWindow {
    utilization: Option<f64>,
    resets_at: Option<String>,
}

/// Claude OAuth 用量 API 响应中的超额用量
#[derive(Deserialize)]
struct ApiExtraUsage {
    is_enabled: Option<bool>,
    monthly_limit: Option<f64>,
    used_credits: Option<f64>,
    utilization: Option<f64>,
    currency: Option<String>,
}

/// 已知的 Claude 用量窗口名称。`QuotaTier::name` 会是其中之一。
pub const TIER_FIVE_HOUR: &str = "five_hour";
pub const TIER_SEVEN_DAY: &str = "seven_day";
pub const TIER_THIRTY_DAY: &str = "30_day";
pub const TIER_SEVEN_DAY_OPUS: &str = "seven_day_opus";
pub const TIER_SEVEN_DAY_SONNET: &str = "seven_day_sonnet";

/// Coding Plan（Kimi / MiniMax）的周窗口 tier 名。与 `coding_plan::query_*`
/// 写入、tray 渲染、commands::provider 扁平化三处共用同一标识。
pub const TIER_WEEKLY_LIMIT: &str = "weekly_limit";

/// 月窗口 tier 名。火山方舟 Agent Plan / Coding Plan 有 5h / 周 / 月 三个展示
/// 窗口（Kimi / MiniMax 只有 5h + 周），月窗口共用此标识；前端 `TIER_I18N_KEYS`
/// 映射到 `subscription.monthly`。
pub const TIER_MONTHLY: &str = "monthly";

/// Gemini 用量分组名称（按模型而非时间窗口）。`classify_gemini_model` 输出。
pub const TIER_GEMINI_PRO: &str = "gemini_pro";
pub const TIER_GEMINI_FLASH: &str = "gemini_flash";
pub const TIER_GEMINI_FLASH_LITE: &str = "gemini_flash_lite";

const KNOWN_TIERS: &[&str] = &[
    TIER_FIVE_HOUR,
    TIER_SEVEN_DAY,
    TIER_SEVEN_DAY_OPUS,
    "seven_day_omelette",
    TIER_SEVEN_DAY_SONNET,
];

fn normalize_claude_tier_name(name: &str) -> &str {
    match name {
        // Anthropic OAuth usage endpoint has been seen returning this variant.
        "seven_day_omelette" => "seven_day_opus",
        _ => name,
    }
}

/// 使用给定的 access_token 查询 Claude 订阅额度（公开接口，供 ClaudeOAuthManager 使用）
pub async fn query_claude_quota_with_token(
    access_token: &str,
    tool_name: &str,
) -> SubscriptionQuota {
    let mut result = query_claude_quota(access_token).await;
    result.tool = tool_name.to_string();
    result
}

/// 使用给定的 access_token 查询 Gemini 订阅额度（公开接口，供 GeminiOAuthManager 使用）
pub async fn query_gemini_quota_with_token(
    access_token: &str,
    tool_name: &str,
) -> SubscriptionQuota {
    let mut result = query_gemini_quota(access_token).await;
    result.tool = tool_name.to_string();
    result
}

pub async fn query_antigravity_quota_with_token(
    access_token: &str,
    project_id: Option<&str>,
    tool_name: &str,
    profile: crate::services::antigravity_models::AntigravityClientProfile,
) -> SubscriptionQuota {
    // Fetch plan tier via loadCodeAssist (integer enums required for Antigravity).
    let plan_label =
        fetch_antigravity_plan_label(&crate::proxy::http_client::get(), access_token).await;

    match crate::services::antigravity_models::fetch_antigravity_available_models(
        access_token,
        project_id,
        profile,
    )
    .await
    {
        Ok(models) => {
            let tiers =
                crate::services::antigravity_models::antigravity_models_to_quota_tiers(&models);
            if !tiers.is_empty() {
                return SubscriptionQuota {
                    tool: tool_name.to_string(),
                    credential_status: CredentialStatus::Valid,
                    credential_message: plan_label,
                    subscription: None,
                    success: true,
                    tiers,
                    extra_usage: None,
                    error: None,
                    queried_at: Some(now_millis()),
                    failure: None,
                };
            }
        }
        Err(err) => {
            log::warn!("Antigravity fetchAvailableModels quota failed, falling back: {err}");
        }
    }

    let mut result = retrieve_user_quota(access_token, project_id, plan_label).await;
    result.tool = tool_name.to_string();
    result
}

/// Call loadCodeAssist with Antigravity integer-enum metadata to get currentTier name.
async fn fetch_antigravity_plan_label(
    client: &reqwest::Client,
    access_token: &str,
) -> Option<String> {
    let platform: i64 = {
        #[cfg(target_os = "macos")]
        {
            if cfg!(target_arch = "aarch64") {
                2
            } else {
                1
            }
        }
        #[cfg(target_os = "linux")]
        {
            if cfg!(target_arch = "aarch64") {
                4
            } else {
                3
            }
        }
        #[cfg(target_os = "windows")]
        {
            5
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            0
        }
    };
    let metadata = serde_json::json!({ "ideType": 9, "platform": platform, "pluginType": 2 });
    let resp = client
        .post("https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist")
        .header("Authorization", format!("Bearer {access_token}"))
        .header("Content-Type", "application/json")
        .header("client-metadata", metadata.to_string())
        .json(&serde_json::json!({ "metadata": metadata }))
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: serde_json::Value = resp.json().await.ok()?;
    body.pointer("/currentTier/name")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

pub async fn query_cursor_quota(
    account: &crate::proxy::providers::cursor_oauth_auth::CursorAccountData,
    access_token: &str,
) -> SubscriptionQuota {
    query_cursor_quota_with_tool(account, access_token, "cursor_oauth").await
}

pub async fn query_cursor_quota_with_tool(
    account: &crate::proxy::providers::cursor_oauth_auth::CursorAccountData,
    access_token: &str,
    tool: &str,
) -> SubscriptionQuota {
    let client = crate::proxy::http_client::get();

    // Parallel: Stripe status + GetCurrentPeriodUsage
    let (stripe_result, usage_result) = tokio::join!(
        fetch_cursor_stripe_status(&client, account, access_token),
        fetch_cursor_period_usage(&client, account, access_token),
    );

    let credential_message = stripe_result
        .as_ref()
        .ok()
        .and_then(|s| s.get("membershipType").and_then(|v| v.as_str()))
        .map(format_cursor_membership_label)
        .or(Some("Cursor".to_string()));

    let (tiers, billing_cycle_end) = match usage_result {
        Ok(usage) => (
            parse_cursor_usage_tiers(&usage),
            cursor_billing_cycle_end(&usage),
        ),
        Err(_) => (vec![], None),
    };
    let subscription =
        build_cursor_subscription_info(credential_message.as_deref(), billing_cycle_end);

    SubscriptionQuota {
        tool: tool.to_string(),
        credential_status: CredentialStatus::Valid,
        credential_message,
        subscription,
        success: true,
        tiers,
        extra_usage: None,
        error: None,
        queried_at: Some(chrono::Utc::now().timestamp_millis()),
        failure: None,
    }
}

async fn fetch_cursor_stripe_status(
    client: &reqwest::Client,
    account: &crate::proxy::providers::cursor_oauth_auth::CursorAccountData,
    access_token: &str,
) -> Result<serde_json::Value, String> {
    // The WorkOS session cookie expects the real WorkOS user id (carried in the
    // access token's `sub` claim), not our synthetic account id. Fall back to
    // the account id only if the token can't be decoded.
    let session_user =
        crate::proxy::providers::cursor_oauth_auth::workos_user_id_from_token(access_token)
            .unwrap_or_else(|| account.account_id.clone());
    let cookie = format!(
        "WorkosCursorSessionToken={}%3A%3A{}",
        session_user, access_token
    );
    let resp = client
        .get("https://cursor.com/api/auth/stripe")
        .header("cookie", &cookie)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("stripe status {}", resp.status()));
    }
    resp.json().await.map_err(|e| e.to_string())
}

async fn fetch_cursor_period_usage(
    client: &reqwest::Client,
    account: &crate::proxy::providers::cursor_oauth_auth::CursorAccountData,
    access_token: &str,
) -> Result<serde_json::Value, String> {
    let mut req = client
        .post("https://api2.cursor.sh/aiserver.v1.DashboardService/GetCurrentPeriodUsage")
        .header("authorization", format!("Bearer {access_token}"))
        .header("connect-protocol-version", "1")
        .header("content-type", "application/json")
        .timeout(std::time::Duration::from_secs(10));
    for (key, value) in
        crate::proxy::providers::cursor_protocol::cursor_identity_headers(account, access_token)
    {
        req = req.header(key, value);
    }
    let resp = req.body("{}").send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("usage status {}", resp.status()));
    }
    resp.json().await.map_err(|e| e.to_string())
}

fn format_cursor_membership_label(membership_type: &str) -> String {
    match membership_type.to_lowercase().as_str() {
        "free" => "Cursor Free".to_string(),
        "pro" => "Cursor Pro".to_string(),
        "pro_plus" | "pro+" => "Cursor Pro+".to_string(),
        "ultra" => "Cursor Ultra".to_string(),
        other => format!("Cursor {other}"),
    }
}

fn cursor_billing_cycle_end(usage: &serde_json::Value) -> Option<String> {
    usage
        .get("billingCycleEnd")
        .and_then(|v| {
            v.as_i64()
                .or_else(|| v.as_f64().map(|f| f as i64))
                .or_else(|| v.as_str().and_then(|s| s.parse::<i64>().ok()))
        })
        .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis)
        .map(|dt| dt.to_rfc3339())
}

fn build_cursor_subscription_info(
    plan_label: Option<&str>,
    billing_cycle_end: Option<String>,
) -> Option<SubscriptionInfo> {
    let plan_label = plan_label
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .map(str::to_string);
    if plan_label.is_none() && billing_cycle_end.is_none() {
        return None;
    }
    let expires_source = billing_cycle_end
        .as_ref()
        .map(|_| "cursor_dashboard.billingCycleEnd".to_string());
    let expires_kind = if billing_cycle_end.is_some() {
        Some(SubscriptionExpiresKind::BillingPeriod)
    } else {
        Some(SubscriptionExpiresKind::Unknown)
    };
    Some(SubscriptionInfo {
        plan_type: None,
        plan_label,
        expires_at: billing_cycle_end,
        expires_source,
        expires_kind,
    })
}

fn parse_cursor_usage_tiers(usage: &serde_json::Value) -> Vec<QuotaTier> {
    let plan_usage = match usage.get("planUsage") {
        Some(pu) => pu,
        None => return vec![],
    };

    let resets_at = cursor_billing_cycle_end(usage);

    let limit = plan_usage
        .get("limit")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);

    // 有 $ 限额（Pro / Pro+ / Ultra）：按金额渲染
    if limit > 0.0 {
        let used = plan_usage
            .get("used")
            .and_then(|v| v.as_f64())
            .or_else(|| {
                let remaining = plan_usage.get("remaining").and_then(|v| v.as_f64())?;
                Some(limit - remaining)
            })
            .unwrap_or(0.0);

        let utilization = plan_usage
            .get("totalPercentUsed")
            .and_then(|v| v.as_f64())
            .unwrap_or_else(|| (used / limit) * 100.0);

        return vec![QuotaTier {
            name: "cursor_credits".to_string(),
            utilization,
            resets_at,
            used: Some(used / 100.0),
            limit: Some(limit / 100.0),
            unit: Some("USD".to_string()),
            used_value_usd: None,
            max_value_usd: None,
        }];
    }

    // free 套餐：API 不暴露具体额度数字，只回传 totalPercentUsed。
    // 用百分比单独渲染一个 tier，至少让卡片有用量信息可看。
    if let Some(pct) = plan_usage.get("totalPercentUsed").and_then(|v| v.as_f64()) {
        return vec![QuotaTier {
            name: "cursor_included_usage".to_string(),
            utilization: pct,
            resets_at,
            used: None,
            limit: None,
            unit: None,
            used_value_usd: None,
            max_value_usd: None,
        }];
    }

    vec![]
}

fn format_claude_plan_label(org_type: &str) -> String {
    match org_type {
        "claude_pro" => "Claude Pro".to_string(),
        "claude_max" => "Claude Max".to_string(),
        "claude_free" => "Claude Free".to_string(),
        "claude_team" => "Claude Team".to_string(),
        "claude_enterprise" => "Claude Enterprise".to_string(),
        other => other.to_string(),
    }
}

/// Fetch /api/oauth/profile and extract a plan label from organization_type.
/// Returns None on any error (non-fatal — quota still succeeds).
async fn fetch_claude_plan_label(client: &reqwest::Client, access_token: &str) -> Option<String> {
    let resp = client
        .get("https://api.anthropic.com/api/oauth/profile")
        .header("Authorization", format!("Bearer {access_token}"))
        .header("anthropic-beta", "oauth-2025-04-20")
        .header("Accept", "application/json")
        .header("accept-language", "*")
        .header("user-agent", "claude-cli/2.1.2 (external, cli)")
        .header("x-app", "cli")
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: serde_json::Value = resp.json().await.ok()?;
    let org_type = body
        .pointer("/organization/organization_type")
        .and_then(|v| v.as_str())?;
    Some(format_claude_plan_label(org_type))
}

/// 查询 Claude 官方订阅额度
async fn query_claude_quota(access_token: &str) -> SubscriptionQuota {
    let client = crate::proxy::http_client::get();

    let (resp, plan_label) = tokio::join!(
        client
            .get("https://api.anthropic.com/api/oauth/usage")
            .header("Authorization", format!("Bearer {access_token}"))
            .header("anthropic-beta", "oauth-2025-04-20")
            .header("Accept", "application/json")
            .header("accept-language", "*")
            .header("user-agent", "claude-cli/2.1.2 (external, cli)")
            .header("x-app", "cli")
            .timeout(std::time::Duration::from_secs(10))
            .send(),
        fetch_claude_plan_label(&client, access_token),
    );
    let resp = resp;

    let resp = match resp {
        Ok(r) => r,
        Err(e) => {
            return SubscriptionQuota::failure(
                "claude",
                QuotaFailure::from_reqwest(&e),
                format!("Network error: {e}"),
            );
        }
    };

    let status = resp.status();

    if !status.is_success() {
        let retry_after = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let failure = classify_http_failure(status, retry_after.as_deref());
        let body = resp.text().await.unwrap_or_default();
        let message = match &failure {
            QuotaFailure::Unauthorized => {
                format!("Authentication failed (HTTP {status}). Please re-login with Claude CLI.")
            }
            QuotaFailure::RateLimited { retry_after_ms } => format!(
                "Rate limited by Anthropic (HTTP {status}, retry after {}s)",
                retry_after_ms / 1000
            ),
            _ => format!(
                "API error (HTTP {status}): {}",
                truncate_for_log(&body, 200)
            ),
        };
        return SubscriptionQuota::failure("claude", failure, message);
    }

    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            return SubscriptionQuota::failure(
                "claude",
                QuotaFailure::Parse,
                format!("Failed to parse API response: {e}"),
            );
        }
    };

    // 解析已知的 tier 窗口
    let mut tiers = Vec::new();
    for &tier_name in KNOWN_TIERS {
        if let Some(window) = body.get(tier_name) {
            if let Ok(w) = serde_json::from_value::<ApiUsageWindow>(window.clone()) {
                if let Some(util) = w.utilization {
                    tiers.push(QuotaTier {
                        name: normalize_claude_tier_name(tier_name).to_string(),
                        utilization: util,
                        resets_at: w.resets_at,
                        used: None,
                        limit: None,
                        unit: None,
                        used_value_usd: None,
                        max_value_usd: None,
                    });
                }
            }
        }
    }

    // 也解析未知窗口（API 可能返回新的窗口类型）
    if let Some(obj) = body.as_object() {
        for (key, value) in obj {
            if key == "extra_usage" || KNOWN_TIERS.contains(&key.as_str()) {
                continue;
            }
            if let Ok(w) = serde_json::from_value::<ApiUsageWindow>(value.clone()) {
                if let Some(util) = w.utilization {
                    tiers.push(QuotaTier {
                        name: normalize_claude_tier_name(key).to_string(),
                        utilization: util,
                        resets_at: w.resets_at,
                        used: None,
                        limit: None,
                        unit: None,
                        used_value_usd: None,
                        max_value_usd: None,
                    });
                }
            }
        }
    }

    // 解析超额使用
    let extra_usage = body.get("extra_usage").and_then(|v| {
        serde_json::from_value::<ApiExtraUsage>(v.clone())
            .ok()
            .map(|e| ExtraUsage {
                is_enabled: e.is_enabled.unwrap_or(false),
                monthly_limit: e.monthly_limit,
                used_credits: e.used_credits,
                utilization: e.utilization,
                currency: e.currency,
            })
    });

    SubscriptionQuota {
        tool: "claude".to_string(),
        credential_status: CredentialStatus::Valid,
        credential_message: plan_label,
        subscription: None,
        success: true,
        tiers,
        extra_usage,
        error: None,
        queried_at: Some(now_millis()),
        failure: None,
    }
}

// ── Codex 凭据读取 ──────────────────────────────────────

#[derive(Deserialize)]
struct CodexAuthJson {
    auth_mode: Option<String>,
    tokens: Option<CodexTokens>,
    last_refresh: Option<String>,
}

#[derive(Deserialize)]
struct CodexTokens {
    access_token: Option<String>,
    account_id: Option<String>,
}

/// (access_token, account_id, status, message)
type CodexCredentials = (
    Option<String>,
    Option<String>,
    CredentialStatus,
    Option<String>,
);

/// 读取 Codex OAuth 凭据
///
/// 按优先级尝试以下来源：
/// 1. macOS Keychain (service: "Codex Auth")
/// 2. 凭据文件 ~/.codex/auth.json
///
/// 仅 auth_mode == "chatgpt" (OAuth) 时有效，API key 模式不支持用量查询。
fn read_codex_credentials() -> CodexCredentials {
    #[cfg(target_os = "macos")]
    {
        if let Some(result) = read_codex_credentials_from_keychain() {
            return result;
        }
    }

    read_codex_credentials_from_file()
}

/// 从 macOS Keychain 读取 Codex 凭据
#[cfg(target_os = "macos")]
fn read_codex_credentials_from_keychain() -> Option<CodexCredentials> {
    let output = std::process::Command::new("security")
        .args(["find-generic-password", "-s", "Codex Auth", "-w"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let json_str = String::from_utf8(output.stdout).ok()?;
    let json_str = json_str.trim();
    if json_str.is_empty() {
        return None;
    }

    Some(parse_codex_credentials_json(json_str))
}

/// 从文件读取 Codex 凭据
fn read_codex_credentials_from_file() -> CodexCredentials {
    let auth_path = crate::codex_config::get_codex_auth_path();

    if !auth_path.exists() {
        return (None, None, CredentialStatus::NotFound, None);
    }

    let content = match std::fs::read_to_string(&auth_path) {
        Ok(c) => c,
        Err(e) => {
            return (
                None,
                None,
                CredentialStatus::ParseError,
                Some(format!("Failed to read Codex auth file: {e}")),
            );
        }
    };

    parse_codex_credentials_json(&content)
}

/// 解析 Codex 凭据 JSON（Keychain 和文件共用）
fn parse_codex_credentials_json(content: &str) -> CodexCredentials {
    let auth: CodexAuthJson = match serde_json::from_str(content) {
        Ok(a) => a,
        Err(e) => {
            return (
                None,
                None,
                CredentialStatus::ParseError,
                Some(format!("Failed to parse Codex auth JSON: {e}")),
            );
        }
    };

    // 仅 OAuth 模式有用量数据
    if auth.auth_mode.as_deref() != Some("chatgpt") {
        return (
            None,
            None,
            CredentialStatus::NotFound,
            Some("Codex not using OAuth mode".to_string()),
        );
    }

    let tokens = match auth.tokens {
        Some(t) => t,
        None => {
            return (
                None,
                None,
                CredentialStatus::ParseError,
                Some("No tokens in Codex auth".to_string()),
            );
        }
    };

    let access_token = match tokens.access_token {
        Some(t) if !t.is_empty() => t,
        _ => {
            return (
                None,
                None,
                CredentialStatus::ParseError,
                Some("access_token is empty or missing".to_string()),
            );
        }
    };

    // 检查 token 是否可能过期（距上次刷新 > 8 天）
    if let Some(ref last_refresh) = auth.last_refresh {
        if is_codex_token_stale(last_refresh) {
            return (
                Some(access_token),
                tokens.account_id,
                CredentialStatus::Expired,
                Some("Codex token may be stale (>8 days since last refresh)".to_string()),
            );
        }
    }

    (
        Some(access_token),
        tokens.account_id,
        CredentialStatus::Valid,
        None,
    )
}

/// 判断 Codex token 是否可能过期（Codex CLI 在 >8 天时自动刷新）
fn is_codex_token_stale(last_refresh: &str) -> bool {
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(last_refresh) {
        let age_secs = now_secs.saturating_sub(dt.timestamp() as u64);
        age_secs > 8 * 24 * 3600
    } else {
        false
    }
}

// ── Codex API 查询 ──────────────────────────────────────

pub(crate) fn normalize_chatgpt_plan_type(plan: &str) -> String {
    plan.trim().to_ascii_lowercase().replace(['-', ' '], "_")
}

pub(crate) fn format_chatgpt_plan_label(plan: &str) -> String {
    match normalize_chatgpt_plan_type(plan).as_str() {
        "free" => "ChatGPT Free".to_string(),
        "plus" => "ChatGPT Plus".to_string(),
        "prolite" | "pro_lite" => "ChatGPT Pro 5x".to_string(),
        "pro" => "ChatGPT Pro 20x".to_string(),
        "team" => "ChatGPT Team".to_string(),
        "business" | "self_serve_business_usage_based" => "ChatGPT Business".to_string(),
        "enterprise" | "hc" | "enterprise_cbp_usage_based" => "ChatGPT Enterprise".to_string(),
        "edu" | "education" | "edu_plus" | "edu_pro" => "ChatGPT Edu".to_string(),
        _ => plan.trim().to_string(),
    }
}

#[derive(Deserialize)]
struct CodexRateLimitWindow {
    used_percent: Option<f64>,
    limit_window_seconds: Option<i64>,
    reset_at: Option<i64>,
}

#[derive(Deserialize)]
struct CodexRateLimit {
    primary_window: Option<CodexRateLimitWindow>,
    secondary_window: Option<CodexRateLimitWindow>,
}

#[derive(Deserialize)]
struct CodexUsageResponse {
    plan_type: Option<String>,
    rate_limit: Option<CodexRateLimit>,
}

const CHATGPT_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const CHATGPT_ACCOUNTS_CHECK_URL: &str =
    "https://chatgpt.com/backend-api/accounts/check/v4-2023-04-27";
const CHATGPT_SUBSCRIPTIONS_URL: &str = "https://chatgpt.com/backend-api/subscriptions";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ChatGptSubscriptionLookup {
    plan_type: Option<String>,
    expires_at: Option<String>,
    expires_source: Option<String>,
}

/// 根据窗口秒数映射到 tier 名称（与 Claude 的命名兼容以复用前端 i18n）
fn window_seconds_to_tier_name(secs: i64) -> String {
    match secs {
        18000 => "five_hour".to_string(),
        604800 => TIER_SEVEN_DAY.to_string(),
        2592000 => TIER_THIRTY_DAY.to_string(),
        s => {
            let hours = s / 3600;
            if hours >= 24 {
                format!("{}_day", hours / 24)
            } else {
                format!("{}_hour", hours)
            }
        }
    }
}

/// Unix 时间戳（秒）转 ISO 8601 字符串
fn unix_ts_to_iso(ts: i64) -> Option<String> {
    chrono::DateTime::from_timestamp(ts, 0).map(|dt| dt.to_rfc3339())
}

fn normalize_rfc3339_string(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    chrono::DateTime::parse_from_rfc3339(trimmed)
        .ok()
        .map(|dt| dt.to_rfc3339())
}

fn extract_chatgpt_account_plan_type(account: &serde_json::Value) -> Option<String> {
    account
        .pointer("/account/plan_type")
        .and_then(|v| v.as_str())
        .or_else(|| {
            account
                .pointer("/entitlement/subscription_plan")
                .and_then(|v| v.as_str())
        })
        .map(normalize_chatgpt_plan_type)
        .filter(|value| !value.is_empty())
}

fn extract_chatgpt_entitlement_expires_at(account: &serde_json::Value) -> Option<String> {
    account
        .pointer("/entitlement/expires_at")
        .and_then(|v| v.as_str())
        .and_then(normalize_rfc3339_string)
}

fn chatgpt_account_lookup_from_value(
    account: &serde_json::Value,
) -> Option<ChatGptSubscriptionLookup> {
    let plan_type = extract_chatgpt_account_plan_type(account);
    let expires_at = extract_chatgpt_entitlement_expires_at(account);
    if plan_type.is_none() && expires_at.is_none() {
        return None;
    }
    let expires_source = expires_at
        .as_ref()
        .map(|_| "accounts_check_entitlement".to_string());
    Some(ChatGptSubscriptionLookup {
        plan_type,
        expires_at,
        expires_source,
    })
}

fn chatgpt_account_matches_id(account: &serde_json::Value, account_id: &str) -> bool {
    let account_id = account_id.trim();
    if account_id.is_empty() {
        return false;
    }
    [
        "/account/id",
        "/account/account_id",
        "/account/chatgpt_account_id",
        "/account/organization_id",
        "/id",
        "/account_id",
        "/chatgpt_account_id",
        "/organization_id",
    ]
    .iter()
    .any(|path| account.pointer(path).and_then(|v| v.as_str()) == Some(account_id))
}

fn parse_chatgpt_accounts_check_lookup(
    body: &serde_json::Value,
    account_id: Option<&str>,
) -> Option<ChatGptSubscriptionLookup> {
    let accounts = body.get("accounts")?.as_object()?;
    let account_id = account_id.map(str::trim).filter(|id| !id.is_empty());

    if let Some(id) = account_id {
        if let Some(account) = accounts.get(id) {
            if let Some(lookup) = chatgpt_account_lookup_from_value(account) {
                return Some(lookup);
            }
        }
        for account in accounts.values() {
            if chatgpt_account_matches_id(account, id) {
                if let Some(lookup) = chatgpt_account_lookup_from_value(account) {
                    return Some(lookup);
                }
            }
        }
    }

    let mut default_candidate = None;
    let mut paid_candidate = None;
    let mut any_candidate = None;

    for account in accounts.values() {
        let Some(lookup) = chatgpt_account_lookup_from_value(account) else {
            continue;
        };
        if any_candidate.is_none() {
            any_candidate = Some(lookup.clone());
        }
        if default_candidate.is_none()
            && account
                .pointer("/account/is_default")
                .and_then(|v| v.as_bool())
                == Some(true)
        {
            default_candidate = Some(lookup.clone());
        }
        if paid_candidate.is_none()
            && lookup
                .plan_type
                .as_deref()
                .is_some_and(|plan| plan != "free")
        {
            paid_candidate = Some(lookup);
        }
    }

    default_candidate.or(paid_candidate).or(any_candidate)
}

fn parse_chatgpt_subscription_lookup(
    body: &serde_json::Value,
) -> Option<ChatGptSubscriptionLookup> {
    let plan_type = body
        .get("plan_type")
        .and_then(|v| v.as_str())
        .map(normalize_chatgpt_plan_type)
        .filter(|value| !value.is_empty());
    let expires_at = body
        .get("active_until")
        .and_then(|v| v.as_str())
        .and_then(normalize_rfc3339_string);
    if plan_type.is_none() && expires_at.is_none() {
        return None;
    }
    let expires_source = expires_at
        .as_ref()
        .map(|_| "subscriptions_active_until".to_string());
    Some(ChatGptSubscriptionLookup {
        plan_type,
        expires_at,
        expires_source,
    })
}

async fn fetch_chatgpt_account_lookup(
    client: &reqwest::Client,
    access_token: &str,
    account_id: Option<&str>,
) -> Option<ChatGptSubscriptionLookup> {
    let resp = match client
        .get(CHATGPT_ACCOUNTS_CHECK_URL)
        .header("Authorization", format!("Bearer {access_token}"))
        .header("Origin", "https://chatgpt.com")
        .header("Referer", "https://chatgpt.com/")
        .header("Accept", "application/json")
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
    {
        Ok(resp) => resp,
        Err(err) => {
            log::debug!("[CodexQuota] accounts/check request failed: {err}");
            return None;
        }
    };

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        log::debug!(
            "[CodexQuota] accounts/check returned HTTP {status}: {}",
            truncate_for_log(&body, 200)
        );
        return None;
    }

    match resp.json::<serde_json::Value>().await {
        Ok(body) => parse_chatgpt_accounts_check_lookup(&body, account_id),
        Err(err) => {
            log::debug!("[CodexQuota] accounts/check parse failed: {err}");
            None
        }
    }
}

async fn fetch_chatgpt_subscription_lookup(
    client: &reqwest::Client,
    access_token: &str,
    account_id: Option<&str>,
) -> Option<ChatGptSubscriptionLookup> {
    let account_id = account_id.map(str::trim).filter(|id| !id.is_empty())?;
    let resp = match client
        .get(CHATGPT_SUBSCRIPTIONS_URL)
        .query(&[("account_id", account_id)])
        .header("Authorization", format!("Bearer {access_token}"))
        .header("Origin", "https://chatgpt.com")
        .header("Referer", "https://chatgpt.com/")
        .header("Accept", "application/json")
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
    {
        Ok(resp) => resp,
        Err(err) => {
            log::debug!("[CodexQuota] subscriptions request failed: {err}");
            return None;
        }
    };

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        log::debug!(
            "[CodexQuota] subscriptions returned HTTP {status}: {}",
            truncate_for_log(&body, 200)
        );
        return None;
    }

    match resp.json::<serde_json::Value>().await {
        Ok(body) => parse_chatgpt_subscription_lookup(&body),
        Err(err) => {
            log::debug!("[CodexQuota] subscriptions parse failed: {err}");
            None
        }
    }
}

fn merge_chatgpt_subscription_lookup(
    mut primary: Option<ChatGptSubscriptionLookup>,
    fallback: Option<ChatGptSubscriptionLookup>,
) -> Option<ChatGptSubscriptionLookup> {
    match (&mut primary, fallback) {
        (Some(primary), Some(fallback)) => {
            if primary.plan_type.is_none() {
                primary.plan_type = fallback.plan_type;
            }
            if primary.expires_at.is_none() {
                primary.expires_at = fallback.expires_at;
                primary.expires_source = fallback.expires_source;
            }
            Some(primary.clone())
        }
        (Some(primary), None) => Some(primary.clone()),
        (None, fallback) => fallback,
    }
}

fn build_chatgpt_subscription_info(
    usage_plan_type: Option<&str>,
    usage_plan_label: Option<&str>,
    lookup: Option<ChatGptSubscriptionLookup>,
) -> Option<SubscriptionInfo> {
    let lookup = lookup.unwrap_or_default();
    let plan_type = usage_plan_type
        .map(normalize_chatgpt_plan_type)
        .filter(|value| !value.is_empty())
        .or(lookup.plan_type);
    let plan_label = plan_type
        .as_deref()
        .map(format_chatgpt_plan_label)
        .or_else(|| {
            usage_plan_label
                .map(str::trim)
                .filter(|label| !label.is_empty())
                .map(str::to_string)
        });

    if plan_type.is_none() && plan_label.is_none() && lookup.expires_at.is_none() {
        return None;
    }

    let expires_kind = if lookup.expires_at.is_some() {
        Some(SubscriptionExpiresKind::Subscription)
    } else {
        Some(SubscriptionExpiresKind::Unknown)
    };

    Some(SubscriptionInfo {
        plan_type,
        plan_label,
        expires_at: lookup.expires_at,
        expires_source: lookup.expires_source,
        expires_kind,
    })
}

/// 查询 Codex / ChatGPT 反代订阅额度
///
/// 参数化 `tool_label` 和 `expired_message` 让该函数可被两个调用点共用：
/// - `"codex"` + "Please re-login with Codex CLI."（CLI 凭据路径）
/// - `"codex_oauth"` + "Please re-login via cc-switch."（cc-switch 自管 OAuth 路径）
pub(crate) async fn query_codex_quota(
    access_token: &str,
    account_id: Option<&str>,
    tool_label: &str,
    expired_message: &str,
) -> SubscriptionQuota {
    query_codex_quota_with_plan(access_token, account_id, tool_label, expired_message)
        .await
        .0
}

pub(crate) async fn query_codex_quota_with_plan(
    access_token: &str,
    account_id: Option<&str>,
    tool_label: &str,
    expired_message: &str,
) -> (SubscriptionQuota, Option<String>) {
    let client = crate::proxy::http_client::get();

    let mut req = client
        .get(CHATGPT_USAGE_URL)
        .header("Authorization", format!("Bearer {access_token}"))
        .header("User-Agent", "codex-cli")
        .header("Accept", "application/json");

    if let Some(id) = account_id {
        req = req.header("ChatGPT-Account-Id", id);
    }

    let resp = match req.timeout(std::time::Duration::from_secs(15)).send().await {
        Ok(r) => r,
        Err(e) => {
            return (
                SubscriptionQuota::failure(
                    tool_label,
                    QuotaFailure::from_reqwest(&e),
                    format!("Network error: {e}"),
                ),
                None,
            );
        }
    };

    let status = resp.status();

    if !status.is_success() {
        let retry_after = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let failure = classify_http_failure(status, retry_after.as_deref());
        let body = resp.text().await.unwrap_or_default();
        let message = match &failure {
            QuotaFailure::Unauthorized => format!("{expired_message} (HTTP {status})"),
            QuotaFailure::RateLimited { retry_after_ms } => format!(
                "Rate limited by Codex upstream (HTTP {status}, retry after {}s)",
                retry_after_ms / 1000
            ),
            _ => format!(
                "API error (HTTP {status}): {}",
                truncate_for_log(&body, 200)
            ),
        };
        return (
            SubscriptionQuota::failure(tool_label, failure, message),
            None,
        );
    }

    let body: CodexUsageResponse = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            return (
                SubscriptionQuota::failure(
                    tool_label,
                    QuotaFailure::Parse,
                    format!("Failed to parse API response: {e}"),
                ),
                None,
            );
        }
    };

    let plan_type = body
        .plan_type
        .as_deref()
        .map(normalize_chatgpt_plan_type)
        .filter(|value| !value.is_empty());
    let plan_label = plan_type.as_deref().map(format_chatgpt_plan_label);

    let (account_lookup, fallback_lookup) = tokio::join!(
        fetch_chatgpt_account_lookup(&client, access_token, account_id),
        fetch_chatgpt_subscription_lookup(&client, access_token, account_id)
    );
    let subscription_lookup = if account_lookup
        .as_ref()
        .and_then(|lookup| lookup.expires_at.as_ref())
        .is_some()
    {
        account_lookup
    } else {
        merge_chatgpt_subscription_lookup(account_lookup, fallback_lookup)
    };
    let subscription = build_chatgpt_subscription_info(
        plan_type.as_deref(),
        plan_label.as_deref(),
        subscription_lookup,
    );
    let credential_message = subscription
        .as_ref()
        .and_then(|info| info.plan_label.clone())
        .or_else(|| plan_label.clone());

    let mut tiers = Vec::new();

    if let Some(rate_limit) = body.rate_limit {
        for window in [rate_limit.primary_window, rate_limit.secondary_window]
            .into_iter()
            .flatten()
        {
            if let Some(used) = window.used_percent {
                tiers.push(QuotaTier {
                    name: window
                        .limit_window_seconds
                        .map(window_seconds_to_tier_name)
                        .unwrap_or_else(|| "unknown".to_string()),
                    utilization: used,
                    resets_at: window.reset_at.and_then(unix_ts_to_iso),
                    used: None,
                    limit: None,
                    unit: None,
                    used_value_usd: None,
                    max_value_usd: None,
                });
            }
        }
    }

    (
        SubscriptionQuota {
            tool: tool_label.to_string(),
            credential_status: CredentialStatus::Valid,
            credential_message,
            subscription,
            success: true,
            tiers,
            extra_usage: None,
            error: None,
            queried_at: Some(now_millis()),
            failure: None,
        },
        plan_type,
    )
}

// ── Gemini 凭据读取 ──────────────────────────────────────

/// Gemini OAuth 凭据文件格式（~/.gemini/oauth_creds.json）
#[derive(Deserialize)]
struct GeminiOAuthCredsFile {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expiry_date: Option<i64>, // 毫秒时间戳
}

/// (access_token, refresh_token, status, message)
type GeminiCredentials = (
    Option<String>,
    Option<String>,
    CredentialStatus,
    Option<String>,
);

/// 读取 Gemini OAuth 凭据
///
/// 按优先级尝试以下来源：
/// 1. macOS Keychain (service: "gemini-cli-oauth", account: "main-account")
/// 2. 凭据文件 ~/.gemini/oauth_creds.json（遗留格式）
///
/// 仅 OAuth 认证模式（`oauth-personal`）有效；API key 模式无法查询官方用量。
fn read_gemini_credentials() -> GeminiCredentials {
    let file_result = read_gemini_credentials_from_file();
    if !matches!(file_result.2, CredentialStatus::NotFound) {
        return file_result;
    }

    #[cfg(target_os = "macos")]
    {
        if let Some(result) = read_gemini_credentials_from_keychain() {
            return result;
        }
    }

    file_result
}

/// 从 macOS Keychain 读取 Gemini 凭据
#[cfg(target_os = "macos")]
fn read_gemini_credentials_from_keychain() -> Option<GeminiCredentials> {
    let output = std::process::Command::new("security")
        .args([
            "find-generic-password",
            "-s",
            "gemini-cli-oauth",
            "-a",
            "main-account",
            "-w",
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let json_str = String::from_utf8(output.stdout).ok()?;
    let json_str = json_str.trim();
    if json_str.is_empty() {
        return None;
    }

    Some(parse_gemini_keychain_json(json_str))
}

/// 解析 Keychain 格式的 Gemini 凭据
///
/// Keychain 格式（keytar）：
/// ```json
/// { "token": { "accessToken": "...", "refreshToken": "...", "expiresAt": 1234 }, "updatedAt": ... }
/// ```
#[cfg(target_os = "macos")]
fn parse_gemini_keychain_json(content: &str) -> GeminiCredentials {
    let parsed: serde_json::Value = match serde_json::from_str(content) {
        Ok(v) => v,
        Err(e) => {
            return (
                None,
                None,
                CredentialStatus::ParseError,
                Some(format!("Failed to parse Gemini keychain JSON: {e}")),
            )
        }
    };

    let token = match parsed.get("token") {
        Some(t) => t,
        None => {
            // Keychain 中可能是扁平格式，尝试文件格式解析
            return parse_gemini_file_json(content);
        }
    };

    let access_token = token
        .get("accessToken")
        .and_then(|v| v.as_str())
        .map(String::from);
    let refresh_token = token
        .get("refreshToken")
        .and_then(|v| v.as_str())
        .map(String::from);
    let expires_at = token.get("expiresAt").and_then(|v| v.as_i64());

    match access_token {
        Some(at) if !at.is_empty() => {
            // expiresAt 是毫秒时间戳
            if let Some(exp_ms) = expires_at {
                if exp_ms < now_millis() {
                    return (
                        Some(at),
                        refresh_token,
                        CredentialStatus::Expired,
                        Some("Gemini access token has expired".to_string()),
                    );
                }
            }
            (Some(at), refresh_token, CredentialStatus::Valid, None)
        }
        _ => (
            None,
            refresh_token,
            CredentialStatus::ParseError,
            Some("accessToken is empty or missing".to_string()),
        ),
    }
}

/// 从文件读取 Gemini 凭据
fn read_gemini_credentials_from_file() -> GeminiCredentials {
    let cred_path = crate::gemini_config::get_gemini_dir().join("oauth_creds.json");
    if !cred_path.exists() {
        return (None, None, CredentialStatus::NotFound, None);
    }

    let content = match std::fs::read_to_string(&cred_path) {
        Ok(c) => c,
        Err(e) => {
            return (
                None,
                None,
                CredentialStatus::ParseError,
                Some(format!("Failed to read Gemini credentials: {e}")),
            )
        }
    };

    parse_gemini_file_json(&content)
}

/// 解析文件格式的 Gemini 凭据
///
/// 文件格式（oauth_creds.json）：
/// ```json
/// { "access_token": "...", "refresh_token": "...", "expiry_date": 1234 }
/// ```
fn parse_gemini_file_json(content: &str) -> GeminiCredentials {
    let creds: GeminiOAuthCredsFile = match serde_json::from_str(content) {
        Ok(c) => c,
        Err(e) => {
            return (
                None,
                None,
                CredentialStatus::ParseError,
                Some(format!("Failed to parse Gemini credentials: {e}")),
            )
        }
    };

    let access_token = match creds.access_token {
        Some(t) if !t.is_empty() => t,
        _ => {
            return (
                None,
                creds.refresh_token,
                CredentialStatus::ParseError,
                Some("access_token is empty or missing".to_string()),
            )
        }
    };

    // expiry_date 是毫秒时间戳
    if let Some(exp_ms) = creds.expiry_date {
        if exp_ms < now_millis() {
            return (
                Some(access_token),
                creds.refresh_token,
                CredentialStatus::Expired,
                Some("Gemini access token has expired".to_string()),
            );
        }
    }

    (
        Some(access_token),
        creds.refresh_token,
        CredentialStatus::Valid,
        None,
    )
}

// ── Gemini Token 刷新 ──────────────────────────────────────

/// Gemini OAuth Client 凭据（公开值，来自 Gemini CLI 源码 google-gemini/gemini-cli）
const GEMINI_OAUTH_CLIENT_ID: &str =
    "681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com";
const GEMINI_OAUTH_CLIENT_SECRET: &str = "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";

/// 使用 refresh_token 刷新 Gemini access token
///
/// Google OAuth access_token 仅有 ~1h 有效期，需要定期用 refresh_token 刷新。
/// refresh_token 本身不过期（除非用户撤销授权）。
async fn refresh_gemini_token(refresh_token: &str) -> Option<String> {
    let client = crate::proxy::http_client::get();

    let resp = client
        .post("https://oauth2.googleapis.com/token")
        .form(&[
            ("client_id", GEMINI_OAUTH_CLIENT_ID),
            ("client_secret", GEMINI_OAUTH_CLIENT_SECRET),
            ("refresh_token", refresh_token),
            ("grant_type", "refresh_token"),
        ])
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .ok()?;

    if !resp.status().is_success() {
        return None;
    }

    let body: serde_json::Value = resp.json().await.ok()?;
    body.get("access_token")?.as_str().map(String::from)
}

// ── Gemini API 查询 ──────────────────────────────────────

/// loadCodeAssist 响应
#[derive(Deserialize)]
struct GeminiLoadCodeAssistResponse {
    #[serde(rename = "cloudaicompanionProject")]
    cloudaicompanion_project: Option<serde_json::Value>,
    #[serde(rename = "currentTier")]
    current_tier: Option<serde_json::Value>,
}

/// 配额 bucket
#[derive(Deserialize)]
struct GeminiBucketInfo {
    #[serde(rename = "remainingFraction")]
    remaining_fraction: Option<f64>,
    #[serde(rename = "resetTime")]
    reset_time: Option<String>,
    #[serde(rename = "modelId")]
    model_id: Option<String>,
}

/// retrieveUserQuota 响应
#[derive(Deserialize)]
struct GeminiQuotaResponse {
    buckets: Option<Vec<GeminiBucketInfo>>,
}

/// 从 loadCodeAssist 响应中提取项目 ID
fn extract_project_id(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Object(obj) => obj
            .get("id")
            .or_else(|| obj.get("projectId"))
            .and_then(|v| v.as_str())
            .map(String::from),
        _ => None,
    }
}

/// 将 Gemini 模型 ID 分类为 Pro / Flash / Flash Lite
fn classify_gemini_model(model_id: &str) -> &str {
    if model_id.contains("flash-lite") {
        TIER_GEMINI_FLASH_LITE
    } else if model_id.contains("flash") {
        TIER_GEMINI_FLASH
    } else if model_id.contains("pro") {
        TIER_GEMINI_PRO
    } else {
        model_id
    }
}

/// 查询 Gemini 官方订阅额度
///
/// 两步 API 调用：
/// 1. loadCodeAssist → 获取 cloudaicompanionProject
/// 2. retrieveUserQuota → 获取按模型分桶的配额数据
async fn query_gemini_quota(access_token: &str) -> SubscriptionQuota {
    let client = crate::proxy::http_client::get();

    // ── Step 1: loadCodeAssist 获取项目 ID ──
    let load_resp = client
        .post("https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist")
        .header("Authorization", format!("Bearer {access_token}"))
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "metadata": {
                "ideType": "GEMINI_CLI",
                "pluginType": "GEMINI"
            }
        }))
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await;

    let load_resp = match load_resp {
        Ok(r) => r,
        Err(e) => {
            return SubscriptionQuota::error(
                "gemini",
                CredentialStatus::Valid,
                format!("Network error (loadCodeAssist): {e}"),
            );
        }
    };

    let load_status = load_resp.status();
    if load_status == reqwest::StatusCode::UNAUTHORIZED
        || load_status == reqwest::StatusCode::FORBIDDEN
    {
        return SubscriptionQuota::error(
            "gemini",
            CredentialStatus::Expired,
            format!("Authentication failed (HTTP {load_status}). Please re-login with Gemini CLI."),
        );
    }
    if !load_status.is_success() {
        let body = load_resp.text().await.unwrap_or_default();
        return SubscriptionQuota::error(
            "gemini",
            CredentialStatus::Valid,
            format!("loadCodeAssist failed (HTTP {load_status}): {body}"),
        );
    }

    let load_body: GeminiLoadCodeAssistResponse = match load_resp.json().await {
        Ok(v) => v,
        Err(e) => {
            return SubscriptionQuota::error(
                "gemini",
                CredentialStatus::Valid,
                format!("Failed to parse loadCodeAssist response: {e}"),
            );
        }
    };

    let project_id = load_body
        .cloudaicompanion_project
        .as_ref()
        .and_then(extract_project_id);

    let plan_label = load_body
        .current_tier
        .as_ref()
        .and_then(|t| t.get("name"))
        .and_then(|v| v.as_str())
        .map(str::to_string);

    // ── Step 2: retrieveUserQuota 获取配额 ──
    retrieve_user_quota(access_token, project_id.as_deref(), plan_label).await
}

/// 调用 retrieveUserQuota 获取按模型分桶的配额数据。
///
/// 被 Gemini（先 loadCodeAssist 取得 project_id）与 Antigravity（project_id 已知，
/// 跳过 loadCodeAssist）复用，两者共享 Google Cloud AI Companion 后端。
async fn retrieve_user_quota(
    access_token: &str,
    project_id: Option<&str>,
    plan_label: Option<String>,
) -> SubscriptionQuota {
    let client = crate::proxy::http_client::get();

    let mut quota_body = serde_json::json!({});
    if let Some(pid) = project_id {
        quota_body["project"] = serde_json::Value::String(pid.to_string());
    }

    let quota_resp = client
        .post("https://cloudcode-pa.googleapis.com/v1internal:retrieveUserQuota")
        .header("Authorization", format!("Bearer {access_token}"))
        .header("Content-Type", "application/json")
        .json(&quota_body)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await;

    let quota_resp = match quota_resp {
        Ok(r) => r,
        Err(e) => {
            return SubscriptionQuota::error(
                "gemini",
                CredentialStatus::Valid,
                format!("Network error (retrieveUserQuota): {e}"),
            );
        }
    };

    let quota_status = quota_resp.status();
    if quota_status == reqwest::StatusCode::UNAUTHORIZED
        || quota_status == reqwest::StatusCode::FORBIDDEN
    {
        return SubscriptionQuota::error(
            "gemini",
            CredentialStatus::Expired,
            format!("Authentication failed (HTTP {quota_status})."),
        );
    }
    if !quota_status.is_success() {
        let body = quota_resp.text().await.unwrap_or_default();
        return SubscriptionQuota::error(
            "gemini",
            CredentialStatus::Valid,
            format!("retrieveUserQuota failed (HTTP {quota_status}): {body}"),
        );
    }

    let quota_data: GeminiQuotaResponse = match quota_resp.json().await {
        Ok(v) => v,
        Err(e) => {
            return SubscriptionQuota::error(
                "gemini",
                CredentialStatus::Valid,
                format!("Failed to parse quota response: {e}"),
            );
        }
    };

    // ── 按模型分类汇总，每类取最低 remainingFraction ──
    let mut category_map: HashMap<String, (f64, Option<String>)> = HashMap::new();

    if let Some(buckets) = quota_data.buckets {
        for bucket in buckets {
            let model_id = bucket.model_id.as_deref().unwrap_or("unknown");
            let category = classify_gemini_model(model_id).to_string();
            let remaining = bucket.remaining_fraction.unwrap_or(1.0).clamp(0.0, 1.0);

            let entry = category_map
                .entry(category)
                .or_insert((remaining, bucket.reset_time.clone()));
            if remaining < entry.0 {
                entry.0 = remaining;
                if bucket.reset_time.is_some() {
                    entry.1.clone_from(&bucket.reset_time);
                }
            }
        }
    }

    // 转换为 tiers（remainingFraction → utilization: 已用百分比）
    let sort_order = |name: &str| -> usize {
        match name {
            TIER_GEMINI_PRO => 0,
            TIER_GEMINI_FLASH => 1,
            TIER_GEMINI_FLASH_LITE => 2,
            _ => 3,
        }
    };

    let mut tiers: Vec<QuotaTier> = category_map
        .into_iter()
        .map(|(name, (remaining, reset_time))| QuotaTier {
            name,
            utilization: (1.0 - remaining) * 100.0,
            resets_at: reset_time,
            used: None,
            limit: None,
            unit: None,
            used_value_usd: None,
            max_value_usd: None,
        })
        .collect();

    tiers.sort_by_key(|t| sort_order(&t.name));

    SubscriptionQuota {
        tool: "gemini".to_string(),
        credential_status: CredentialStatus::Valid,
        credential_message: plan_label,
        subscription: None,
        success: true,
        tiers,
        extra_usage: None,
        error: None,
        queried_at: Some(now_millis()),
        failure: None,
    }
}

// ── 入口函数 ──────────────────────────────────────────────

/// 查询指定 CLI 工具的官方订阅额度
pub async fn get_subscription_quota(tool: &str) -> Result<SubscriptionQuota, String> {
    match tool {
        "claude" => {
            let (token, status, message) = read_claude_credentials();

            match status {
                CredentialStatus::NotFound => Ok(SubscriptionQuota::not_found("claude")),
                CredentialStatus::ParseError => Ok(SubscriptionQuota::error(
                    "claude",
                    CredentialStatus::ParseError,
                    message.unwrap_or_else(|| "Failed to parse credentials".to_string()),
                )),
                CredentialStatus::Expired => {
                    // 即使过期也尝试调用 API（token 可能实际上仍有效）
                    if let Some(token) = token {
                        let result = query_claude_quota(&token).await;
                        if result.success {
                            return Ok(result);
                        }
                    }
                    Ok(SubscriptionQuota::error(
                        "claude",
                        CredentialStatus::Expired,
                        message.unwrap_or_else(|| "OAuth token has expired".to_string()),
                    ))
                }
                CredentialStatus::Valid => {
                    let token = token.expect("token must be Some when status is Valid");
                    Ok(query_claude_quota(&token).await)
                }
            }
        }
        "codex" => {
            let (token, account_id, status, message) = read_codex_credentials();

            match status {
                CredentialStatus::NotFound => Ok(SubscriptionQuota::not_found("codex")),
                CredentialStatus::ParseError => Ok(SubscriptionQuota::error(
                    "codex",
                    CredentialStatus::ParseError,
                    message.unwrap_or_else(|| "Failed to parse credentials".to_string()),
                )),
                CredentialStatus::Expired => {
                    // 即使可能过期也尝试调用 API
                    if let Some(token) = token {
                        let result = query_codex_quota(
                            &token,
                            account_id.as_deref(),
                            "codex",
                            "Authentication failed. Please re-login with Codex CLI.",
                        )
                        .await;
                        if result.success {
                            return Ok(result);
                        }
                    }
                    Ok(SubscriptionQuota::error(
                        "codex",
                        CredentialStatus::Expired,
                        message.unwrap_or_else(|| "Codex OAuth token may be stale".to_string()),
                    ))
                }
                CredentialStatus::Valid => {
                    let token = token.expect("token must be Some when status is Valid");
                    Ok(query_codex_quota(
                        &token,
                        account_id.as_deref(),
                        "codex",
                        "Authentication failed. Please re-login with Codex CLI.",
                    )
                    .await)
                }
            }
        }
        "gemini" => {
            let (token, refresh_token, status, message) = read_gemini_credentials();

            match status {
                CredentialStatus::NotFound => Ok(SubscriptionQuota::not_found("gemini")),
                CredentialStatus::ParseError => Ok(SubscriptionQuota::error(
                    "gemini",
                    CredentialStatus::ParseError,
                    message.unwrap_or_else(|| "Failed to parse credentials".to_string()),
                )),
                CredentialStatus::Expired => {
                    // Gemini access_token 仅 ~1h 有效，尝试用 refresh_token 刷新
                    if let Some(ref rt) = refresh_token {
                        if let Some(new_token) = refresh_gemini_token(rt).await {
                            return Ok(query_gemini_quota(&new_token).await);
                        }
                    }
                    // 刷新失败，尝试用旧 token
                    if let Some(ref token) = token {
                        let result = query_gemini_quota(token).await;
                        if result.success {
                            return Ok(result);
                        }
                    }
                    Ok(SubscriptionQuota::error(
                        "gemini",
                        CredentialStatus::Expired,
                        message.unwrap_or_else(|| "Gemini OAuth token has expired".to_string()),
                    ))
                }
                CredentialStatus::Valid => {
                    let token = token.expect("token must be Some when status is Valid");
                    Ok(query_gemini_quota(&token).await)
                }
            }
        }
        _ => Ok(SubscriptionQuota::not_found(tool)),
    }
}

// ── 辅助函数 ──────────────────────────────────────────────

fn truncate_for_log(body: &str, max_chars: usize) -> String {
    if body.chars().count() <= max_chars {
        return body.to_string();
    }
    let prefix: String = body.chars().take(max_chars).collect();
    format!("{prefix}… (truncated)")
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[cfg(test)]
mod cursor_tests {
    use super::*;

    #[test]
    fn membership_label_maps_known_tiers() {
        assert_eq!(format_cursor_membership_label("free"), "Cursor Free");
        assert_eq!(format_cursor_membership_label("pro"), "Cursor Pro");
        assert_eq!(format_cursor_membership_label("pro_plus"), "Cursor Pro+");
        assert_eq!(format_cursor_membership_label("ultra"), "Cursor Ultra");
        // 大小写不敏感
        assert_eq!(format_cursor_membership_label("PRO"), "Cursor Pro");
        // 未知等级回落到原值
        assert_eq!(format_cursor_membership_label("team"), "Cursor team");
    }

    #[test]
    fn chatgpt_plan_label_distinguishes_pro_tiers() {
        assert_eq!(format_chatgpt_plan_label("free"), "ChatGPT Free");
        assert_eq!(format_chatgpt_plan_label("plus"), "ChatGPT Plus");
        assert_eq!(format_chatgpt_plan_label("prolite"), "ChatGPT Pro 5x");
        assert_eq!(format_chatgpt_plan_label("pro-lite"), "ChatGPT Pro 5x");
        assert_eq!(format_chatgpt_plan_label("pro"), "ChatGPT Pro 20x");
        assert_eq!(
            format_chatgpt_plan_label("self_serve_business_usage_based"),
            "ChatGPT Business"
        );
    }

    #[test]
    fn chatgpt_accounts_check_prefers_exact_account() {
        let body = serde_json::json!({
            "accounts": {
                "org-default": {
                    "account": {
                        "id": "org-default",
                        "plan_type": "free",
                        "is_default": true
                    },
                    "entitlement": {
                        "subscription_plan": "free"
                    }
                },
                "acct-target": {
                    "account": {
                        "id": "acct-target",
                        "plan_type": "plus",
                        "is_default": false
                    },
                    "entitlement": {
                        "subscription_plan": "plus",
                        "expires_at": "2026-07-12T00:00:00+00:00"
                    }
                }
            }
        });

        let lookup = parse_chatgpt_accounts_check_lookup(&body, Some("acct-target")).unwrap();
        assert_eq!(lookup.plan_type.as_deref(), Some("plus"));
        assert_eq!(
            lookup.expires_at.as_deref(),
            Some("2026-07-12T00:00:00+00:00")
        );
        assert_eq!(
            lookup.expires_source.as_deref(),
            Some("accounts_check_entitlement")
        );
    }

    #[test]
    fn chatgpt_accounts_check_prefers_default_then_paid() {
        let body = serde_json::json!({
            "accounts": {
                "paid": {
                    "account": {
                        "plan_type": "pro",
                        "is_default": false
                    },
                    "entitlement": {
                        "expires_at": "2026-08-01T00:00:00Z"
                    }
                },
                "default": {
                    "account": {
                        "plan_type": "plus",
                        "is_default": true
                    },
                    "entitlement": {
                        "expires_at": "2026-07-12T00:00:00Z"
                    }
                }
            }
        });

        let lookup = parse_chatgpt_accounts_check_lookup(&body, None).unwrap();
        assert_eq!(lookup.plan_type.as_deref(), Some("plus"));
        assert_eq!(
            lookup.expires_at.as_deref(),
            Some("2026-07-12T00:00:00+00:00")
        );

        let body_without_default = serde_json::json!({
            "accounts": {
                "free": {
                    "account": {
                        "plan_type": "free"
                    }
                },
                "paid": {
                    "account": {
                        "plan_type": "pro"
                    },
                    "entitlement": {
                        "expires_at": "2026-08-01T00:00:00Z"
                    }
                }
            }
        });
        let lookup = parse_chatgpt_accounts_check_lookup(&body_without_default, None).unwrap();
        assert_eq!(lookup.plan_type.as_deref(), Some("pro"));
    }

    #[test]
    fn chatgpt_subscriptions_lookup_reads_active_until() {
        let body = serde_json::json!({
            "plan_type": "plus",
            "active_until": "2026-07-12T00:00:00Z",
            "will_renew": true
        });

        let lookup = parse_chatgpt_subscription_lookup(&body).unwrap();
        assert_eq!(lookup.plan_type.as_deref(), Some("plus"));
        assert_eq!(
            lookup.expires_at.as_deref(),
            Some("2026-07-12T00:00:00+00:00")
        );
        assert_eq!(
            lookup.expires_source.as_deref(),
            Some("subscriptions_active_until")
        );
    }

    #[test]
    fn parse_usage_tiers_reads_plan_usage() {
        let usage = serde_json::json!({
            "planUsage": {
                "limit": 40000.0,       // cents
                "used": 4230.0,         // cents
                "totalPercentUsed": 10.575
            },
            "billingCycleEnd": 1_700_000_000_000_i64
        });
        let tiers = parse_cursor_usage_tiers(&usage);
        assert_eq!(tiers.len(), 1);
        let tier = &tiers[0];
        assert_eq!(tier.name, "cursor_credits");
        assert_eq!(tier.unit.as_deref(), Some("USD"));
        assert_eq!(tier.used, Some(42.30));
        assert_eq!(tier.limit, Some(400.0));
        assert!((tier.utilization - 10.575).abs() < 1e-6);
        assert!(tier.resets_at.is_some());
    }

    #[test]
    fn cursor_subscription_info_uses_billing_cycle_end() {
        let usage = serde_json::json!({
            "billingCycleEnd": "1780376587000",
            "planUsage": {
                "limit": 40000.0,
                "used": 4230.0,
                "totalPercentUsed": 10.575
            }
        });

        let subscription =
            build_cursor_subscription_info(Some("Cursor Pro"), cursor_billing_cycle_end(&usage))
                .unwrap();

        assert_eq!(subscription.plan_label.as_deref(), Some("Cursor Pro"));
        assert_eq!(
            subscription.expires_at.as_deref(),
            Some("2026-06-02T05:03:07+00:00")
        );
        assert_eq!(
            subscription.expires_source.as_deref(),
            Some("cursor_dashboard.billingCycleEnd")
        );
        assert_eq!(
            subscription.expires_kind.as_ref(),
            Some(&SubscriptionExpiresKind::BillingPeriod)
        );
    }

    #[test]
    fn parse_usage_tiers_derives_used_from_remaining() {
        let usage = serde_json::json!({
            "planUsage": {
                "limit": 1000.0,
                "remaining": 250.0
            }
        });
        let tiers = parse_cursor_usage_tiers(&usage);
        assert_eq!(tiers.len(), 1);
        let tier = &tiers[0];
        assert_eq!(tier.used, Some(7.5)); // (1000 - 250) / 100
                                          // 无 totalPercentUsed 时按 used/limit 推算
        assert!((tier.utilization - 75.0).abs() < 1e-6);
    }

    #[test]
    fn parse_usage_tiers_empty_when_no_limit() {
        assert!(parse_cursor_usage_tiers(&serde_json::json!({})).is_empty());
        assert!(parse_cursor_usage_tiers(&serde_json::json!({
            "planUsage": { "limit": 0.0 }
        }))
        .is_empty());
    }

    #[test]
    fn parse_usage_tiers_free_account_uses_percent_fallback() {
        // free 账号实测响应：无 limit/used/remaining，只有百分比和 displayMessage
        let usage = serde_json::json!({
            "billingCycleEnd": "1780376587000",
            "planUsage": {
                "autoPercentUsed": 0,
                "apiPercentUsed": 0,
                "totalPercentUsed": 0
            }
        });
        let tiers = parse_cursor_usage_tiers(&usage);
        assert_eq!(tiers.len(), 1);
        let tier = &tiers[0];
        assert_eq!(tier.name, "cursor_included_usage");
        assert_eq!(tier.utilization, 0.0);
        assert!(tier.used.is_none());
        assert!(tier.limit.is_none());
        assert!(tier.unit.is_none());
        assert!(tier.resets_at.is_some());
    }
}
