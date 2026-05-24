//! Kiro OAuth Authentication Module
//!
//! Implements Kiro portal OAuth (PKCE browser flow) with multi-account
//! management. Providers bind to accounts through `meta.authBinding` using
//! `auth_provider = "kiro_oauth"`.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;

use super::copilot_auth::GitHubAccount;

const KIRO_PORTAL_URL: &str = "https://app.kiro.dev";
const CALLBACK_PORT: u16 = 54547;
const CALLBACK_PATH: &str = "/oauth/callback";
const CALLBACK_TIMEOUT_SECS: u64 = 300;
const TOKEN_REFRESH_BUFFER_MS: i64 = 60_000;
const DEFAULT_REGION: &str = "us-east-1";
const DEFAULT_KIRO_VERSION: &str = "2.3.0";

#[derive(Debug, thiserror::Error)]
pub enum KiroOAuthError {
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

impl From<reqwest::Error> for KiroOAuthError {
    fn from(err: reqwest::Error) -> Self {
        KiroOAuthError::NetworkError(err.to_string())
    }
}

impl From<std::io::Error> for KiroOAuthError {
    fn from(err: std::io::Error) -> Self {
        KiroOAuthError::IoError(err.to_string())
    }
}

#[derive(Debug, Clone, Deserialize)]
struct SocialTokenResponse {
    #[serde(rename = "accessToken")]
    access_token: String,
    #[serde(default, rename = "refreshToken")]
    refresh_token: Option<String>,
    #[serde(default, rename = "expiresAt")]
    expires_at: Option<String>,
    #[serde(default, rename = "expiresIn")]
    expires_in: Option<i64>,
    #[serde(default, rename = "profileArn")]
    profile_arn: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KiroUsageLimitsResponse {
    #[serde(default)]
    pub next_date_reset: Option<f64>,
    #[serde(default)]
    pub subscription_info: Option<KiroSubscriptionInfo>,
    #[serde(default)]
    pub usage_breakdown_list: Vec<KiroUsageBreakdown>,
    #[serde(default)]
    pub overage_configuration: Option<KiroOverageConfiguration>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KiroSubscriptionInfo {
    #[serde(default)]
    pub subscription_title: Option<String>,
    #[serde(default)]
    pub overage_capability: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KiroOverageConfiguration {
    #[serde(default)]
    pub overage_enabled: Option<bool>,
    #[serde(default)]
    pub overage_status: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KiroUsageBreakdown {
    #[serde(default)]
    pub current_usage_with_precision: f64,
    #[serde(default)]
    pub bonuses: Vec<KiroBonus>,
    #[serde(default)]
    pub free_trial_info: Option<KiroFreeTrialInfo>,
    #[serde(default)]
    pub next_date_reset: Option<f64>,
    #[serde(default)]
    pub usage_limit_with_precision: f64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KiroBonus {
    #[serde(default)]
    pub current_usage: f64,
    #[serde(default)]
    pub usage_limit: f64,
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KiroFreeTrialInfo {
    #[serde(default)]
    pub current_usage_with_precision: f64,
    #[serde(default)]
    pub free_trial_status: Option<String>,
    #[serde(default)]
    pub usage_limit_with_precision: f64,
}

impl KiroBonus {
    pub fn is_active(&self) -> bool {
        self.status
            .as_deref()
            .map(|s| s == "ACTIVE")
            .unwrap_or(false)
    }
}

impl KiroFreeTrialInfo {
    pub fn is_active(&self) -> bool {
        self.free_trial_status
            .as_deref()
            .map(|s| s == "ACTIVE")
            .unwrap_or(false)
    }
}

impl KiroUsageLimitsResponse {
    pub fn subscription_title(&self) -> Option<&str> {
        self.subscription_info
            .as_ref()
            .and_then(|info| info.subscription_title.as_deref())
    }

    pub fn overage_enabled(&self) -> Option<bool> {
        let config = self.overage_configuration.as_ref()?;
        if let Some(enabled) = config.overage_enabled {
            return Some(enabled);
        }
        config
            .overage_status
            .as_deref()
            .map(|s| s.eq_ignore_ascii_case("ENABLED"))
    }

    pub fn primary_breakdown(&self) -> Option<&KiroUsageBreakdown> {
        self.usage_breakdown_list.first()
    }

    pub fn current_usage(&self) -> f64 {
        let Some(breakdown) = self.primary_breakdown() else {
            return 0.0;
        };
        let mut total = breakdown.current_usage_with_precision;
        if let Some(trial) = &breakdown.free_trial_info {
            if trial.is_active() {
                total += trial.current_usage_with_precision;
            }
        }
        for bonus in &breakdown.bonuses {
            if bonus.is_active() {
                total += bonus.current_usage;
            }
        }
        total
    }

    pub fn usage_limit(&self) -> f64 {
        let Some(breakdown) = self.primary_breakdown() else {
            return 0.0;
        };
        let mut total = breakdown.usage_limit_with_precision;
        if let Some(trial) = &breakdown.free_trial_info {
            if trial.is_active() {
                total += trial.usage_limit_with_precision;
            }
        }
        for bonus in &breakdown.bonuses {
            if bonus.is_active() {
                total += bonus.usage_limit;
            }
        }
        total
    }

    pub fn next_reset_timestamp(&self) -> Option<f64> {
        self.primary_breakdown()
            .and_then(|breakdown| breakdown.next_date_reset)
            .or(self.next_date_reset)
    }
}

#[derive(Debug, Clone)]
struct CachedAccessToken {
    token: String,
    expires_at_ms: i64,
}

impl CachedAccessToken {
    fn is_expiring_soon(&self) -> bool {
        self.expires_at_ms - chrono::Utc::now().timestamp_millis() < TOKEN_REFRESH_BUFFER_MS
    }
}

#[derive(Debug, Clone)]
struct PendingOAuthFlow {
    code_verifier: String,
    expires_at_ms: i64,
}

#[derive(Debug)]
enum FlowResult {
    Pending,
    Ready(Result<GitHubAccount, String>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KiroAccountData {
    pub account_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    pub refresh_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_arn: Option<String>,
    #[serde(default)]
    pub auth_region: String,
    #[serde(default)]
    pub api_region: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<String>,
    pub authenticated_at: i64,
}

impl From<&KiroAccountData> for GitHubAccount {
    fn from(data: &KiroAccountData) -> Self {
        GitHubAccount {
            id: data.account_id.clone(),
            login: data
                .email
                .clone()
                .unwrap_or_else(|| format!("Kiro ({})", short_id(&data.account_id))),
            avatar_url: None,
            authenticated_at: data.authenticated_at,
            github_domain: "kiro.dev".to_string(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct KiroOAuthStore {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    accounts: HashMap<String, KiroAccountData>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default_account_id: Option<String>,
}

#[derive(Clone)]
pub struct KiroOAuthManager {
    accounts: Arc<RwLock<HashMap<String, KiroAccountData>>>,
    default_account_id: Arc<RwLock<Option<String>>>,
    access_tokens: Arc<RwLock<HashMap<String, CachedAccessToken>>>,
    refresh_locks: Arc<RwLock<HashMap<String, Arc<Mutex<()>>>>>,
    pending_flows: Arc<RwLock<HashMap<String, PendingOAuthFlow>>>,
    flow_results: Arc<RwLock<HashMap<String, FlowResult>>>,
    active_flow_handle: Arc<Mutex<Option<JoinHandle<()>>>>,
    http_client: Client,
    storage_path: PathBuf,
}

impl KiroOAuthManager {
    pub fn new(data_dir: PathBuf) -> Self {
        let storage_path = data_dir.join("kiro_oauth_auth.json");
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
            log::warn!("[KiroOAuth] 加载存储失败: {e}");
        }
        manager
    }

    fn generate_code_verifier() -> String {
        use rand::RngCore;
        let mut bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut bytes);
        URL_SAFE_NO_PAD.encode(bytes)
    }

    fn generate_code_challenge(verifier: &str) -> String {
        let hash = Sha256::digest(verifier.as_bytes());
        URL_SAFE_NO_PAD.encode(hash)
    }

    fn generate_state() -> String {
        use rand::RngCore;
        let mut bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut bytes);
        URL_SAFE_NO_PAD.encode(bytes)
    }

    pub async fn start_browser_flow(&self) -> Result<KiroOAuthStartResponse, KiroOAuthError> {
        use tokio::net::TcpListener;

        let code_verifier = Self::generate_code_verifier();
        let code_challenge = Self::generate_code_challenge(&code_verifier);
        let state = Self::generate_state();
        let redirect_uri = format!("http://127.0.0.1:{CALLBACK_PORT}");
        let auth_url = format!(
            "{KIRO_PORTAL_URL}/signin?state={}&code_challenge={}&code_challenge_method=S256&redirect_uri={}&redirect_from=KiroIDE",
            urlencoding::encode(&state),
            urlencoding::encode(&code_challenge),
            urlencoding::encode(&redirect_uri),
        );

        {
            let mut handle_guard = self.active_flow_handle.lock().await;
            if let Some(prev) = handle_guard.take() {
                prev.abort();
                let _ = prev.await;
            }
        }
        self.flow_results.write().await.clear();

        let addr = format!("127.0.0.1:{CALLBACK_PORT}");
        let listener = TcpListener::bind(&addr).await.map_err(|e| {
            KiroOAuthError::CallbackServerError(format!("无法绑定回调端口 {CALLBACK_PORT}: {e}"))
        })?;

        let expires_at_ms =
            chrono::Utc::now().timestamp_millis() + (CALLBACK_TIMEOUT_SECS as i64) * 1000;
        {
            let mut pending = self.pending_flows.write().await;
            let now_ms = chrono::Utc::now().timestamp_millis();
            pending.retain(|_, flow| flow.expires_at_ms > now_ms);
            pending.insert(
                state.clone(),
                PendingOAuthFlow {
                    code_verifier,
                    expires_at_ms,
                },
            );
        }
        self.flow_results
            .write()
            .await
            .insert(state.clone(), FlowResult::Pending);

        let manager = self.clone();
        let state_clone = state.clone();
        let handle = tokio::spawn(async move {
            let result = manager
                .run_callback_on_listener(listener, &state_clone)
                .await;
            manager.flow_results.write().await.insert(
                state_clone,
                FlowResult::Ready(result.map_err(|e| e.to_string())),
            );
        });
        *self.active_flow_handle.lock().await = Some(handle);

        Ok(KiroOAuthStartResponse {
            auth_url,
            state,
            callback_port: CALLBACK_PORT,
        })
    }

    pub async fn poll_callback_result(
        &self,
        state: &str,
    ) -> Result<Option<GitHubAccount>, KiroOAuthError> {
        let mut results = self.flow_results.write().await;
        match results.get(state) {
            None => Err(KiroOAuthError::TokenFetchFailed(
                "未找到对应的 OAuth 流程（state 不匹配或已过期），请重新登录".to_string(),
            )),
            Some(FlowResult::Pending) => Ok(None),
            Some(FlowResult::Ready(_)) => {
                let entry = results.remove(state).expect("checked above");
                match entry {
                    FlowResult::Ready(Ok(account)) => Ok(Some(account)),
                    FlowResult::Ready(Err(e)) => Err(KiroOAuthError::TokenFetchFailed(e)),
                    FlowResult::Pending => Ok(None),
                }
            }
        }
    }

    async fn run_callback_on_listener(
        &self,
        listener: tokio::net::TcpListener,
        state: &str,
    ) -> Result<GitHubAccount, KiroOAuthError> {
        let result = tokio::time::timeout(
            tokio::time::Duration::from_secs(CALLBACK_TIMEOUT_SECS),
            async { Self::accept_callback(&listener).await },
        )
        .await
        .map_err(|_| KiroOAuthError::Timeout)??;

        if result.state != state {
            return Err(KiroOAuthError::TokenFetchFailed(
                "OAuth state 不匹配".to_string(),
            ));
        }

        let flow = self
            .pending_flows
            .write()
            .await
            .remove(state)
            .ok_or_else(|| KiroOAuthError::TokenFetchFailed("OAuth 流程已过期".to_string()))?;

        let redirect_uri = if result.login_option.is_empty() {
            format!("http://127.0.0.1:{CALLBACK_PORT}{}", result.path)
        } else {
            format!(
                "http://127.0.0.1:{CALLBACK_PORT}{}?login_option={}",
                result.path,
                urlencoding::encode(&result.login_option)
            )
        };
        let token = self
            .exchange_code_for_token(&result.code, &flow.code_verifier, &redirect_uri)
            .await?;
        let account = self.store_token_response(token).await?;
        Ok(GitHubAccount::from(&account))
    }

    async fn accept_callback(
        listener: &tokio::net::TcpListener,
    ) -> Result<KiroCallback, KiroOAuthError> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        loop {
            let (mut stream, _) = listener.accept().await?;
            let mut buf = vec![0u8; 8192];
            let n = stream.read(&mut buf).await?;
            let request = String::from_utf8_lossy(&buf[..n]);
            let first_line = request.lines().next().unwrap_or("");

            let path = first_line
                .strip_prefix("GET ")
                .and_then(|s| {
                    s.strip_suffix(" HTTP/1.1")
                        .or_else(|| s.strip_suffix(" HTTP/1.0"))
                })
                .unwrap_or("");

            if let Some(callback) = parse_callback(path) {
                let body = "<html><head><meta charset='utf-8'><title>Kiro OAuth</title></head><body style='font-family:sans-serif;text-align:center;padding:60px'><h2>Login complete</h2><p>You can close this tab and return to CC Switch.</p></body></html>";
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(), body
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.flush().await;
                return Ok(callback);
            }

            if path.contains("error=") {
                let body =
                    "<html><body>Login failed. Please close this tab and retry.</body></html>";
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(), body
                );
                let _ = stream.write_all(response.as_bytes()).await;
                return Err(KiroOAuthError::UserCancelled);
            }

            let _ = stream
                .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
                .await;
        }
    }

    async fn exchange_code_for_token(
        &self,
        code: &str,
        code_verifier: &str,
        redirect_uri: &str,
    ) -> Result<SocialTokenResponse, KiroOAuthError> {
        let url = format!("https://prod.{DEFAULT_REGION}.auth.desktop.kiro.dev/oauth/token");
        let resp = self
            .http_client
            .post(&url)
            .header("Content-Type", "application/json")
            .header("User-Agent", format!("KiroIDE-{DEFAULT_KIRO_VERSION}"))
            .header(
                "host",
                format!("prod.{DEFAULT_REGION}.auth.desktop.kiro.dev"),
            )
            .json(&serde_json::json!({
                "code": code,
                "code_verifier": code_verifier,
                "redirect_uri": redirect_uri,
                "invitation_code": null
            }))
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(KiroOAuthError::TokenFetchFailed(format!(
                "token exchange failed: {status} {body}"
            )));
        }

        resp.json::<SocialTokenResponse>()
            .await
            .map_err(|e| KiroOAuthError::ParseError(e.to_string()))
    }

    async fn store_token_response(
        &self,
        token: SocialTokenResponse,
    ) -> Result<KiroAccountData, KiroOAuthError> {
        let refresh_token = token.refresh_token.clone().ok_or_else(|| {
            KiroOAuthError::TokenFetchFailed("响应缺少 refresh_token".to_string())
        })?;
        let account_id = format!("kiro_{}", sha256_hex(&refresh_token)[..24].to_string());
        let account = KiroAccountData {
            account_id: account_id.clone(),
            email: None,
            refresh_token,
            profile_arn: token.profile_arn.clone(),
            auth_region: DEFAULT_REGION.to_string(),
            api_region: DEFAULT_REGION.to_string(),
            machine_id: Some(machine_id_from_refresh_token(
                token.refresh_token.as_deref().unwrap_or_default(),
            )),
            authenticated_at: chrono::Utc::now().timestamp(),
        };

        {
            let mut accounts = self.accounts.write().await;
            accounts.insert(account_id.clone(), account.clone());
        }
        {
            let mut default = self.default_account_id.write().await;
            if default.is_none() {
                *default = Some(account_id.clone());
            }
        }
        self.cache_access_token(&account_id, token).await;
        self.save_to_disk().await?;
        Ok(account)
    }

    async fn cache_access_token(&self, account_id: &str, token: SocialTokenResponse) {
        let expires_at_ms = token
            .expires_at
            .as_deref()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.timestamp_millis())
            .or_else(|| {
                token
                    .expires_in
                    .map(|s| chrono::Utc::now().timestamp_millis() + s * 1000)
            })
            .unwrap_or_else(|| chrono::Utc::now().timestamp_millis() + 15 * 60 * 1000);
        self.access_tokens.write().await.insert(
            account_id.to_string(),
            CachedAccessToken {
                token: token.access_token,
                expires_at_ms,
            },
        );
    }

    pub async fn get_valid_token_for_account(
        &self,
        account_id: &str,
    ) -> Result<String, KiroOAuthError> {
        if let Some(cached) = self.access_tokens.read().await.get(account_id).cloned() {
            if !cached.is_expiring_soon() {
                return Ok(cached.token);
            }
        }

        let lock = {
            let mut locks = self.refresh_locks.write().await;
            locks
                .entry(account_id.to_string())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let _guard = lock.lock().await;

        if let Some(cached) = self.access_tokens.read().await.get(account_id).cloned() {
            if !cached.is_expiring_soon() {
                return Ok(cached.token);
            }
        }

        let account = self
            .accounts
            .read()
            .await
            .get(account_id)
            .cloned()
            .ok_or_else(|| KiroOAuthError::AccountNotFound(account_id.to_string()))?;

        let token = self.refresh_social_token(&account).await?;
        let access_token = token.access_token.clone();

        {
            let mut accounts = self.accounts.write().await;
            if let Some(existing) = accounts.get_mut(account_id) {
                if let Some(new_refresh_token) = token.refresh_token.clone() {
                    existing.refresh_token = new_refresh_token;
                    existing.machine_id =
                        Some(machine_id_from_refresh_token(&existing.refresh_token));
                }
                if let Some(profile_arn) = token.profile_arn.clone() {
                    existing.profile_arn = Some(profile_arn);
                }
            }
        }
        self.cache_access_token(account_id, token).await;
        self.save_to_disk().await?;
        Ok(access_token)
    }

    async fn refresh_social_token(
        &self,
        account: &KiroAccountData,
    ) -> Result<SocialTokenResponse, KiroOAuthError> {
        let region = if account.auth_region.trim().is_empty() {
            DEFAULT_REGION
        } else {
            account.auth_region.as_str()
        };
        let url = format!("https://prod.{region}.auth.desktop.kiro.dev/refreshToken");
        let machine_id = account
            .machine_id
            .clone()
            .unwrap_or_else(|| machine_id_from_refresh_token(&account.refresh_token));

        let resp = self
            .http_client
            .post(&url)
            .header("Accept", "application/json, text/plain, */*")
            .header("Content-Type", "application/json")
            .header(
                "User-Agent",
                format!("KiroIDE-{DEFAULT_KIRO_VERSION}-{machine_id}"),
            )
            .header("host", format!("prod.{region}.auth.desktop.kiro.dev"))
            .json(&serde_json::json!({ "refreshToken": account.refresh_token }))
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            if status.as_u16() == 400 && body.contains("invalid_grant") {
                return Err(KiroOAuthError::RefreshTokenInvalid);
            }
            return Err(KiroOAuthError::TokenFetchFailed(format!(
                "refresh failed: {status} {body}"
            )));
        }

        resp.json::<SocialTokenResponse>()
            .await
            .map_err(|e| KiroOAuthError::ParseError(e.to_string()))
    }

    pub async fn get_valid_token(&self) -> Result<String, KiroOAuthError> {
        match self.resolve_default_account_id().await {
            Some(id) => self.get_valid_token_for_account(&id).await,
            None => Err(KiroOAuthError::AccountNotFound(
                "未找到可用 Kiro 账号".to_string(),
            )),
        }
    }

    pub async fn default_account_id(&self) -> Option<String> {
        self.resolve_default_account_id().await
    }

    pub async fn get_account(&self, account_id: &str) -> Option<KiroAccountData> {
        self.accounts.read().await.get(account_id).cloned()
    }

    pub async fn get_default_account(&self) -> Option<KiroAccountData> {
        let id = self.resolve_default_account_id().await?;
        self.get_account(&id).await
    }

    pub async fn invalidate_cached_token(&self, account_id: &str) {
        self.access_tokens.write().await.remove(account_id);
    }

    pub async fn get_usage_limits_for_account(
        &self,
        account_id: &str,
    ) -> Result<KiroUsageLimitsResponse, KiroOAuthError> {
        let account = self
            .get_account(account_id)
            .await
            .ok_or_else(|| KiroOAuthError::AccountNotFound(account_id.to_string()))?;
        let token = self.get_valid_token_for_account(account_id).await?;
        let response = self.send_usage_limits_request(&account, &token).await?;

        let response = if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            self.invalidate_cached_token(account_id).await;
            let token = self.get_valid_token_for_account(account_id).await?;
            self.send_usage_limits_request(&account, &token).await?
        } else {
            response
        };

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(KiroOAuthError::TokenFetchFailed(format!(
                "usage limits failed: {status} {body}"
            )));
        }

        response
            .json::<KiroUsageLimitsResponse>()
            .await
            .map_err(|e| KiroOAuthError::ParseError(e.to_string()))
    }

    async fn send_usage_limits_request(
        &self,
        account: &KiroAccountData,
        token: &str,
    ) -> Result<reqwest::Response, KiroOAuthError> {
        let region = if account.api_region.trim().is_empty() {
            DEFAULT_REGION
        } else {
            account.api_region.as_str()
        };
        let host = format!("q.{region}.amazonaws.com");
        let mut url = format!(
            "https://{host}/getUsageLimits?origin=AI_EDITOR&resourceType=AGENTIC_REQUEST&isEmailRequired=true"
        );
        if let Some(profile_arn) = account
            .profile_arn
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            url.push_str("&profileArn=");
            url.push_str(&urlencoding::encode(profile_arn));
        }

        let machine_id = account
            .machine_id
            .clone()
            .unwrap_or_else(|| machine_id_from_refresh_token(&account.refresh_token));
        let user_agent = format!(
            "aws-sdk-js/1.0.0 ua/2.1 os/macos lang/js md/nodejs#22.22.0 api/codewhispererruntime#1.0.0 m/N,E KiroIDE-{DEFAULT_KIRO_VERSION}-{machine_id}"
        );
        let amz_user_agent =
            format!("aws-sdk-js/1.0.0 KiroIDE-{DEFAULT_KIRO_VERSION}-{machine_id}");

        self.http_client
            .get(url)
            .header("x-amz-user-agent", amz_user_agent)
            .header("user-agent", user_agent)
            .header("host", host)
            .header("amz-sdk-invocation-id", uuid::Uuid::new_v4().to_string())
            .header("amz-sdk-request", "attempt=1; max=1")
            .header("Authorization", format!("Bearer {token}"))
            .header("Connection", "close")
            .send()
            .await
            .map_err(KiroOAuthError::from)
    }

    pub async fn remove_account(&self, account_id: &str) -> Result<(), KiroOAuthError> {
        {
            let mut accounts = self.accounts.write().await;
            if accounts.remove(account_id).is_none() {
                return Err(KiroOAuthError::AccountNotFound(account_id.to_string()));
            }
        }
        self.access_tokens.write().await.remove(account_id);
        {
            let accounts = self.accounts.read().await;
            let mut default = self.default_account_id.write().await;
            if default.as_deref() == Some(account_id) {
                *default = Self::fallback_default_account_id(&accounts);
            }
        }
        self.save_to_disk().await
    }

    pub async fn set_default_account(&self, account_id: &str) -> Result<(), KiroOAuthError> {
        if !self.accounts.read().await.contains_key(account_id) {
            return Err(KiroOAuthError::AccountNotFound(account_id.to_string()));
        }
        *self.default_account_id.write().await = Some(account_id.to_string());
        self.save_to_disk().await
    }

    pub async fn logout(&self) -> Result<(), KiroOAuthError> {
        self.accounts.write().await.clear();
        self.access_tokens.write().await.clear();
        *self.default_account_id.write().await = None;
        self.save_to_disk().await
    }

    pub async fn get_status(&self) -> KiroOAuthStatus {
        let accounts = self.accounts.read().await;
        let default_account_id = self.resolve_default_account_id().await;
        KiroOAuthStatus {
            authenticated: !accounts.is_empty(),
            default_account_id: default_account_id.clone(),
            accounts: Self::sorted_public_accounts(&accounts, default_account_id.as_deref()),
        }
    }

    fn fallback_default_account_id(accounts: &HashMap<String, KiroAccountData>) -> Option<String> {
        accounts.keys().min().cloned()
    }

    fn sorted_public_accounts(
        accounts: &HashMap<String, KiroAccountData>,
        default_account_id: Option<&str>,
    ) -> Vec<GitHubAccount> {
        let mut out: Vec<_> = accounts.values().map(GitHubAccount::from).collect();
        out.sort_by(|a, b| {
            let a_default = default_account_id == Some(a.id.as_str());
            let b_default = default_account_id == Some(b.id.as_str());
            b_default
                .cmp(&a_default)
                .then_with(|| a.login.cmp(&b.login))
                .then_with(|| a.id.cmp(&b.id))
        });
        out
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

    async fn save_to_disk(&self) -> Result<(), KiroOAuthError> {
        let accounts = self.accounts.read().await.clone();
        let default_account_id = self.resolve_default_account_id().await;
        let store = KiroOAuthStore {
            version: 1,
            accounts,
            default_account_id,
        };
        if let Some(parent) = self.storage_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = self.storage_path.with_extension("json.tmp");
        let json = serde_json::to_vec_pretty(&store)
            .map_err(|e| KiroOAuthError::ParseError(e.to_string()))?;
        {
            let mut file = fs::File::create(&tmp)?;
            file.write_all(&json)?;
            file.sync_all()?;
        }
        fs::rename(tmp, &self.storage_path)?;
        Ok(())
    }

    fn load_from_disk_sync(&self) -> Result<(), KiroOAuthError> {
        if !self.storage_path.exists() {
            return Ok(());
        }
        let content = fs::read_to_string(&self.storage_path)?;
        if content.trim().is_empty() {
            return Ok(());
        }
        let mut store: KiroOAuthStore = serde_json::from_str(&content)
            .map_err(|e| KiroOAuthError::ParseError(e.to_string()))?;
        for account in store.accounts.values_mut() {
            if account.auth_region.is_empty() {
                account.auth_region = DEFAULT_REGION.to_string();
            }
            if account.api_region.is_empty() {
                account.api_region = DEFAULT_REGION.to_string();
            }
            if account.machine_id.is_none() {
                account.machine_id = Some(machine_id_from_refresh_token(&account.refresh_token));
            }
        }
        if let Ok(mut accounts) = self.accounts.try_write() {
            *accounts = store.accounts;
        }
        if let Ok(mut default) = self.default_account_id.try_write() {
            *default = store.default_account_id;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct KiroOAuthStartResponse {
    pub auth_url: String,
    pub state: String,
    pub callback_port: u16,
}

#[derive(Debug, Clone, Serialize)]
pub struct KiroOAuthStatus {
    pub authenticated: bool,
    pub default_account_id: Option<String>,
    pub accounts: Vec<GitHubAccount>,
}

#[derive(Debug)]
struct KiroCallback {
    code: String,
    path: String,
    login_option: String,
    state: String,
}

fn parse_callback(path_and_query: &str) -> Option<KiroCallback> {
    let (path, query) = path_and_query.split_once('?')?;
    if path != CALLBACK_PATH && path != "/oauth/callback" && path != "/signin/callback" {
        return None;
    }
    let params = parse_query_string(query);
    if params.contains_key("error") {
        return None;
    }
    Some(KiroCallback {
        code: params.get("code")?.clone(),
        path: path.to_string(),
        login_option: params.get("login_option").cloned().unwrap_or_default(),
        state: params.get("state").cloned().unwrap_or_default(),
    })
}

fn parse_query_string(query: &str) -> HashMap<String, String> {
    query
        .split('&')
        .filter_map(|pair| {
            let mut iter = pair.splitn(2, '=');
            let key = iter.next()?.to_string();
            let raw = iter.next().unwrap_or_default().replace('+', " ");
            let value = urlencoding::decode(&raw)
                .map(|s| s.into_owned())
                .unwrap_or(raw);
            Some((key, value))
        })
        .collect()
}

pub fn machine_id_from_refresh_token(refresh_token: &str) -> String {
    sha256_hex(&format!("KotlinNativeAPI/{refresh_token}"))
}

fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn short_id(value: &str) -> String {
    if value.len() > 12 {
        format!("{}...{}", &value[..6], &value[value.len() - 4..])
    } else {
        value.to_string()
    }
}
