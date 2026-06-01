//! Codex OAuth Authentication Module
//!
//! 实现 OpenAI ChatGPT Plus/Pro 订阅的 OAuth Device Code 流程。
//! 支持多账号管理，每个 Provider 可关联不同的 ChatGPT 账号。
//!
//! ## 认证流程
//! 1. 启动 Device Code 流程，获取 device_auth_id 和 user_code
//! 2. 用户在浏览器中完成 ChatGPT 授权
//! 3. 轮询获取 authorization_code 和 code_verifier（注意：verifier 由服务端返回）
//! 4. 使用 code + verifier 换取 access_token + refresh_token + id_token
//! 5. 自动刷新 access_token（到期前 60 秒）
//!
//! ## 多账号支持
//! - 每个 ChatGPT 账号独立存储 refresh_token
//! - Provider 通过 meta.authBinding 关联账号（auth_provider = "codex_oauth"）
//! - 通过 JWT id_token 提取 chatgpt_account_id 作为账号唯一标识

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

use super::copilot_auth::{GitHubAccount, GitHubDeviceCodeResponse};

/// OpenAI OAuth 客户端 ID（OpenCode 使用，与官方 Codex CLI 相同）
const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

/// Device Code 启动 URL
const DEVICE_AUTH_USERCODE_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/usercode";

/// Device Code 轮询 URL
const DEVICE_AUTH_TOKEN_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/token";

/// OAuth Token URL（用于 code 换 token 和 refresh token）
const OAUTH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";

/// Device Code 验证 URL（向用户展示）
const DEVICE_VERIFICATION_URL: &str = "https://auth.openai.com/codex/device";

/// Device Code 流程的 redirect_uri（OpenAI 服务端约定）
const DEVICE_REDIRECT_URI: &str = "https://auth.openai.com/deviceauth/callback";

/// Token 刷新提前量（毫秒）
const TOKEN_REFRESH_BUFFER_MS: i64 = 60_000;

/// Device Code 默认有效时长（秒），OpenAI 文档约定 15 分钟
const DEVICE_CODE_DEFAULT_EXPIRES_IN: u64 = 900;

/// 轮询间隔安全余量（秒）
const POLLING_SAFETY_MARGIN_SECS: u64 = 3;

/// User-Agent
const CODEX_USER_AGENT: &str = "cc-switch-codex-oauth";

/// Codex OAuth 错误
#[derive(Debug, thiserror::Error)]
pub enum CodexOAuthError {
    #[error("等待用户授权中")]
    AuthorizationPending,

    #[error("用户拒绝授权")]
    AccessDenied,

    #[error("Device Code 已过期")]
    ExpiredToken,

    #[error("OAuth Token 获取失败: {0}")]
    TokenFetchFailed(String),

    #[error("Refresh Token 失效或已过期")]
    RefreshTokenInvalid,

    #[error("账号已交接给下游消费方，已停止自动续期。如需恢复请先 restore。")]
    AccountHandedOff,

    #[error("网络错误: {0}")]
    NetworkError(String),

    #[error("解析错误: {0}")]
    ParseError(String),

    #[error("IO 错误: {0}")]
    IoError(String),

    #[error("账号不存在: {0}")]
    AccountNotFound(String),
}

impl From<reqwest::Error> for CodexOAuthError {
    fn from(err: reqwest::Error) -> Self {
        CodexOAuthError::NetworkError(err.to_string())
    }
}

impl From<std::io::Error> for CodexOAuthError {
    fn from(err: std::io::Error) -> Self {
        CodexOAuthError::IoError(err.to_string())
    }
}

/// OpenAI Device Code 响应
#[derive(Debug, Clone, Deserialize)]
struct DeviceCodeResponse {
    device_auth_id: String,
    user_code: String,
    #[serde(default)]
    interval: Option<serde_json::Value>,
    #[serde(default)]
    expires_in: Option<u64>,
}

/// OpenAI Device Code 轮询响应（成功）
#[derive(Debug, Clone, Deserialize)]
struct DevicePollSuccess {
    authorization_code: String,
    code_verifier: String,
}

/// OAuth Token 响应
#[derive(Debug, Clone, Deserialize)]
struct OAuthTokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
}

/// 解析后的 JWT claims（仅关心 chatgpt_account_id 等字段）
#[derive(Debug, Clone, Default, Deserialize)]
struct IdTokenClaims {
    #[serde(default)]
    chatgpt_account_id: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    organizations: Vec<OrgClaim>,
    #[serde(default, rename = "https://api.openai.com/auth")]
    openai_auth: Option<OpenAiAuthClaim>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct OrgClaim {
    #[serde(default)]
    id: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct OpenAiAuthClaim {
    #[serde(default)]
    chatgpt_account_id: Option<String>,
}

/// 缓存的 access_token（含过期时间）
#[derive(Debug, Clone)]
struct CachedAccessToken {
    token: String,
    /// 过期时间戳（毫秒）
    expires_at_ms: i64,
}

impl CachedAccessToken {
    fn is_expiring_soon(&self) -> bool {
        let now = chrono::Utc::now().timestamp_millis();
        self.expires_at_ms - now < TOKEN_REFRESH_BUFFER_MS
    }
}

/// 进行中的 Device Code 条目，带过期时间以便清理放弃的登录流程
#[derive(Debug, Clone)]
struct PendingDeviceCode {
    user_code: String,
    /// Unix 毫秒时间戳，超时后可清理
    expires_at_ms: i64,
}

/// 持久化的账号数据
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CodexAccountData {
    /// chatgpt_account_id（同时作为 HashMap 的 key）
    pub account_id: String,
    /// 账号邮箱（如果可获取）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// Refresh Token（持久化）
    pub refresh_token: String,
    /// 认证时间戳（秒）
    pub authenticated_at: i64,
    /// True when the user has explicitly exported this session and handed
    /// authoritative refresh ownership to a downstream consumer. While set,
    /// `get_valid_token_for_account` returns an error rather than refreshing
    /// (which would race with the downstream's rotation and silently poison
    /// either side's `refresh_token`). Default false; back-compat aware via
    /// `serde(default)` so existing on-disk files load unchanged.
    #[serde(default, skip_serializing_if = "is_false")]
    pub handed_off: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// 公开的账号信息（返回给前端，复用 GitHubAccount 结构）
impl From<&CodexAccountData> for GitHubAccount {
    fn from(data: &CodexAccountData) -> Self {
        GitHubAccount {
            id: data.account_id.clone(),
            // 用 email 作为显示名（若无则用 account_id）
            login: data
                .email
                .clone()
                .unwrap_or_else(|| format!("ChatGPT ({})", &data.account_id)),
            email: data.email.clone(),
            avatar_url: None,
            authenticated_at: data.authenticated_at,
            github_domain: "github.com".to_string(),
        }
    }
}

/// 持久化存储结构（v1）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct CodexOAuthStore {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    accounts: HashMap<String, CodexAccountData>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default_account_id: Option<String>,
}

/// Codex OAuth 认证管理器（多账号）
pub struct CodexOAuthManager {
    accounts: Arc<RwLock<HashMap<String, CodexAccountData>>>,
    default_account_id: Arc<RwLock<Option<String>>>,
    /// 内存缓存的 access_token（不持久化）
    access_tokens: Arc<RwLock<HashMap<String, CachedAccessToken>>>,
    /// 每个账号的刷新锁
    refresh_locks: Arc<RwLock<HashMap<String, Arc<Mutex<()>>>>>,
    /// 进行中的 Device Code 流程：device_auth_id -> {user_code, expires_at_ms}
    /// 过期条目会在 start_device_flow 时被清理，防止放弃的登录流程导致无界增长
    pending_device_codes: Arc<RwLock<HashMap<String, PendingDeviceCode>>>,
    http_client: Client,
    storage_path: PathBuf,
}

impl CodexOAuthManager {
    pub fn new(data_dir: PathBuf) -> Self {
        let storage_path = data_dir.join("codex_oauth_auth.json");

        let manager = Self {
            accounts: Arc::new(RwLock::new(HashMap::new())),
            default_account_id: Arc::new(RwLock::new(None)),
            access_tokens: Arc::new(RwLock::new(HashMap::new())),
            refresh_locks: Arc::new(RwLock::new(HashMap::new())),
            pending_device_codes: Arc::new(RwLock::new(HashMap::new())),
            http_client: Client::new(),
            storage_path,
        };

        if let Err(e) = manager.load_from_disk_sync() {
            log::warn!("[CodexOAuth] 加载存储失败: {e}");
        }

        manager
    }

    // ==================== 设备码流程 ====================

    /// 启动 Device Code 流程
    ///
    /// 返回 GitHubDeviceCodeResponse 复用现有前端结构，但字段含义对应 OpenAI 的字段：
    /// - device_code = device_auth_id
    /// - user_code = user_code
    /// - verification_uri = https://auth.openai.com/codex/device
    pub async fn start_device_flow(&self) -> Result<GitHubDeviceCodeResponse, CodexOAuthError> {
        log::info!("[CodexOAuth] 启动 Device Code 流程");

        let response = self
            .http_client
            .post(DEVICE_AUTH_USERCODE_URL)
            .header("Content-Type", "application/json")
            .header("User-Agent", CODEX_USER_AGENT)
            .json(&serde_json::json!({ "client_id": CODEX_CLIENT_ID }))
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(CodexOAuthError::NetworkError(format!(
                "Device Code 请求失败: {status} - {text}"
            )));
        }

        let device: DeviceCodeResponse = response
            .json()
            .await
            .map_err(|e| CodexOAuthError::ParseError(e.to_string()))?;

        let interval = parse_interval(device.interval.as_ref());
        let expires_in = device.expires_in.unwrap_or(DEVICE_CODE_DEFAULT_EXPIRES_IN);
        let expires_at_ms = chrono::Utc::now().timestamp_millis() + (expires_in as i64) * 1000;

        // 记录 device_auth_id -> 用户码映射；同时清理所有已过期的条目，
        // 避免用户放弃登录流程导致 HashMap 无界增长
        {
            let mut pending = self.pending_device_codes.write().await;
            let now_ms = chrono::Utc::now().timestamp_millis();
            pending.retain(|_, entry| entry.expires_at_ms > now_ms);
            pending.insert(
                device.device_auth_id.clone(),
                PendingDeviceCode {
                    user_code: device.user_code.clone(),
                    expires_at_ms,
                },
            );
        }

        log::info!(
            "[CodexOAuth] 获取 Device Code 成功，user_code: {}",
            device.user_code
        );

        Ok(GitHubDeviceCodeResponse {
            device_code: device.device_auth_id,
            user_code: device.user_code,
            verification_uri: DEVICE_VERIFICATION_URL.to_string(),
            expires_in,
            interval,
        })
    }

    /// 轮询 Device Code 状态
    ///
    /// 接收 device_code（即 device_auth_id），返回 Some(account) 表示授权成功
    pub async fn poll_for_token(
        &self,
        device_code: &str,
    ) -> Result<Option<GitHubAccount>, CodexOAuthError> {
        let entry = {
            let pending = self.pending_device_codes.read().await;
            pending.get(device_code).cloned()
        };

        let entry = entry.ok_or_else(|| {
            CodexOAuthError::TokenFetchFailed(
                "未找到对应的 user_code，请重新启动登录流程".to_string(),
            )
        })?;

        if entry.expires_at_ms <= chrono::Utc::now().timestamp_millis() {
            let mut pending = self.pending_device_codes.write().await;
            pending.remove(device_code);
            return Err(CodexOAuthError::ExpiredToken);
        }

        let user_code = entry.user_code;

        log::debug!("[CodexOAuth] 轮询 Device Code");

        let poll_response = self
            .http_client
            .post(DEVICE_AUTH_TOKEN_URL)
            .header("Content-Type", "application/json")
            .header("User-Agent", CODEX_USER_AGENT)
            .json(&serde_json::json!({
                "device_auth_id": device_code,
                "user_code": user_code,
            }))
            .send()
            .await?;

        let status = poll_response.status();

        // 403/404 表示用户未完成授权，继续轮询
        if status == reqwest::StatusCode::FORBIDDEN || status == reqwest::StatusCode::NOT_FOUND {
            return Err(CodexOAuthError::AuthorizationPending);
        }

        if status == reqwest::StatusCode::GONE {
            return Err(CodexOAuthError::ExpiredToken);
        }

        if !status.is_success() {
            let text = poll_response.text().await.unwrap_or_default();
            return Err(CodexOAuthError::TokenFetchFailed(format!(
                "{status} - {text}"
            )));
        }

        let success: DevicePollSuccess = poll_response
            .json()
            .await
            .map_err(|e| CodexOAuthError::ParseError(e.to_string()))?;

        log::info!("[CodexOAuth] 用户已授权，正在换取 OAuth Token");

        // 用 authorization_code + code_verifier 换 token
        let tokens = self
            .exchange_code_for_tokens(&success.authorization_code, &success.code_verifier)
            .await?;

        // 清理 pending device code
        {
            let mut pending = self.pending_device_codes.write().await;
            pending.remove(device_code);
        }

        let refresh_token = tokens.refresh_token.clone().ok_or_else(|| {
            CodexOAuthError::TokenFetchFailed("响应缺少 refresh_token".to_string())
        })?;

        let (account_id, email) = extract_identity_from_tokens(&tokens);
        let account_id = account_id.ok_or_else(|| {
            CodexOAuthError::ParseError("无法从 token 中提取 account_id".to_string())
        })?;

        // 缓存 access_token
        {
            let mut tokens_cache = self.access_tokens.write().await;
            tokens_cache.insert(
                account_id.clone(),
                CachedAccessToken {
                    token: tokens.access_token.clone(),
                    expires_at_ms: compute_expires_at_ms(tokens.expires_in),
                },
            );
        }

        let account = self
            .add_account_internal(account_id, refresh_token, email)
            .await?;

        Ok(Some(account))
    }

    /// 用 authorization_code + code_verifier 换取 tokens
    async fn exchange_code_for_tokens(
        &self,
        code: &str,
        code_verifier: &str,
    ) -> Result<OAuthTokenResponse, CodexOAuthError> {
        let response = self
            .http_client
            .post(OAUTH_TOKEN_URL)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("User-Agent", CODEX_USER_AGENT)
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", DEVICE_REDIRECT_URI),
                ("client_id", CODEX_CLIENT_ID),
                ("code_verifier", code_verifier),
            ])
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(CodexOAuthError::TokenFetchFailed(format!(
                "Token 交换失败: {status} - {text}"
            )));
        }

        response
            .json()
            .await
            .map_err(|e| CodexOAuthError::ParseError(e.to_string()))
    }

    /// 用 refresh_token 刷新 access_token
    async fn refresh_with_token(
        &self,
        refresh_token: &str,
    ) -> Result<OAuthTokenResponse, CodexOAuthError> {
        let response = self
            .http_client
            .post(OAUTH_TOKEN_URL)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("User-Agent", CODEX_USER_AGENT)
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
                ("client_id", CODEX_CLIENT_ID),
                ("scope", "openid profile email"),
            ])
            .send()
            .await?;

        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(CodexOAuthError::RefreshTokenInvalid);
        }

        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(CodexOAuthError::TokenFetchFailed(format!(
                "Refresh 失败: {status} - {text}"
            )));
        }

        response
            .json()
            .await
            .map_err(|e| CodexOAuthError::ParseError(e.to_string()))
    }

    // ==================== Token 获取（含自动刷新） ====================

    /// 获取指定账号的有效 access_token（必要时自动刷新）
    pub async fn get_valid_token_for_account(
        &self,
        account_id: &str,
    ) -> Result<String, CodexOAuthError> {
        // 先检查缓存
        {
            let tokens = self.access_tokens.read().await;
            if let Some(cached) = tokens.get(account_id) {
                if !cached.is_expiring_soon() {
                    return Ok(cached.token.clone());
                }
            }
        }

        log::info!("[CodexOAuth] 账号 {account_id} 的 access_token 需要刷新");

        let refresh_lock = self.get_refresh_lock(account_id).await;
        let _guard = refresh_lock.lock().await;

        // double-check
        {
            let tokens = self.access_tokens.read().await;
            if let Some(cached) = tokens.get(account_id) {
                if !cached.is_expiring_soon() {
                    return Ok(cached.token.clone());
                }
            }
        }

        let refresh_token = {
            let accounts = self.accounts.read().await;
            let account = accounts
                .get(account_id)
                .ok_or_else(|| CodexOAuthError::AccountNotFound(account_id.to_string()))?;
            // Handed-off accounts must not refresh — another consumer owns the
            // refresh_token now, and concurrent rotation would silently break
            // both sides. The user gets a clear error to resume management
            // explicitly when they want this account back.
            if account.handed_off {
                return Err(CodexOAuthError::AccountHandedOff);
            }
            account.refresh_token.clone()
        };

        let new_tokens = self.refresh_with_token(&refresh_token).await?;

        // 如果服务端返回了新的 refresh_token，更新存储
        if let Some(new_refresh) = new_tokens.refresh_token.clone() {
            if new_refresh != refresh_token {
                let mut accounts = self.accounts.write().await;
                if let Some(account) = accounts.get_mut(account_id) {
                    account.refresh_token = new_refresh;
                }
                drop(accounts);
                self.save_to_disk().await?;
            }
        }

        let access_token = new_tokens.access_token.clone();
        let expires_at_ms = compute_expires_at_ms(new_tokens.expires_in);

        {
            let mut tokens = self.access_tokens.write().await;
            tokens.insert(
                account_id.to_string(),
                CachedAccessToken {
                    token: access_token.clone(),
                    expires_at_ms,
                },
            );
        }

        Ok(access_token)
    }

    /// 获取默认账号的有效 token
    pub async fn get_valid_token(&self) -> Result<String, CodexOAuthError> {
        match self.resolve_default_account_id().await {
            Some(id) => self.get_valid_token_for_account(&id).await,
            None => Err(CodexOAuthError::AccountNotFound(
                "无可用的 ChatGPT 账号".to_string(),
            )),
        }
    }

    /// 获取默认账号 ID（热路径使用，避免克隆整个账号 HashMap）
    pub async fn default_account_id(&self) -> Option<String> {
        self.resolve_default_account_id().await
    }

    // ==================== 多账号管理 ====================

    /// Export a managed account back to the canonical session shape, with the
    /// freshest access_token cc-switch can produce.
    ///
    /// When `refresh_first=true`, calls `get_valid_token_for_account` which
    /// refreshes if the cached token is expiring soon (the manager's normal
    /// 60-second buffer applies) and records any rotated refresh_token to
    /// disk before returning. When `false`, uses the cached token as-is —
    /// useful for "give me whatever you have, even if stale" debug exports.
    ///
    /// The returned canonical carries `account_id`, `refresh_token`, `email`,
    /// `exp` (parsed from the fresh access_token's JWT payload), and `source =
    /// CcSwitch` so re-imports round-trip cleanly.
    pub async fn export_account(
        &self,
        account_id: &str,
        refresh_first: bool,
    ) -> Result<crate::proxy::providers::codex_oauth_session::CanonicalCodexSession, CodexOAuthError>
    {
        use crate::proxy::providers::codex_oauth_session::{
            decode_jwt_exp, CanonicalCodexSession, CodexSessionSource,
        };

        let account_id = account_id.trim();
        if account_id.is_empty() {
            return Err(CodexOAuthError::AccountNotFound("empty".to_string()));
        }

        // Snapshot the persisted account fields up-front. `get_valid_token_for_account`
        // may rotate refresh_token under us, so we re-read after the refresh
        // returns to capture the rotated value.
        let (mut refresh_token, email) = {
            let accounts = self.accounts.read().await;
            let account = accounts
                .get(account_id)
                .ok_or_else(|| CodexOAuthError::AccountNotFound(account_id.to_string()))?;
            (account.refresh_token.clone(), account.email.clone())
        };

        let access_token = if refresh_first {
            self.get_valid_token_for_account(account_id).await?
        } else {
            let cached = self.access_tokens.read().await;
            cached
                .get(account_id)
                .map(|c| c.token.clone())
                .ok_or_else(|| {
                    CodexOAuthError::TokenFetchFailed(
                        "no cached access_token; call with refresh_first=true".to_string(),
                    )
                })?
        };

        // Re-snapshot refresh_token after a possible refresh-time rotation so
        // the exported file ships the value the next consumer should use.
        if refresh_first {
            let accounts = self.accounts.read().await;
            if let Some(account) = accounts.get(account_id) {
                refresh_token = account.refresh_token.clone();
            }
        }

        let exp = decode_jwt_exp(&access_token);

        Ok(CanonicalCodexSession {
            access_token,
            refresh_token: Some(refresh_token),
            id_token: None,
            account_id: Some(account_id.to_string()),
            user_id: None,
            email,
            plan_type: None,
            organization_id: None,
            exp,
            last_refresh: Some(chrono::Utc::now().timestamp()),
            source: CodexSessionSource::CcSwitch,
            extras: Default::default(),
        })
    }

    pub async fn list_accounts(&self) -> Vec<GitHubAccount> {
        let accounts = self.accounts.read().await.clone();
        let default_id = self.resolve_default_account_id().await;
        Self::sorted_accounts(&accounts, default_id.as_deref())
    }

    /// Import a canonical Codex session (parsed from any of the supported
    /// external formats) into the managed account pool.
    ///
    /// The session is added (or, when `update_existing` is true, updated)
    /// keyed by `chatgpt_account_id`. The provided `access_token` is also
    /// seeded into the in-memory cache with the parsed `exp` so callers that
    /// hit `get_valid_token_for_account` immediately after importing can skip
    /// the first refresh round-trip.
    ///
    /// **Contract**: the canonical session must carry a `refresh_token` and
    /// an `account_id`. Imports without a refresh_token cannot participate in
    /// auto-refresh (cc-switch's deployment runs no Codex CLI), so we reject
    /// them at this layer; the command layer maps that to a per-row warning.
    /// Imports without an `account_id` are rejected for the same reason — the
    /// account_id is the primary identity key.
    pub async fn import_canonical_session(
        &self,
        session: &crate::proxy::providers::codex_oauth_session::CanonicalCodexSession,
        update_existing: bool,
    ) -> Result<CodexImportOutcome, CodexOAuthError> {
        let outcome = self.import_one_no_save(session, update_existing).await?;
        // Only the create/update branches mutate persisted state; a Skipped
        // outcome touched nothing on disk so we can avoid the write.
        if !matches!(outcome.action, CodexImportAction::Skipped) {
            self.save_to_disk().await?;
        }
        Ok(outcome)
    }

    /// Per-row variant of `import_canonical_session` that DOES NOT persist
    /// to disk. Use when running a batch import where the caller wants to
    /// fsync once at the end via `persist_imports`, turning the previous
    /// O(N) atomic-write cost into O(1).
    ///
    /// In-memory state is updated immediately; if the process crashes before
    /// `persist_imports` runs the batch is lost — same durability semantics
    /// as the rest of the manager between writes.
    pub async fn import_canonical_session_without_persist(
        &self,
        session: &crate::proxy::providers::codex_oauth_session::CanonicalCodexSession,
        update_existing: bool,
    ) -> Result<CodexImportOutcome, CodexOAuthError> {
        self.import_one_no_save(session, update_existing).await
    }

    /// Persist the in-memory store to disk. Pair with
    /// `import_canonical_session_without_persist` at the end of a batch.
    pub async fn persist_imports(&self) -> Result<(), CodexOAuthError> {
        self.save_to_disk().await
    }

    /// Batched analogue of `import_canonical_session`. Runs the per-row logic
    /// over the slice, collecting per-row `Result`s, and persists ONCE at the
    /// end — turning the previous O(N) `save_to_disk` calls (each a full
    /// atomic JSON write) into O(1). Callers get the same outcomes they would
    /// have gotten from sequential single-item imports.
    ///
    /// Save is skipped when no row produced a Created or Updated outcome —
    /// useful when a paste was 100% duplicates or 100% structurally invalid.
    pub async fn import_canonical_sessions(
        &self,
        sessions: &[(
            crate::proxy::providers::codex_oauth_session::CanonicalCodexSession,
            bool,
        )],
    ) -> Vec<Result<CodexImportOutcome, CodexOAuthError>> {
        let mut outcomes = Vec::with_capacity(sessions.len());
        let mut dirty = false;
        for (session, update_existing) in sessions {
            let outcome = self.import_one_no_save(session, *update_existing).await;
            if let Ok(o) = &outcome {
                if !matches!(o.action, CodexImportAction::Skipped) {
                    dirty = true;
                }
            }
            outcomes.push(outcome);
        }
        if dirty {
            // Single fsync for the whole batch. If this fails we still report
            // the per-row outcomes — the user can retry, the in-memory state
            // is already authoritative until next process restart.
            if let Err(err) = self.save_to_disk().await {
                outcomes.push(Err(err));
            }
        }
        outcomes
    }

    async fn import_one_no_save(
        &self,
        session: &crate::proxy::providers::codex_oauth_session::CanonicalCodexSession,
        update_existing: bool,
    ) -> Result<CodexImportOutcome, CodexOAuthError> {
        let refresh_token = session
            .refresh_token
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                CodexOAuthError::TokenFetchFailed(
                    "缺少 refresh_token，无法纳入自动续期".to_string(),
                )
            })?
            .to_string();
        let account_id = session
            .account_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                CodexOAuthError::ParseError("无法从 token 中提取 chatgpt_account_id".to_string())
            })?
            .to_string();

        // Determine whether this is a fresh insert or an existing-account update.
        let already_existed = self.accounts.read().await.contains_key(&account_id);
        if already_existed && !update_existing {
            let accounts = self.accounts.read().await;
            let account = accounts
                .get(&account_id)
                .map(GitHubAccount::from)
                .expect("contains_key just returned true");
            return Ok(CodexImportOutcome {
                account,
                action: CodexImportAction::Skipped,
            });
        }

        let now = chrono::Utc::now().timestamp();
        let data = CodexAccountData {
            account_id: account_id.clone(),
            email: session.email.clone(),
            refresh_token,
            authenticated_at: now,
            handed_off: false,
        };
        let account = GitHubAccount::from(&data);

        {
            let mut accounts = self.accounts.write().await;
            accounts.insert(account_id.clone(), data);
        }
        {
            let mut default = self.default_account_id.write().await;
            if default.is_none() {
                *default = Some(account_id.clone());
            }
        }

        let access_token = session.access_token.trim();
        if !access_token.is_empty() {
            let now_ms = chrono::Utc::now().timestamp_millis();
            let expires_at_ms = session
                .exp
                .map(|secs| secs * 1000)
                .unwrap_or(now_ms + 60_000);
            if expires_at_ms > now_ms {
                let mut tokens = self.access_tokens.write().await;
                tokens.insert(
                    account_id.clone(),
                    CachedAccessToken {
                        token: access_token.to_string(),
                        expires_at_ms,
                    },
                );
            }
        }

        let action = if already_existed {
            CodexImportAction::Updated
        } else {
            CodexImportAction::Created
        };
        Ok(CodexImportOutcome { account, action })
    }

    /// 作废指定账号的 access_token 缓存。
    ///
    /// 用于上游返回 401 时，由 forwarder 触发，使下一次 `get_valid_token_for_account`
    /// 走 refresh 分支去拿新 token。不动 refresh_token。
    pub async fn invalidate_cached_token(&self, account_id: &str) {
        let mut tokens = self.access_tokens.write().await;
        if tokens.remove(account_id).is_some() {
            log::info!("[CodexOAuth] 已作废 access_token 缓存 (account={account_id})");
        }
    }

    pub async fn remove_account(&self, account_id: &str) -> Result<(), CodexOAuthError> {
        log::info!("[CodexOAuth] 移除账号: {account_id}");

        {
            let mut accounts = self.accounts.write().await;
            if accounts.remove(account_id).is_none() {
                return Err(CodexOAuthError::AccountNotFound(account_id.to_string()));
            }
        }

        {
            let mut tokens = self.access_tokens.write().await;
            tokens.remove(account_id);
        }
        {
            let mut locks = self.refresh_locks.write().await;
            locks.remove(account_id);
        }

        {
            let accounts = self.accounts.read().await;
            let mut default = self.default_account_id.write().await;
            if default.as_deref() == Some(account_id) {
                *default = Self::fallback_default_account_id(&accounts);
            }
        }

        self.save_to_disk().await?;
        Ok(())
    }

    pub async fn set_default_account(&self, account_id: &str) -> Result<(), CodexOAuthError> {
        {
            let accounts = self.accounts.read().await;
            if !accounts.contains_key(account_id) {
                return Err(CodexOAuthError::AccountNotFound(account_id.to_string()));
            }
        }

        {
            let mut default = self.default_account_id.write().await;
            *default = Some(account_id.to_string());
        }

        self.save_to_disk().await?;
        Ok(())
    }

    /// Mark an account as "handed off": after this call `get_valid_token_for_account`
    /// returns `AccountHandedOff` instead of refreshing. Use after exporting a
    /// session to a downstream that will own refresh from now on, to prevent
    /// concurrent rotation from invalidating both sides' refresh_token.
    pub async fn mark_account_handoff(&self, account_id: &str) -> Result<(), CodexOAuthError> {
        let account_id = account_id.trim();
        let mut accounts = self.accounts.write().await;
        let account = accounts
            .get_mut(account_id)
            .ok_or_else(|| CodexOAuthError::AccountNotFound(account_id.to_string()))?;
        if account.handed_off {
            return Ok(());
        }
        account.handed_off = true;
        drop(accounts);

        // Drop the now-stale cached access_token so reads see the handoff
        // immediately rather than serving stale cache until natural expiry.
        let mut tokens = self.access_tokens.write().await;
        tokens.remove(account_id);
        drop(tokens);

        self.save_to_disk().await
    }

    /// Reverse `mark_account_handoff`. Subsequent `get_valid_token_for_account`
    /// will refresh again. Note this does NOT verify the refresh_token is still
    /// valid; if the downstream consumer rotated it, the next refresh will fail
    /// with `RefreshTokenInvalid` and the user will have to re-import.
    pub async fn restore_account_management(
        &self,
        account_id: &str,
    ) -> Result<(), CodexOAuthError> {
        let account_id = account_id.trim();
        let mut accounts = self.accounts.write().await;
        let account = accounts
            .get_mut(account_id)
            .ok_or_else(|| CodexOAuthError::AccountNotFound(account_id.to_string()))?;
        if !account.handed_off {
            return Ok(());
        }
        account.handed_off = false;
        drop(accounts);
        self.save_to_disk().await
    }

    /// Whether the given account is currently in the handed-off state. Used by
    /// command-layer code to surface badge state to the UI without round-tripping
    /// through `get_valid_token_for_account`.
    pub async fn is_handed_off(&self, account_id: &str) -> bool {
        self.accounts
            .read()
            .await
            .get(account_id.trim())
            .map(|a| a.handed_off)
            .unwrap_or(false)
    }

    pub async fn clear_auth(&self) -> Result<(), CodexOAuthError> {
        log::info!("[CodexOAuth] 清除所有认证");

        {
            let mut accounts = self.accounts.write().await;
            accounts.clear();
        }
        {
            let mut default = self.default_account_id.write().await;
            *default = None;
        }
        {
            let mut tokens = self.access_tokens.write().await;
            tokens.clear();
        }
        {
            let mut locks = self.refresh_locks.write().await;
            locks.clear();
        }
        {
            let mut pending = self.pending_device_codes.write().await;
            pending.clear();
        }

        if self.storage_path.exists() {
            std::fs::remove_file(&self.storage_path)?;
        }

        Ok(())
    }

    pub async fn is_authenticated(&self) -> bool {
        let accounts = self.accounts.read().await;
        !accounts.is_empty()
    }

    /// 获取认证状态摘要（与 Copilot 的格式保持一致，便于复用前端）
    pub async fn get_status(&self) -> CodexOAuthStatus {
        let accounts_map = self.accounts.read().await.clone();
        let default_id = self.resolve_default_account_id().await;
        let account_list = Self::sorted_accounts(&accounts_map, default_id.as_deref());
        let authenticated = !account_list.is_empty();
        let username = default_id
            .as_ref()
            .and_then(|id| accounts_map.get(id))
            .and_then(|a| a.email.clone())
            .or_else(|| account_list.first().map(|a| a.login.clone()));

        CodexOAuthStatus {
            accounts: account_list,
            default_account_id: default_id,
            authenticated,
            username,
        }
    }

    // ==================== 内部方法 ====================

    async fn add_account_internal(
        &self,
        account_id: String,
        refresh_token: String,
        email: Option<String>,
    ) -> Result<GitHubAccount, CodexOAuthError> {
        let now = chrono::Utc::now().timestamp();

        let data = CodexAccountData {
            account_id: account_id.clone(),
            email,
            refresh_token,
            authenticated_at: now,
            handed_off: false,
        };

        let account = GitHubAccount::from(&data);

        {
            let mut accounts = self.accounts.write().await;
            accounts.insert(account_id.clone(), data);
        }

        {
            let mut default = self.default_account_id.write().await;
            if default.is_none() {
                *default = Some(account_id);
            }
        }

        self.save_to_disk().await?;
        Ok(account)
    }

    fn fallback_default_account_id(accounts: &HashMap<String, CodexAccountData>) -> Option<String> {
        accounts
            .iter()
            .max_by(|(id_a, a), (id_b, b)| {
                a.authenticated_at
                    .cmp(&b.authenticated_at)
                    .then_with(|| id_b.cmp(id_a))
            })
            .map(|(id, _)| id.clone())
    }

    fn sorted_accounts(
        accounts: &HashMap<String, CodexAccountData>,
        default_account_id: Option<&str>,
    ) -> Vec<GitHubAccount> {
        let mut list: Vec<GitHubAccount> = accounts.values().map(GitHubAccount::from).collect();
        list.sort_by(|a, b| {
            let a_default = default_account_id == Some(a.id.as_str());
            let b_default = default_account_id == Some(b.id.as_str());
            b_default
                .cmp(&a_default)
                .then_with(|| b.authenticated_at.cmp(&a.authenticated_at))
                .then_with(|| a.login.cmp(&b.login))
        });
        list
    }

    async fn resolve_default_account_id(&self) -> Option<String> {
        let stored = self.default_account_id.read().await.clone();
        let accounts = self.accounts.read().await;

        if let Some(id) = stored {
            if accounts.contains_key(&id) {
                return Some(id);
            }
        }

        Self::fallback_default_account_id(&accounts)
    }

    async fn get_refresh_lock(&self, account_id: &str) -> Arc<Mutex<()>> {
        {
            let locks = self.refresh_locks.read().await;
            if let Some(lock) = locks.get(account_id) {
                return Arc::clone(lock);
            }
        }

        let mut locks = self.refresh_locks.write().await;
        Arc::clone(
            locks
                .entry(account_id.to_string())
                .or_insert_with(|| Arc::new(Mutex::new(()))),
        )
    }

    fn write_store_atomic(&self, content: &str) -> Result<(), CodexOAuthError> {
        if let Some(parent) = self.storage_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let parent = self
            .storage_path
            .parent()
            .ok_or_else(|| CodexOAuthError::IoError("无效的存储路径".to_string()))?;
        let file_name = self
            .storage_path
            .file_name()
            .ok_or_else(|| CodexOAuthError::IoError("无效的存储文件名".to_string()))?
            .to_string_lossy()
            .to_string();
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let tmp_path = parent.join(format!("{file_name}.tmp.{ts}"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

            let mut file = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(&tmp_path)?;
            file.write_all(content.as_bytes())?;
            file.flush()?;

            fs::rename(&tmp_path, &self.storage_path)?;
            fs::set_permissions(&self.storage_path, fs::Permissions::from_mode(0o600))?;
        }

        #[cfg(windows)]
        {
            let mut file = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&tmp_path)?;
            file.write_all(content.as_bytes())?;
            file.flush()?;

            if self.storage_path.exists() {
                let _ = fs::remove_file(&self.storage_path);
            }
            fs::rename(&tmp_path, &self.storage_path)?;
        }

        Ok(())
    }

    fn load_from_disk_sync(&self) -> Result<(), CodexOAuthError> {
        if !self.storage_path.exists() {
            return Ok(());
        }

        let content = std::fs::read_to_string(&self.storage_path)?;
        let store: CodexOAuthStore = serde_json::from_str(&content)
            .map_err(|e| CodexOAuthError::ParseError(e.to_string()))?;

        if let Ok(mut accounts) = self.accounts.try_write() {
            *accounts = store.accounts;
            log::info!("[CodexOAuth] 从磁盘加载 {} 个账号", accounts.len());
        }
        if let Ok(mut default) = self.default_account_id.try_write() {
            *default = store.default_account_id;
            if default.is_none() {
                if let Ok(accounts) = self.accounts.try_read() {
                    *default = Self::fallback_default_account_id(&accounts);
                }
            }
        }

        Ok(())
    }

    async fn save_to_disk(&self) -> Result<(), CodexOAuthError> {
        let accounts = self.accounts.read().await.clone();
        let default = self.resolve_default_account_id().await;

        let store = CodexOAuthStore {
            version: 1,
            accounts,
            default_account_id: default,
        };

        let content = serde_json::to_string_pretty(&store)
            .map_err(|e| CodexOAuthError::ParseError(e.to_string()))?;

        self.write_store_atomic(&content)?;

        log::info!(
            "[CodexOAuth] 保存到磁盘成功（{} 个账号）",
            store.accounts.len()
        );

        Ok(())
    }
}

/// Codex OAuth 状态摘要
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexOAuthStatus {
    pub accounts: Vec<GitHubAccount>,
    pub default_account_id: Option<String>,
    pub authenticated: bool,
    pub username: Option<String>,
}

/// Result of importing one canonical session via `import_canonical_session`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexImportOutcome {
    pub account: GitHubAccount,
    pub action: CodexImportAction,
}

/// Whether the import created a new account, updated an existing one, or was
/// skipped because the account already existed and `update_existing=false`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodexImportAction {
    Created,
    Updated,
    Skipped,
}

// ==================== 工具函数 ====================

/// 解析 OpenAI Device Code 响应中的 interval 字段
///
/// 服务端可能返回字符串或数字，需要兼容
fn parse_interval(value: Option<&serde_json::Value>) -> u64 {
    let raw = match value {
        Some(serde_json::Value::Number(n)) => n.as_u64().unwrap_or(5),
        Some(serde_json::Value::String(s)) => s.parse::<u64>().unwrap_or(5),
        _ => 5,
    };
    raw.max(1) + POLLING_SAFETY_MARGIN_SECS
}

/// 从 expires_in（秒）计算过期时间戳（毫秒）
fn compute_expires_at_ms(expires_in: Option<i64>) -> i64 {
    let now_ms = chrono::Utc::now().timestamp_millis();
    let secs = expires_in.unwrap_or(3600);
    now_ms + secs * 1000
}

/// 解析 JWT 中的 claims
fn parse_jwt_claims(token: &str) -> Option<IdTokenClaims> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let decoded = URL_SAFE_NO_PAD.decode(parts[1]).ok()?;
    serde_json::from_slice(&decoded).ok()
}

/// 从 token 响应中提取 (account_id, email)
fn extract_identity_from_tokens(tokens: &OAuthTokenResponse) -> (Option<String>, Option<String>) {
    let mut account_id: Option<String> = None;
    let mut email: Option<String> = None;

    if let Some(id_token) = tokens.id_token.as_deref() {
        if let Some(claims) = parse_jwt_claims(id_token) {
            account_id = claims
                .chatgpt_account_id
                .clone()
                .or_else(|| {
                    claims
                        .openai_auth
                        .as_ref()
                        .and_then(|a| a.chatgpt_account_id.clone())
                })
                .or_else(|| claims.organizations.first().and_then(|o| o.id.clone()));
            email = claims.email.clone();
        }
    }

    if account_id.is_none() {
        if let Some(claims) = parse_jwt_claims(&tokens.access_token) {
            account_id = claims
                .chatgpt_account_id
                .clone()
                .or_else(|| {
                    claims
                        .openai_auth
                        .as_ref()
                        .and_then(|a| a.chatgpt_account_id.clone())
                })
                .or_else(|| claims.organizations.first().and_then(|o| o.id.clone()));
            if email.is_none() {
                email = claims.email.clone();
            }
        }
    }

    (account_id, email)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_interval_number() {
        let v = serde_json::Value::Number(serde_json::Number::from(5));
        assert_eq!(parse_interval(Some(&v)), 5 + POLLING_SAFETY_MARGIN_SECS);
    }

    #[test]
    fn test_parse_interval_string() {
        let v = serde_json::Value::String("10".to_string());
        assert_eq!(parse_interval(Some(&v)), 10 + POLLING_SAFETY_MARGIN_SECS);
    }

    #[test]
    fn test_parse_interval_default() {
        assert_eq!(parse_interval(None), 5 + POLLING_SAFETY_MARGIN_SECS);
    }

    #[test]
    fn test_parse_interval_min() {
        let v = serde_json::Value::Number(serde_json::Number::from(0));
        // 0 应被提升到 1
        assert_eq!(parse_interval(Some(&v)), 1 + POLLING_SAFETY_MARGIN_SECS);
    }

    #[test]
    fn test_compute_expires_at_ms() {
        let result = compute_expires_at_ms(Some(3600));
        let now = chrono::Utc::now().timestamp_millis();
        // 应在未来约 3600 秒处（允许少量误差）
        assert!(result > now + 3500 * 1000);
        assert!(result < now + 3700 * 1000);
    }

    #[test]
    fn test_compute_expires_at_ms_default() {
        let result = compute_expires_at_ms(None);
        let now = chrono::Utc::now().timestamp_millis();
        assert!(result > now);
    }

    #[test]
    fn test_cached_token_expiring_soon() {
        let now = chrono::Utc::now().timestamp_millis();
        // 30 秒后过期 - 在缓冲期内
        let expiring = CachedAccessToken {
            token: "t".to_string(),
            expires_at_ms: now + 30_000,
        };
        assert!(expiring.is_expiring_soon());

        // 1 小时后过期 - 不在缓冲期内
        let valid = CachedAccessToken {
            token: "t".to_string(),
            expires_at_ms: now + 3_600_000,
        };
        assert!(!valid.is_expiring_soon());
    }

    #[test]
    fn test_parse_jwt_claims_invalid() {
        assert!(parse_jwt_claims("not-a-jwt").is_none());
        assert!(parse_jwt_claims("only.two").is_none());
    }

    #[test]
    fn test_parse_jwt_claims_valid() {
        // Header: {"alg":"none"}
        // Payload: {"chatgpt_account_id":"acc-123","email":"test@example.com"}
        // Signature: empty
        let header = URL_SAFE_NO_PAD.encode(b"{\"alg\":\"none\"}");
        let payload = URL_SAFE_NO_PAD
            .encode(b"{\"chatgpt_account_id\":\"acc-123\",\"email\":\"test@example.com\"}");
        let jwt = format!("{header}.{payload}.");
        let claims = parse_jwt_claims(&jwt).unwrap();
        assert_eq!(claims.chatgpt_account_id.as_deref(), Some("acc-123"));
        assert_eq!(claims.email.as_deref(), Some("test@example.com"));
    }

    #[test]
    fn test_parse_jwt_claims_organizations_fallback() {
        let header = URL_SAFE_NO_PAD.encode(b"{\"alg\":\"none\"}");
        let payload = URL_SAFE_NO_PAD.encode(b"{\"organizations\":[{\"id\":\"org-456\"}]}");
        let jwt = format!("{header}.{payload}.");
        let claims = parse_jwt_claims(&jwt).unwrap();
        assert_eq!(
            claims
                .organizations
                .first()
                .and_then(|o| o.id.clone())
                .as_deref(),
            Some("org-456")
        );
    }

    #[tokio::test]
    async fn test_manager_initial_state() {
        let temp = tempfile::tempdir().unwrap();
        let manager = CodexOAuthManager::new(temp.path().to_path_buf());
        assert!(!manager.is_authenticated().await);
        assert!(manager.list_accounts().await.is_empty());
    }

    #[tokio::test]
    async fn test_manager_save_and_load() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().to_path_buf();

        // Manually inject an account through internal methods
        {
            let manager = CodexOAuthManager::new(path.clone());
            manager
                .add_account_internal(
                    "acc-123".to_string(),
                    "rt-secret".to_string(),
                    Some("user@example.com".to_string()),
                )
                .await
                .unwrap();
        }

        // New manager should load from disk
        let manager2 = CodexOAuthManager::new(path);
        let accounts = manager2.list_accounts().await;
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].id, "acc-123");
    }

    #[tokio::test]
    async fn test_remove_account() {
        let temp = tempfile::tempdir().unwrap();
        let manager = CodexOAuthManager::new(temp.path().to_path_buf());

        manager
            .add_account_internal(
                "acc-123".to_string(),
                "rt".to_string(),
                Some("a@example.com".to_string()),
            )
            .await
            .unwrap();
        manager
            .add_account_internal(
                "acc-456".to_string(),
                "rt2".to_string(),
                Some("b@example.com".to_string()),
            )
            .await
            .unwrap();

        manager.remove_account("acc-123").await.unwrap();
        let accounts = manager.list_accounts().await;
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].id, "acc-456");
    }

    #[tokio::test]
    async fn import_canonical_session_creates_then_dedups() {
        use crate::proxy::providers::codex_oauth_session::{
            CanonicalCodexSession, CodexSessionSource,
        };
        let temp = tempfile::tempdir().unwrap();
        let manager = CodexOAuthManager::new(temp.path().to_path_buf());

        let session = CanonicalCodexSession {
            access_token: "at-1".to_string(),
            refresh_token: Some("rt-1".to_string()),
            account_id: Some("acct-imp".to_string()),
            email: Some("imp@example.com".to_string()),
            exp: Some(chrono::Utc::now().timestamp() + 3_600),
            source: CodexSessionSource::CodexCli,
            ..Default::default()
        };

        // First insert: Created.
        let outcome = manager
            .import_canonical_session(&session, true)
            .await
            .unwrap();
        assert_eq!(outcome.action, CodexImportAction::Created);
        assert_eq!(outcome.account.id, "acct-imp");

        // access_token must be pre-seeded so we don't refresh on first read.
        let cached = manager.access_tokens.read().await;
        assert_eq!(
            cached.get("acct-imp").map(|c| c.token.as_str()),
            Some("at-1")
        );
        drop(cached);

        // Re-import without update_existing → Skipped, refresh_token unchanged.
        let mut session2 = session.clone();
        session2.refresh_token = Some("rt-NEW".to_string());
        let outcome = manager
            .import_canonical_session(&session2, false)
            .await
            .unwrap();
        assert_eq!(outcome.action, CodexImportAction::Skipped);
        let accounts = manager.accounts.read().await;
        assert_eq!(accounts.get("acct-imp").unwrap().refresh_token, "rt-1");
        drop(accounts);

        // Re-import with update_existing → Updated, refresh_token rotates.
        let outcome = manager
            .import_canonical_session(&session2, true)
            .await
            .unwrap();
        assert_eq!(outcome.action, CodexImportAction::Updated);
        let accounts = manager.accounts.read().await;
        assert_eq!(accounts.get("acct-imp").unwrap().refresh_token, "rt-NEW");
    }

    #[tokio::test]
    async fn import_canonical_session_rejects_missing_refresh_or_account() {
        use crate::proxy::providers::codex_oauth_session::CanonicalCodexSession;
        let temp = tempfile::tempdir().unwrap();
        let manager = CodexOAuthManager::new(temp.path().to_path_buf());

        let no_refresh = CanonicalCodexSession {
            access_token: "at".to_string(),
            account_id: Some("acct".to_string()),
            ..Default::default()
        };
        assert!(matches!(
            manager.import_canonical_session(&no_refresh, true).await,
            Err(CodexOAuthError::TokenFetchFailed(_))
        ));

        let no_account = CanonicalCodexSession {
            access_token: "at".to_string(),
            refresh_token: Some("rt".to_string()),
            ..Default::default()
        };
        assert!(matches!(
            manager.import_canonical_session(&no_account, true).await,
            Err(CodexOAuthError::ParseError(_))
        ));
    }

    #[tokio::test]
    async fn import_canonical_session_skips_cache_seed_for_expired_access_token() {
        use crate::proxy::providers::codex_oauth_session::CanonicalCodexSession;
        let temp = tempfile::tempdir().unwrap();
        let manager = CodexOAuthManager::new(temp.path().to_path_buf());

        let session = CanonicalCodexSession {
            access_token: "at-expired".to_string(),
            refresh_token: Some("rt".to_string()),
            account_id: Some("acct".to_string()),
            // Past exp by an hour
            exp: Some(chrono::Utc::now().timestamp() - 3_600),
            ..Default::default()
        };
        manager
            .import_canonical_session(&session, true)
            .await
            .unwrap();

        let cached = manager.access_tokens.read().await;
        assert!(
            cached.get("acct").is_none(),
            "expired access_token must not be cached — next request must refresh"
        );
    }

    #[tokio::test]
    async fn export_account_returns_canonical_with_cached_token() {
        use crate::proxy::providers::codex_oauth_session::{
            CanonicalCodexSession, CodexSessionSource,
        };
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};

        let temp = tempfile::tempdir().unwrap();
        let manager = CodexOAuthManager::new(temp.path().to_path_buf());

        let header = URL_SAFE_NO_PAD.encode(b"{\"alg\":\"none\"}");
        let future_exp = chrono::Utc::now().timestamp() + 7_200;
        let payload = URL_SAFE_NO_PAD.encode(format!("{{\"exp\":{future_exp}}}").as_bytes());
        let jwt = format!("{header}.{payload}.");

        let imported = CanonicalCodexSession {
            access_token: jwt.clone(),
            refresh_token: Some("rt-export".to_string()),
            account_id: Some("acct-exp".to_string()),
            email: Some("u@example.com".to_string()),
            exp: Some(future_exp),
            source: CodexSessionSource::CodexCli,
            ..Default::default()
        };
        manager
            .import_canonical_session(&imported, true)
            .await
            .unwrap();

        // refresh_first=false avoids the HTTP path and just returns cache.
        let exported = manager
            .export_account("acct-exp", false)
            .await
            .expect("cached token export should succeed");

        assert_eq!(exported.account_id.as_deref(), Some("acct-exp"));
        assert_eq!(exported.email.as_deref(), Some("u@example.com"));
        assert_eq!(exported.refresh_token.as_deref(), Some("rt-export"));
        assert_eq!(exported.access_token, jwt);
        assert_eq!(exported.exp, Some(future_exp));
        assert_eq!(exported.source, CodexSessionSource::CcSwitch);
    }

    #[tokio::test]
    async fn export_account_errors_when_account_missing() {
        let temp = tempfile::tempdir().unwrap();
        let manager = CodexOAuthManager::new(temp.path().to_path_buf());
        assert!(matches!(
            manager.export_account("does-not-exist", false).await,
            Err(CodexOAuthError::AccountNotFound(_))
        ));
        assert!(matches!(
            manager.export_account("   ", false).await,
            Err(CodexOAuthError::AccountNotFound(_))
        ));
    }

    #[tokio::test]
    async fn handoff_blocks_refresh_and_drops_cached_token() {
        use crate::proxy::providers::codex_oauth_session::{
            CanonicalCodexSession, CodexSessionSource,
        };
        let temp = tempfile::tempdir().unwrap();
        let manager = CodexOAuthManager::new(temp.path().to_path_buf());

        let session = CanonicalCodexSession {
            access_token: "at-handoff".to_string(),
            refresh_token: Some("rt".to_string()),
            account_id: Some("acct-h".to_string()),
            exp: Some(chrono::Utc::now().timestamp() + 3_600),
            source: CodexSessionSource::CodexCli,
            ..Default::default()
        };
        manager
            .import_canonical_session(&session, true)
            .await
            .unwrap();

        // Pre-condition: cached token present, not handed off.
        assert!(manager.access_tokens.read().await.contains_key("acct-h"));
        assert!(!manager.is_handed_off("acct-h").await);

        manager.mark_account_handoff("acct-h").await.unwrap();
        assert!(manager.is_handed_off("acct-h").await);
        // Cache cleared so reads can't serve a stale value past handoff.
        assert!(!manager.access_tokens.read().await.contains_key("acct-h"));

        // Refresh path refuses with AccountHandedOff rather than touching network.
        let err = manager
            .get_valid_token_for_account("acct-h")
            .await
            .unwrap_err();
        assert!(matches!(err, CodexOAuthError::AccountHandedOff));

        // Idempotent: a second handoff is a no-op.
        manager.mark_account_handoff("acct-h").await.unwrap();

        // Restore reverses the state; idempotent in the other direction too.
        manager.restore_account_management("acct-h").await.unwrap();
        assert!(!manager.is_handed_off("acct-h").await);
        manager.restore_account_management("acct-h").await.unwrap();
    }

    #[tokio::test]
    async fn batch_import_persists_once_at_end() {
        use crate::proxy::providers::codex_oauth_session::{
            CanonicalCodexSession, CodexSessionSource,
        };
        let temp = tempfile::tempdir().unwrap();
        let manager = CodexOAuthManager::new(temp.path().to_path_buf());

        let make = |id: &str| CanonicalCodexSession {
            access_token: format!("at-{id}"),
            refresh_token: Some(format!("rt-{id}")),
            account_id: Some(id.to_string()),
            email: Some(format!("{id}@example.com")),
            exp: Some(chrono::Utc::now().timestamp() + 3_600),
            source: CodexSessionSource::CodexCli,
            ..Default::default()
        };

        // Use the no-persist API for the loop, then a single persist call.
        for id in ["a", "b", "c"] {
            let session = make(id);
            let outcome = manager
                .import_canonical_session_without_persist(&session, true)
                .await
                .unwrap();
            assert_eq!(outcome.action, CodexImportAction::Created);
        }
        // Storage file should NOT exist yet — nothing has been persisted.
        let storage = temp.path().join("codex_oauth_auth.json");
        assert!(
            !storage.exists(),
            "no-persist imports must not touch disk before persist_imports()"
        );

        manager.persist_imports().await.unwrap();
        assert!(storage.exists());

        // Reload from disk and verify all three account ids round-tripped.
        let manager2 = CodexOAuthManager::new(temp.path().to_path_buf());
        let listed = manager2.list_accounts().await;
        assert_eq!(listed.len(), 3);
        let ids: std::collections::HashSet<String> = listed.into_iter().map(|a| a.id).collect();
        for id in ["a", "b", "c"] {
            assert!(ids.contains(id), "{id} missing after reload");
        }
    }

    #[tokio::test]
    async fn batch_slice_import_records_per_row_outcomes() {
        use crate::proxy::providers::codex_oauth_session::{
            CanonicalCodexSession, CodexSessionSource,
        };
        let temp = tempfile::tempdir().unwrap();
        let manager = CodexOAuthManager::new(temp.path().to_path_buf());

        let valid = CanonicalCodexSession {
            access_token: "at-v".to_string(),
            refresh_token: Some("rt-v".to_string()),
            account_id: Some("acct-v".to_string()),
            exp: Some(chrono::Utc::now().timestamp() + 3_600),
            source: CodexSessionSource::CodexCli,
            ..Default::default()
        };
        let invalid_no_account = CanonicalCodexSession {
            access_token: "at".to_string(),
            refresh_token: Some("rt".to_string()),
            ..Default::default()
        };
        let inputs = vec![
            (valid.clone(), true),
            (invalid_no_account, true),
            (valid, false), // duplicate, update_existing=false → Skipped
        ];

        let outcomes = manager.import_canonical_sessions(&inputs).await;
        assert!(matches!(
            outcomes[0],
            Ok(CodexImportOutcome {
                action: CodexImportAction::Created,
                ..
            })
        ));
        assert!(matches!(outcomes[1], Err(CodexOAuthError::ParseError(_))));
        assert!(matches!(
            outcomes[2],
            Ok(CodexImportOutcome {
                action: CodexImportAction::Skipped,
                ..
            })
        ));

        // File on disk reflects the single successful insert exactly once.
        let manager2 = CodexOAuthManager::new(temp.path().to_path_buf());
        assert_eq!(manager2.list_accounts().await.len(), 1);
    }
}
