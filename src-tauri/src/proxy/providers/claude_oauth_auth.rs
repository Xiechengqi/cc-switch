//! Claude OAuth Authentication Module
//!
//! 实现 Anthropic Claude 官方订阅的 OAuth PKCE 浏览器流程。
//! 支持多账号管理，每个 Provider 可关联不同的 Claude 账号。
//!
//! ## 认证流程
//! 1. 生成 PKCE code_verifier / code_challenge
//! 2. 启动本地回调服务器（端口 54545）
//! 3. 打开浏览器让用户在 claude.ai 完成授权
//! 4. 回调服务器接收 authorization_code
//! 5. 使用 code + code_verifier 换取 access_token + refresh_token
//! 6. 自动刷新 access_token（到期前 60 秒）
//!
//! ## 多账号支持
//! - 每个 Claude 账号独立存储 refresh_token
//! - Provider 通过 meta.authBinding 关联账号（auth_provider = "claude_oauth"）
//! - 通过 token 响应中的 account.email_address 作为账号标识

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;

use super::copilot_auth::GitHubAccount;

static GLOBAL_CLAUDE_OAUTH_MANAGER: OnceLock<Arc<RwLock<ClaudeOAuthManager>>> = OnceLock::new();

pub fn set_global_claude_oauth_manager(manager: Arc<RwLock<ClaudeOAuthManager>>) {
    let _ = GLOBAL_CLAUDE_OAUTH_MANAGER.set(manager);
}

pub fn global_claude_oauth_manager() -> Option<Arc<RwLock<ClaudeOAuthManager>>> {
    GLOBAL_CLAUDE_OAUTH_MANAGER.get().cloned()
}

/// Claude OAuth 客户端 ID（与 Claude Code CLI 相同）
const CLAUDE_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";

/// Claude OAuth 授权 URL
const CLAUDE_AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";

/// Claude OAuth Token URL（localhost callback / Claude Code CLI style）。
const CLAUDE_API_TOKEN_URL: &str = "https://api.anthropic.com/v1/oauth/token";

/// Claude OAuth Token URL（platform out-of-band web-paste style）。
const CLAUDE_PLATFORM_TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";

/// 本地回调服务器端口
const CALLBACK_PORT: u16 = 54545;

/// 回调路径
const CALLBACK_PATH: &str = "/callback";

/// Web 浏览器模式回退 redirect_uri（Anthropic 自家的 out-of-band 授权码展示页）。
/// 当 cc-switch 通过 client URL 在远程浏览器里访问、本机 listener 不可达时使用——
/// 用户在 platform.claude.com 上看到授权码后手动粘回 cc-switch。
const WEB_PASTE_REDIRECT_URI: &str = "https://platform.claude.com/oauth/code/callback";

/// OAuth Scopes
const CLAUDE_SCOPES: &str =
    "user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";

/// Token 刷新提前量（毫秒）
const TOKEN_REFRESH_BUFFER_MS: i64 = 60_000;

/// 回调等待超时（秒）
const CALLBACK_TIMEOUT_SECS: u64 = 300;

/// Token endpoint 单次请求超时（秒）。
const TOKEN_REQUEST_TIMEOUT_SECS: u64 = 30;

/// User-Agent
const CLAUDE_USER_AGENT: &str = "cc-switch-claude-oauth";

/// platform.claude.com 的 out-of-band token endpoint 更接近浏览器/axios 调用。
const CLAUDE_PLATFORM_USER_AGENT: &str = "axios/1.13.6";

/// Claude OAuth 错误
#[derive(Debug, thiserror::Error)]
pub enum ClaudeOAuthError {
    #[error("等待用户授权中")]
    AuthorizationPending,

    #[error("用户取消授权")]
    UserCancelled,

    #[error("授权超时")]
    Timeout,

    #[error("OAuth Token 获取失败: {0}")]
    TokenFetchFailed(String),

    #[error("Refresh Token 失效或已过期")]
    RefreshTokenInvalid,

    #[error("网络错误: {0}")]
    NetworkError(String),

    #[error("解析错误: {0}")]
    ParseError(String),

    #[error("IO 错误: {0}")]
    IoError(String),

    #[error("账号不存在: {0}")]
    AccountNotFound(String),

    #[error("回调服务器错误: {0}")]
    CallbackServerError(String),
}

impl From<reqwest::Error> for ClaudeOAuthError {
    fn from(err: reqwest::Error) -> Self {
        ClaudeOAuthError::NetworkError(err.to_string())
    }
}

impl From<std::io::Error> for ClaudeOAuthError {
    fn from(err: std::io::Error) -> Self {
        ClaudeOAuthError::IoError(err.to_string())
    }
}

/// OAuth Token 响应
#[derive(Debug, Clone, Deserialize)]
struct OAuthTokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    account: Option<AccountInfo>,
    #[serde(default)]
    organization: Option<OrgInfo>,
}

#[derive(Debug, Clone, Deserialize)]
struct AccountInfo {
    #[serde(default)]
    uuid: Option<String>,
    #[serde(default)]
    email_address: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct OrgInfo {
    #[serde(default)]
    uuid: Option<String>,
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

/// 进行中的 OAuth 流程
#[derive(Debug, Clone)]
struct PendingOAuthFlow {
    code_verifier: String,
    /// 当前流程使用的 redirect_uri。token 换取时必须和 authorize 步骤严格一致，
    /// localhost 模式 = `http://localhost:54545/callback`，
    /// web-paste 模式 = `https://platform.claude.com/oauth/code/callback`。
    redirect_uri: String,
    /// Unix 毫秒时间戳，超时后可清理
    expires_at_ms: i64,
}

/// OAuth 浏览器流程的运行模式。
///
/// - `Localhost`：cc-switch 进程在本机起 TCP listener 接收 claude.ai 重定向回来的 code，
///   桌面 Tauri app 用户体验最好（点开链接、授权、自动完成）。
/// - `WebPaste`：远程浏览器访问 client URL 时（cc-switch 进程的 localhost 不可达），
///   `redirect_uri` 用 Anthropic 自家的 out-of-band 页面 `platform.claude.com/oauth/code/callback`。
///   用户在那里复制授权码，粘回 cc-switch web UI；前端调 `submit_pasted_code(state, code)`
///   触发 token 换取。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OAuthFlowMode {
    Localhost,
    WebPaste,
}

/// 后台回调任务的结果状态（由前端轮询消费）
#[derive(Debug)]
enum FlowResult {
    /// 后台任务仍在等待浏览器回调
    Pending,
    /// 后台任务已完成（成功或失败）
    Ready(Result<GitHubAccount, String>),
}

/// 持久化的账号数据
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ClaudeAccountData {
    /// 账号唯一标识（account UUID 或 email）
    pub account_id: String,
    /// 账号邮箱
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// Organization UUID
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org_uuid: Option<String>,
    /// Refresh Token（持久化）
    pub refresh_token: String,
    /// 认证时间戳（秒）
    pub authenticated_at: i64,
}

/// 公开的账号信息（返回给前端，复用 GitHubAccount 结构）
impl From<&ClaudeAccountData> for GitHubAccount {
    fn from(data: &ClaudeAccountData) -> Self {
        GitHubAccount {
            id: data.account_id.clone(),
            login: data
                .email
                .clone()
                .unwrap_or_else(|| format!("Claude ({})", &data.account_id)),
            email: data.email.clone(),
            avatar_url: None,
            authenticated_at: data.authenticated_at,
            github_domain: "github.com".to_string(),
        }
    }
}

/// 持久化存储结构
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ClaudeOAuthStore {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    accounts: HashMap<String, ClaudeAccountData>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default_account_id: Option<String>,
}

/// Claude OAuth 认证管理器（多账号）
#[derive(Clone)]
pub struct ClaudeOAuthManager {
    accounts: Arc<RwLock<HashMap<String, ClaudeAccountData>>>,
    default_account_id: Arc<RwLock<Option<String>>>,
    /// 内存缓存的 access_token（不持久化）
    access_tokens: Arc<RwLock<HashMap<String, CachedAccessToken>>>,
    /// 每个账号的刷新锁
    refresh_locks: Arc<RwLock<HashMap<String, Arc<Mutex<()>>>>>,
    /// 进行中的 OAuth 流程（state -> flow_data）
    pending_flows: Arc<RwLock<HashMap<String, PendingOAuthFlow>>>,
    /// 后台回调任务的结果槽（state -> Pending/Ready），
    /// 由后台任务填充，由前端通过 poll_callback_result 取出。
    flow_results: Arc<RwLock<HashMap<String, FlowResult>>>,
    /// 当前活动的回调后台任务句柄。
    /// 新的登录流程会先 abort + join 旧任务以释放回调端口。
    active_flow_handle: Arc<Mutex<Option<JoinHandle<()>>>>,
    http_client: Client,
    storage_path: PathBuf,
}

impl ClaudeOAuthManager {
    pub fn new(data_dir: PathBuf) -> Self {
        let storage_path = data_dir.join("claude_oauth_auth.json");

        let manager = Self {
            accounts: Arc::new(RwLock::new(HashMap::new())),
            default_account_id: Arc::new(RwLock::new(None)),
            access_tokens: Arc::new(RwLock::new(HashMap::new())),
            refresh_locks: Arc::new(RwLock::new(HashMap::new())),
            pending_flows: Arc::new(RwLock::new(HashMap::new())),
            flow_results: Arc::new(RwLock::new(HashMap::new())),
            active_flow_handle: Arc::new(Mutex::new(None)),
            http_client: Client::new(),
            storage_path,
        };

        if let Err(e) = manager.load_from_disk_sync() {
            log::warn!("[ClaudeOAuth] 加载存储失败: {e}");
        }

        manager
    }

    // ==================== PKCE 工具 ====================

    /// 生成 PKCE code_verifier（128 字符 URL-safe Base64）
    fn generate_code_verifier() -> String {
        use rand::RngCore;
        let mut bytes = [0u8; 96];
        rand::thread_rng().fill_bytes(&mut bytes);
        URL_SAFE_NO_PAD.encode(bytes)
    }

    /// 生成 PKCE code_challenge（SHA256 + URL-safe Base64）
    fn generate_code_challenge(verifier: &str) -> String {
        let hash = Sha256::digest(verifier.as_bytes());
        URL_SAFE_NO_PAD.encode(hash)
    }

    /// 生成随机 state 参数
    fn generate_state() -> String {
        use rand::RngCore;
        let mut bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut bytes);
        URL_SAFE_NO_PAD.encode(bytes)
    }

    // ==================== OAuth 浏览器流程 ====================

    /// 启动 OAuth 浏览器登录流程。
    ///
    /// `mode` 控制 redirect_uri 选择 + 是否绑定本机回调端口：
    /// - [`OAuthFlowMode::Localhost`]（桌面 Tauri 默认）：在 127.0.0.1:54545 绑定
    ///   listener，redirect_uri 用 `http://localhost:54545/callback`。授权后浏览器
    ///   自动回调，前端轮询 `poll_callback_result(state)` 拿结果。
    /// - [`OAuthFlowMode::WebPaste`]（通过 client URL 远程访问 cc-switch 时）：
    ///   redirect_uri 用 Anthropic 自家的 `platform.claude.com/oauth/code/callback`，
    ///   不绑端口、不 spawn 后台任务；用户在那个页面看到授权码后手动复制粘回
    ///   cc-switch UI，前端调 `submit_pasted_code(state, code)` 完成 token 换取。
    ///
    /// 两种模式共享 PKCE / state / pending_flows / flow_results 机制——`submit_pasted_code`
    /// 成功后也会把 `FlowResult::Ready` 写进 `flow_results`，所以既有的轮询代码
    /// 完全不需要改也能感知到结果。
    pub async fn start_browser_flow(
        &self,
        mode: OAuthFlowMode,
    ) -> Result<ClaudeOAuthStartResponse, ClaudeOAuthError> {
        use tokio::net::TcpListener;

        let code_verifier = Self::generate_code_verifier();
        let code_challenge = Self::generate_code_challenge(&code_verifier);
        let state = Self::generate_state();
        let (redirect_uri, auth_url) = build_authorize_url(mode, &code_challenge, &state);

        // ── 1. abort 旧的回调任务以释放端口 ──
        {
            let mut handle_guard = self.active_flow_handle.lock().await;
            if let Some(prev) = handle_guard.take() {
                prev.abort();
                let _ = prev.await; // 等待任务退出、listener 被 drop
            }
        }
        // 清理旧的 flow_results
        {
            let mut results = self.flow_results.write().await;
            results.clear();
        }

        let expires_at_ms =
            chrono::Utc::now().timestamp_millis() + (CALLBACK_TIMEOUT_SECS as i64) * 1000;

        // ── 2. 记录进行中的流程（在端口绑定之前确保 state 已可被消费） ──
        {
            let mut pending = self.pending_flows.write().await;
            let now_ms = chrono::Utc::now().timestamp_millis();
            pending.retain(|_, flow| flow.expires_at_ms > now_ms);
            pending.insert(
                state.clone(),
                PendingOAuthFlow {
                    code_verifier,
                    redirect_uri: redirect_uri.clone(),
                    expires_at_ms,
                },
            );
        }

        // 标记为 Pending（两种模式都标 Pending；WebPaste 由 submit_pasted_code 翻成 Ready）
        {
            let mut results = self.flow_results.write().await;
            results.insert(state.clone(), FlowResult::Pending);
        }

        let callback_port = match mode {
            OAuthFlowMode::Localhost => {
                // ── 3a. localhost 模式：绑回调端口 + spawn 等待任务 ──
                let addr = format!("127.0.0.1:{CALLBACK_PORT}");
                let listener = TcpListener::bind(&addr).await.map_err(|e| {
                    ClaudeOAuthError::CallbackServerError(format!(
                        "无法绑定回调端口 {CALLBACK_PORT}: {e}"
                    ))
                })?;
                log::info!("[ClaudeOAuth] 回调服务器启动于 {addr}");

                let manager = self.clone();
                let state_clone = state.clone();
                let handle = tokio::spawn(async move {
                    let result = manager
                        .run_callback_on_listener(listener, &state_clone)
                        .await;
                    let mut results = manager.flow_results.write().await;
                    results.insert(
                        state_clone,
                        FlowResult::Ready(result.map_err(|e| e.to_string())),
                    );
                });
                {
                    let mut handle_guard = self.active_flow_handle.lock().await;
                    *handle_guard = Some(handle);
                }
                CALLBACK_PORT
            }
            OAuthFlowMode::WebPaste => {
                // ── 3b. web-paste 模式：跳过端口绑定，等待 submit_pasted_code 触发 ──
                log::info!("[ClaudeOAuth] web-paste 模式：等待用户粘贴授权码");
                0
            }
        };

        log::info!("[ClaudeOAuth] 启动浏览器 OAuth 流程，mode={mode:?}, state: {state}");

        Ok(ClaudeOAuthStartResponse {
            auth_url,
            state,
            callback_port,
        })
    }

    /// Web-paste 模式专用：用户从 platform.claude.com 复制 code 后调用。
    ///
    /// 把 (state, code) 喂回与 [`handle_callback`] 相同的 token 换取链路，
    /// 结果同时写入 `flow_results`——既有的 `poll_callback_result` 调用方
    /// （比如 commands/auth.rs:auth_poll_for_account）能继续以无差别方式拿到结果。
    pub async fn submit_pasted_code(
        &self,
        code: &str,
        state: &str,
    ) -> Result<GitHubAccount, ClaudeOAuthError> {
        let result = self.handle_callback(code, state).await;
        // 镜像 localhost 模式后台任务的行为：把结果写入 flow_results 让 poll 可见。
        {
            let mut results = self.flow_results.write().await;
            results.insert(
                state.to_string(),
                FlowResult::Ready(
                    result
                        .as_ref()
                        .map(|account| account.clone())
                        .map_err(|e| e.to_string()),
                ),
            );
        }
        result
    }

    /// 处理 OAuth 回调（收到 authorization_code 后调用）
    ///
    /// 前端或回调服务器在收到 code 和 state 后，调用此方法完成 token 交换。
    pub async fn handle_callback(
        &self,
        code: &str,
        state: &str,
    ) -> Result<GitHubAccount, ClaudeOAuthError> {
        let (authorization_code, token_state) = parse_authorization_code_input(code, state)?;

        // 读取并验证 pending flow。成功保存账号后再消费，失败时允许用户重试粘贴。
        let flow = {
            let pending = self.pending_flows.read().await;
            pending.get(state).cloned().ok_or_else(|| {
                ClaudeOAuthError::TokenFetchFailed(
                    "未找到对应的 OAuth 流程（state 不匹配或已过期），请重新登录".to_string(),
                )
            })?
        };

        if flow.expires_at_ms <= chrono::Utc::now().timestamp_millis() {
            let mut pending = self.pending_flows.write().await;
            pending.remove(state);
            return Err(ClaudeOAuthError::Timeout);
        }

        log::info!("[ClaudeOAuth] 收到回调，正在换取 OAuth Token");

        // 用 authorization_code + code_verifier 换 token
        let tokens = self
            .exchange_code_for_tokens(
                &authorization_code,
                &token_state,
                &flow.code_verifier,
                &flow.redirect_uri,
            )
            .await?;

        let refresh_token = tokens.refresh_token.clone().ok_or_else(|| {
            ClaudeOAuthError::TokenFetchFailed("响应缺少 refresh_token".to_string())
        })?;

        // 提取账号信息
        let (account_id, email, org_uuid) = extract_identity_from_response(&tokens);
        let account_id = account_id.ok_or_else(|| {
            ClaudeOAuthError::ParseError("无法从 token 响应中提取账号标识".to_string())
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
            .add_account_internal(account_id, refresh_token, email, org_uuid)
            .await?;

        {
            let mut pending = self.pending_flows.write().await;
            pending.remove(state);
        }

        Ok(account)
    }

    /// 非阻塞轮询回调结果
    ///
    /// 由前端定期调用。返回 `Ok(None)` 表示仍在等待浏览器回调，
    /// 返回 `Ok(Some(account))` 表示认证完成，返回 `Err` 表示认证失败。
    pub async fn poll_callback_result(
        &self,
        state: &str,
    ) -> Result<Option<GitHubAccount>, ClaudeOAuthError> {
        let mut results = self.flow_results.write().await;

        match results.get(state) {
            None => {
                // 没有对应流程记录 — state 无效或已被消费
                Err(ClaudeOAuthError::TokenFetchFailed(
                    "未找到对应的 OAuth 流程（state 不匹配或已过期），请重新登录".to_string(),
                ))
            }
            Some(FlowResult::Pending) => Ok(None),
            Some(FlowResult::Ready(_)) => {
                // 取出结果（只能消费一次）
                let entry = results.remove(state).unwrap();
                if let FlowResult::Ready(r) = entry {
                    match r {
                        Ok(account) => Ok(Some(account)),
                        Err(e) => Err(ClaudeOAuthError::TokenFetchFailed(e)),
                    }
                } else {
                    unreachable!()
                }
            }
        }
    }

    /// 在已绑定的 listener 上等待 OAuth 回调（内部方法，由 spawn 任务调用）
    async fn run_callback_on_listener(
        &self,
        listener: tokio::net::TcpListener,
        state: &str,
    ) -> Result<GitHubAccount, ClaudeOAuthError> {
        log::info!("[ClaudeOAuth] 后台任务：等待 OAuth 回调...");

        let timeout = tokio::time::Duration::from_secs(CALLBACK_TIMEOUT_SECS);
        let result = tokio::time::timeout(timeout, Self::accept_callback(&listener)).await;

        match result {
            Ok(Ok((code, received_state))) => {
                if received_state != state {
                    return Err(ClaudeOAuthError::TokenFetchFailed(format!(
                        "state 不匹配: 期望 {state}, 收到 {received_state}"
                    )));
                }
                self.handle_callback(&code, &received_state).await
            }
            Ok(Err(e)) => Err(e),
            Err(_) => {
                let mut pending = self.pending_flows.write().await;
                pending.remove(state);
                Err(ClaudeOAuthError::Timeout)
            }
        }
    }

    /// 接受单个 HTTP 回调请求并解析 code/state
    async fn accept_callback(
        listener: &tokio::net::TcpListener,
    ) -> Result<(String, String), ClaudeOAuthError> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (mut stream, _) = listener
            .accept()
            .await
            .map_err(|e| ClaudeOAuthError::CallbackServerError(format!("accept 失败: {e}")))?;

        let mut buf = vec![0u8; 4096];
        let n = stream
            .read(&mut buf)
            .await
            .map_err(|e| ClaudeOAuthError::CallbackServerError(format!("读取请求失败: {e}")))?;

        let request = String::from_utf8_lossy(&buf[..n]);

        // 解析 GET 请求的 query parameters
        let (code, state) = parse_callback_request(&request)?;

        // 返回成功页面
        let response_body = r#"<!DOCTYPE html><html><body><h2>Authorization successful!</h2><p>You can close this window and return to cc-switch.</p><script>window.close()</script></body></html>"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            response_body.len(),
            response_body
        );
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.flush().await;

        Ok((code, state))
    }

    /// 用 authorization_code + code_verifier 换取 tokens。
    ///
    /// `redirect_uri` 必须和 authorize 步骤里的字字相符（OAuth 2.0 RFC 6749 §4.1.3），
    /// 由调用方按 [`PendingOAuthFlow::redirect_uri`] 传进来——localhost 模式是
    /// `http://localhost:54545/callback`，web-paste 模式是
    /// `https://platform.claude.com/oauth/code/callback`。
    async fn exchange_code_for_tokens(
        &self,
        code: &str,
        state: &str,
        code_verifier: &str,
        redirect_uri: &str,
    ) -> Result<OAuthTokenResponse, ClaudeOAuthError> {
        let body = serde_json::json!({
            "code": code,
            "state": state,
            "grant_type": "authorization_code",
            "client_id": CLAUDE_CLIENT_ID,
            "redirect_uri": redirect_uri,
            "code_verifier": code_verifier,
        });

        let urls = token_urls_for_redirect(redirect_uri);
        let mut last_error: Option<ClaudeOAuthError> = None;
        for (idx, token_url) in urls.iter().copied().enumerate() {
            log::info!("[ClaudeOAuth] 正在请求 OAuth Token endpoint: {token_url}");
            let response = self
                .http_client
                .post(token_url)
                .header("Content-Type", "application/json")
                .header("Accept", "application/json, text/plain, */*")
                .header("User-Agent", user_agent_for_token_url(token_url))
                .timeout(Duration::from_secs(TOKEN_REQUEST_TIMEOUT_SECS))
                .json(&body)
                .send()
                .await;

            let response = match response {
                Ok(response) => response,
                Err(err) => {
                    let message = format!("Token 交换请求失败 ({token_url}): {err}");
                    if idx + 1 < urls.len() {
                        log::warn!("[ClaudeOAuth] {message}，尝试备用 endpoint");
                        last_error = Some(ClaudeOAuthError::NetworkError(message));
                        continue;
                    }
                    return Err(ClaudeOAuthError::NetworkError(message));
                }
            };

            let status = response.status();
            if !status.is_success() {
                let text = response.text().await.unwrap_or_default();
                let message = format!("Token 交换失败 ({token_url}): {status} - {text}");
                if idx + 1 < urls.len() {
                    log::warn!("[ClaudeOAuth] {message}，尝试备用 endpoint");
                    last_error = Some(ClaudeOAuthError::TokenFetchFailed(message));
                    continue;
                }
                return Err(ClaudeOAuthError::TokenFetchFailed(message));
            }

            log::info!("[ClaudeOAuth] OAuth Token endpoint 请求成功: {token_url}");
            return response
                .json()
                .await
                .map_err(|e| ClaudeOAuthError::ParseError(e.to_string()));
        }

        Err(last_error.unwrap_or_else(|| {
            ClaudeOAuthError::TokenFetchFailed("没有可用的 OAuth Token endpoint".to_string())
        }))
    }

    /// 用 refresh_token 刷新 access_token
    async fn refresh_with_token(
        &self,
        refresh_token: &str,
    ) -> Result<OAuthTokenResponse, ClaudeOAuthError> {
        let body = serde_json::json!({
            "client_id": CLAUDE_CLIENT_ID,
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
        });

        let urls = [CLAUDE_API_TOKEN_URL, CLAUDE_PLATFORM_TOKEN_URL];
        let mut last_error: Option<ClaudeOAuthError> = None;
        for (idx, token_url) in urls.iter().copied().enumerate() {
            log::info!("[ClaudeOAuth] 正在刷新 OAuth Token endpoint: {token_url}");
            let response = self
                .http_client
                .post(token_url)
                .header("Content-Type", "application/json")
                .header("Accept", "application/json, text/plain, */*")
                .header("User-Agent", user_agent_for_token_url(token_url))
                .timeout(Duration::from_secs(TOKEN_REQUEST_TIMEOUT_SECS))
                .json(&body)
                .send()
                .await;

            let response = match response {
                Ok(response) => response,
                Err(err) => {
                    let message = format!("Refresh 请求失败 ({token_url}): {err}");
                    if idx + 1 < urls.len() {
                        log::warn!("[ClaudeOAuth] {message}，尝试备用 endpoint");
                        last_error = Some(ClaudeOAuthError::NetworkError(message));
                        continue;
                    }
                    return Err(ClaudeOAuthError::NetworkError(message));
                }
            };

            let status = response.status();
            if !status.is_success() {
                let text = response.text().await.unwrap_or_default();
                let message = format!("Refresh 失败 ({token_url}): {status} - {text}");
                if idx + 1 < urls.len() {
                    log::warn!("[ClaudeOAuth] {message}，尝试备用 endpoint");
                    last_error = Some(ClaudeOAuthError::TokenFetchFailed(message));
                    continue;
                }
                if status == reqwest::StatusCode::UNAUTHORIZED
                    || status == reqwest::StatusCode::FORBIDDEN
                {
                    return Err(ClaudeOAuthError::RefreshTokenInvalid);
                }
                return Err(ClaudeOAuthError::TokenFetchFailed(message));
            }

            log::info!("[ClaudeOAuth] OAuth Token 刷新成功: {token_url}");
            return response
                .json()
                .await
                .map_err(|e| ClaudeOAuthError::ParseError(e.to_string()));
        }

        Err(last_error.unwrap_or_else(|| {
            ClaudeOAuthError::TokenFetchFailed("没有可用的 OAuth Token endpoint".to_string())
        }))
    }

    // ==================== Token 获取（含自动刷新） ====================

    /// 获取指定账号的有效 access_token（必要时自动刷新）
    pub async fn get_valid_token_for_account(
        &self,
        account_id: &str,
    ) -> Result<String, ClaudeOAuthError> {
        // 先检查缓存
        {
            let tokens = self.access_tokens.read().await;
            if let Some(cached) = tokens.get(account_id) {
                if !cached.is_expiring_soon() {
                    return Ok(cached.token.clone());
                }
            }
        }

        log::info!("[ClaudeOAuth] 账号 {account_id} 的 access_token 需要刷新");

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
            accounts
                .get(account_id)
                .map(|a| a.refresh_token.clone())
                .ok_or_else(|| ClaudeOAuthError::AccountNotFound(account_id.to_string()))?
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
    pub async fn get_valid_token(&self) -> Result<String, ClaudeOAuthError> {
        match self.resolve_default_account_id().await {
            Some(id) => self.get_valid_token_for_account(&id).await,
            None => Err(ClaudeOAuthError::AccountNotFound(
                "无可用的 Claude 账号，请先登录".to_string(),
            )),
        }
    }

    /// 获取默认账号 ID
    pub async fn default_account_id(&self) -> Option<String> {
        self.resolve_default_account_id().await
    }

    // ==================== 多账号管理 ====================

    pub async fn list_accounts(&self) -> Vec<GitHubAccount> {
        let accounts = self.accounts.read().await.clone();
        let default_id = self.resolve_default_account_id().await;
        Self::sorted_accounts(&accounts, default_id.as_deref())
    }

    /// 作废指定账号的 access_token 缓存。
    ///
    /// 用于上游返回 401 时，由 forwarder 触发，使下一次 `get_valid_token_for_account`
    /// 走 refresh 分支去拿新 token。不动 `accounts` 里的 refresh_token。
    pub async fn invalidate_cached_token(&self, account_id: &str) {
        let mut tokens = self.access_tokens.write().await;
        if tokens.remove(account_id).is_some() {
            log::info!("[ClaudeOAuth] 已作废 access_token 缓存 (account={account_id})");
        }
    }

    pub async fn remove_account(&self, account_id: &str) -> Result<(), ClaudeOAuthError> {
        log::info!("[ClaudeOAuth] 移除账号: {account_id}");

        {
            let mut accounts = self.accounts.write().await;
            if accounts.remove(account_id).is_none() {
                return Err(ClaudeOAuthError::AccountNotFound(account_id.to_string()));
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

    pub async fn set_default_account(&self, account_id: &str) -> Result<(), ClaudeOAuthError> {
        {
            let accounts = self.accounts.read().await;
            if !accounts.contains_key(account_id) {
                return Err(ClaudeOAuthError::AccountNotFound(account_id.to_string()));
            }
        }

        {
            let mut default = self.default_account_id.write().await;
            *default = Some(account_id.to_string());
        }

        self.save_to_disk().await?;
        Ok(())
    }

    pub async fn clear_auth(&self) -> Result<(), ClaudeOAuthError> {
        log::info!("[ClaudeOAuth] 清除所有认证");

        // abort 活动的回调任务
        {
            let mut handle_guard = self.active_flow_handle.lock().await;
            if let Some(prev) = handle_guard.take() {
                prev.abort();
                let _ = prev.await;
            }
        }

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
            let mut pending = self.pending_flows.write().await;
            pending.clear();
        }
        {
            let mut results = self.flow_results.write().await;
            results.clear();
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

    /// 获取认证状态摘要
    pub async fn get_status(&self) -> ClaudeOAuthStatus {
        let accounts_map = self.accounts.read().await.clone();
        let default_id = self.resolve_default_account_id().await;
        let account_list = Self::sorted_accounts(&accounts_map, default_id.as_deref());
        let authenticated = !account_list.is_empty();
        let username = default_id
            .as_ref()
            .and_then(|id| accounts_map.get(id))
            .and_then(|a| a.email.clone())
            .or_else(|| account_list.first().map(|a| a.login.clone()));

        ClaudeOAuthStatus {
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
        org_uuid: Option<String>,
    ) -> Result<GitHubAccount, ClaudeOAuthError> {
        let now = chrono::Utc::now().timestamp();

        let data = ClaudeAccountData {
            account_id: account_id.clone(),
            email,
            org_uuid,
            refresh_token,
            authenticated_at: now,
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

    fn fallback_default_account_id(
        accounts: &HashMap<String, ClaudeAccountData>,
    ) -> Option<String> {
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
        accounts: &HashMap<String, ClaudeAccountData>,
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

    fn write_store_atomic(&self, content: &str) -> Result<(), ClaudeOAuthError> {
        if let Some(parent) = self.storage_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let parent = self
            .storage_path
            .parent()
            .ok_or_else(|| ClaudeOAuthError::IoError("无效的存储路径".to_string()))?;
        let file_name = self
            .storage_path
            .file_name()
            .ok_or_else(|| ClaudeOAuthError::IoError("无效的存储文件名".to_string()))?
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

    fn load_from_disk_sync(&self) -> Result<(), ClaudeOAuthError> {
        if !self.storage_path.exists() {
            return Ok(());
        }

        let content = std::fs::read_to_string(&self.storage_path)?;
        let store: ClaudeOAuthStore = serde_json::from_str(&content)
            .map_err(|e| ClaudeOAuthError::ParseError(e.to_string()))?;

        if let Ok(mut accounts) = self.accounts.try_write() {
            *accounts = store.accounts;
            log::info!("[ClaudeOAuth] 从磁盘加载 {} 个账号", accounts.len());
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

    async fn save_to_disk(&self) -> Result<(), ClaudeOAuthError> {
        let accounts = self.accounts.read().await.clone();
        let default = self.resolve_default_account_id().await;

        let store = ClaudeOAuthStore {
            version: 1,
            accounts,
            default_account_id: default,
        };

        let content = serde_json::to_string_pretty(&store)
            .map_err(|e| ClaudeOAuthError::ParseError(e.to_string()))?;

        self.write_store_atomic(&content)?;

        log::info!(
            "[ClaudeOAuth] 保存到磁盘成功（{} 个账号）",
            store.accounts.len()
        );

        Ok(())
    }
}

/// Claude OAuth 状态摘要
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudeOAuthStatus {
    pub accounts: Vec<GitHubAccount>,
    pub default_account_id: Option<String>,
    pub authenticated: bool,
    pub username: Option<String>,
}

/// 启动浏览器流程的响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudeOAuthStartResponse {
    /// 浏览器打开的授权 URL
    pub auth_url: String,
    /// 本次流程的 state（用于匹配回调）
    pub state: String,
    /// 回调服务器端口
    pub callback_port: u16,
}

// ==================== 工具函数 ====================

fn extract_identity_from_response(
    tokens: &OAuthTokenResponse,
) -> (Option<String>, Option<String>, Option<String>) {
    let email = tokens
        .account
        .as_ref()
        .and_then(|a| a.email_address.clone());
    let account_uuid = tokens.account.as_ref().and_then(|a| a.uuid.clone());
    let org_uuid = tokens.organization.as_ref().and_then(|o| o.uuid.clone());

    // 优先使用 email 作为账号 ID（更易识别），回退到 account UUID
    let account_id = email.clone().or(account_uuid);

    (account_id, email, org_uuid)
}

fn compute_expires_at_ms(expires_in: Option<i64>) -> i64 {
    let expires_in = expires_in.unwrap_or(3600); // 默认 1 小时
    chrono::Utc::now().timestamp_millis() + expires_in * 1000
}

fn token_urls_for_redirect(redirect_uri: &str) -> Vec<&'static str> {
    if redirect_uri == WEB_PASTE_REDIRECT_URI {
        vec![CLAUDE_PLATFORM_TOKEN_URL, CLAUDE_API_TOKEN_URL]
    } else {
        vec![CLAUDE_API_TOKEN_URL, CLAUDE_PLATFORM_TOKEN_URL]
    }
}

fn user_agent_for_token_url(token_url: &str) -> &'static str {
    if token_url == CLAUDE_PLATFORM_TOKEN_URL {
        CLAUDE_PLATFORM_USER_AGENT
    } else {
        CLAUDE_USER_AGENT
    }
}

fn parse_authorization_code_input(
    raw_code: &str,
    expected_state: &str,
) -> Result<(String, String), ClaudeOAuthError> {
    let trimmed = raw_code.trim();
    if trimmed.is_empty() {
        return Err(ClaudeOAuthError::TokenFetchFailed(
            "授权码为空，请重新复制 platform.claude.com 显示的完整授权码".to_string(),
        ));
    }

    let (code, state_from_code) = match trimmed.split_once('#') {
        Some((code, state)) => (code.trim(), Some(state.trim())),
        None => (trimmed, None),
    };

    if code.is_empty() {
        return Err(ClaudeOAuthError::TokenFetchFailed(
            "授权码格式无效：缺少 code 部分".to_string(),
        ));
    }

    let token_state = match state_from_code {
        Some(state) if state.is_empty() => expected_state.to_string(),
        Some(state) => {
            if state != expected_state {
                return Err(ClaudeOAuthError::TokenFetchFailed(format!(
                    "state 不匹配: 期望 {expected_state}, 收到 {state}"
                )));
            }
            state.to_string()
        }
        None => expected_state.to_string(),
    };

    Ok((code.to_string(), token_state))
}

/// 构造 (redirect_uri, auth_url) 二元组。纯函数，便于单测。
///
/// 见 [`OAuthFlowMode`] 对两种模式的差异。
fn build_authorize_url(mode: OAuthFlowMode, code_challenge: &str, state: &str) -> (String, String) {
    let redirect_uri = match mode {
        OAuthFlowMode::Localhost => format!("http://localhost:{CALLBACK_PORT}{CALLBACK_PATH}"),
        OAuthFlowMode::WebPaste => WEB_PASTE_REDIRECT_URI.to_string(),
    };
    let auth_url = format!(
        "{CLAUDE_AUTHORIZE_URL}?code=true&client_id={CLAUDE_CLIENT_ID}&response_type=code&redirect_uri={}&scope={}&code_challenge={}&code_challenge_method=S256&state={}",
        urlencoding::encode(&redirect_uri),
        urlencoding::encode(CLAUDE_SCOPES),
        urlencoding::encode(code_challenge),
        urlencoding::encode(state),
    );
    (redirect_uri, auth_url)
}

/// 解析 HTTP 回调请求中的 code 和 state
fn parse_callback_request(request: &str) -> Result<(String, String), ClaudeOAuthError> {
    // 解析 GET /callback?code=xxx&state=yyy HTTP/1.1
    let first_line = request
        .lines()
        .next()
        .ok_or_else(|| ClaudeOAuthError::CallbackServerError("空请求".to_string()))?;

    let path = first_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| ClaudeOAuthError::CallbackServerError("无法解析请求路径".to_string()))?;

    // 检查是否有 error
    if let Some(query) = path.split('?').nth(1) {
        let params: HashMap<&str, &str> = query
            .split('&')
            .filter_map(|p| {
                let mut parts = p.splitn(2, '=');
                Some((parts.next()?, parts.next().unwrap_or("")))
            })
            .collect();

        if let Some(error) = params.get("error") {
            let desc = params.get("error_description").unwrap_or(&"");
            return Err(ClaudeOAuthError::TokenFetchFailed(format!(
                "OAuth 错误: {error} - {desc}"
            )));
        }

        let code = params
            .get("code")
            .ok_or_else(|| ClaudeOAuthError::CallbackServerError("回调缺少 code 参数".to_string()))?
            .to_string();

        let state = params
            .get("state")
            .ok_or_else(|| {
                ClaudeOAuthError::CallbackServerError("回调缺少 state 参数".to_string())
            })?
            .to_string();

        // URL decode
        let code = urlencoding::decode(&code)
            .unwrap_or_else(|_| code.clone().into())
            .to_string();
        let state = urlencoding::decode(&state)
            .unwrap_or_else(|_| state.clone().into())
            .to_string();

        Ok((code, state))
    } else {
        Err(ClaudeOAuthError::CallbackServerError(
            "回调请求缺少查询参数".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_CHALLENGE: &str = "ltDIXwTqQFLmXcVs2j5J5n-ZQ5_KeMmxpEvMVPKIj90";
    const SAMPLE_STATE: &str = "RaZZE1uDqXE78bwEnJxRx9itnWHUeGxm0jy1BQD8q90";

    #[test]
    fn localhost_mode_uses_loopback_redirect() {
        let (redirect, url) =
            build_authorize_url(OAuthFlowMode::Localhost, SAMPLE_CHALLENGE, SAMPLE_STATE);
        assert_eq!(redirect, "http://localhost:54545/callback");
        assert!(url.starts_with("https://claude.ai/oauth/authorize?"));
        assert!(url.contains("client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A54545%2Fcallback"));
        // 标识 web-paste 的占位符不应出现
        assert!(!url.contains("platform.claude.com"));
    }

    #[test]
    fn web_paste_mode_uses_platform_redirect() {
        let (redirect, url) =
            build_authorize_url(OAuthFlowMode::WebPaste, SAMPLE_CHALLENGE, SAMPLE_STATE);
        assert_eq!(redirect, "https://platform.claude.com/oauth/code/callback");
        assert!(url
            .contains("redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback"));
        // 不应再带 localhost
        assert!(!url.contains("localhost"));
    }

    #[test]
    fn auth_url_includes_pkce_and_state_intact() {
        let (_, url) = build_authorize_url(OAuthFlowMode::WebPaste, SAMPLE_CHALLENGE, SAMPLE_STATE);
        // PKCE challenge 和 state 是 URL-safe base64，不需要 percent-encode 之外再做处理。
        assert!(url.contains(&format!(
            "code_challenge={}",
            urlencoding::encode(SAMPLE_CHALLENGE)
        )));
        assert!(url.contains(&format!("state={}", urlencoding::encode(SAMPLE_STATE))));
    }

    #[test]
    fn parses_platform_code_with_state_fragment() {
        let (code, state) =
            parse_authorization_code_input(&format!("auth-code#{SAMPLE_STATE}"), SAMPLE_STATE)
                .unwrap();
        assert_eq!(code, "auth-code");
        assert_eq!(state, SAMPLE_STATE);
    }

    #[test]
    fn rejects_platform_code_with_mismatched_state_fragment() {
        let err =
            parse_authorization_code_input("auth-code#other-state", SAMPLE_STATE).unwrap_err();
        assert!(err.to_string().contains("state 不匹配"));
    }

    #[test]
    fn web_paste_redirect_prefers_platform_token_endpoint() {
        assert_eq!(
            token_urls_for_redirect(WEB_PASTE_REDIRECT_URI),
            vec![CLAUDE_PLATFORM_TOKEN_URL, CLAUDE_API_TOKEN_URL]
        );
    }

    /// 两种模式的 redirect_uri 必须完全不同——这就是整个 web 模式的关键不变量。
    #[test]
    fn modes_produce_distinct_redirects() {
        let (loopback, _) =
            build_authorize_url(OAuthFlowMode::Localhost, SAMPLE_CHALLENGE, SAMPLE_STATE);
        let (web, _) = build_authorize_url(OAuthFlowMode::WebPaste, SAMPLE_CHALLENGE, SAMPLE_STATE);
        assert_ne!(loopback, web);
    }
}
