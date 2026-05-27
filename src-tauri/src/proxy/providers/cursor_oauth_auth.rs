//! Cursor OAuth Authentication Module.
//!
//! Implements Cursor deep-control browser login with multi-account management.
//! Providers bind to accounts through `meta.authBinding` using
//! `auth_provider = "cursor_oauth"`.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

use super::copilot_auth::{GitHubAccount, GitHubDeviceCodeResponse};

pub const CURSOR_CLIENT_ID: &str = "KbZUR41cY7W6zRSdpSUJ7I7mLYBKOCmB";
pub const DEFAULT_CURSOR_CLIENT_VERSION: &str = "cli-2026.01.09-231024f";

const LOGIN_URL: &str = "https://www.cursor.com/loginDeepControl";
const POLL_URL: &str = "https://api2.cursor.sh/auth/poll";
const TOKEN_URL: &str = "https://api2.cursor.sh/oauth/token";
const USER_INFO_URL: &str = "https://api.cursor.com/v0/me";
const TOKEN_REFRESH_BUFFER_MS: i64 = 60_000;
const BROWSER_FLOW_TIMEOUT_SECS: i64 = 300;

#[derive(Debug, thiserror::Error)]
pub enum CursorOAuthError {
    #[error("等待用户授权中")]
    AuthorizationPending,
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
}

impl From<reqwest::Error> for CursorOAuthError {
    fn from(err: reqwest::Error) -> Self {
        CursorOAuthError::NetworkError(err.to_string())
    }
}

impl From<std::io::Error> for CursorOAuthError {
    fn from(err: std::io::Error) -> Self {
        CursorOAuthError::IoError(err.to_string())
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
struct PendingBrowserFlow {
    verifier: String,
    expires_at_ms: i64,
}

#[derive(Debug, Clone, Deserialize)]
struct CursorPollResponse {
    #[serde(default, alias = "accessToken")]
    access_token: Option<String>,
    #[serde(default, alias = "refreshToken")]
    refresh_token: Option<String>,
    #[serde(default, alias = "idToken")]
    id_token: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default, alias = "authId")]
    auth_id: Option<String>,
    #[serde(default, alias = "apiKey")]
    api_key: Option<String>,
    #[serde(flatten)]
    extra: HashMap<String, serde_json::Value>,
}

impl CursorPollResponse {
    fn access_token(&self) -> Option<&str> {
        self.access_token.as_deref().or(self.api_key.as_deref())
    }

    fn refresh_token(&self) -> Option<&str> {
        self.refresh_token.as_deref()
    }

    fn auth_id(&self) -> Option<&str> {
        self.auth_id.as_deref()
    }

    fn display_email(&self) -> Option<String> {
        self.email
            .as_deref()
            .and_then(valid_email)
            .map(ToString::to_string)
            .or_else(|| self.id_token.as_deref().and_then(email_from_jwt))
            .or_else(|| find_email_in_map(&self.extra))
    }
}

#[derive(Debug, Clone, Deserialize)]
struct CursorRefreshResponse {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default, rename = "shouldLogout")]
    should_logout: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CursorAccountData {
    pub account_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    pub refresh_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor_service_machine_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor_client_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor_config_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor_client_id: Option<String>,
    pub authenticated_at: i64,
}

impl CursorAccountData {
    pub fn machine_id(&self) -> &str {
        self.cursor_service_machine_id
            .as_deref()
            .unwrap_or(self.account_id.as_str())
    }

    pub fn client_version(&self) -> &str {
        self.cursor_client_version
            .as_deref()
            .unwrap_or(DEFAULT_CURSOR_CLIENT_VERSION)
    }

    pub fn config_version(&self) -> String {
        self.cursor_config_version
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
    }

    pub fn client_id(&self) -> &str {
        self.cursor_client_id.as_deref().unwrap_or(CURSOR_CLIENT_ID)
    }
}

impl From<&CursorAccountData> for GitHubAccount {
    fn from(data: &CursorAccountData) -> Self {
        let display_email = data.email.as_deref().and_then(valid_email);
        GitHubAccount {
            id: data.account_id.clone(),
            login: display_email
                .map(|email| format!("Cursor({email})"))
                .unwrap_or_else(|| format!("Cursor({})", short_id(&data.account_id))),
            email: display_email.map(ToString::to_string),
            avatar_url: None,
            authenticated_at: data.authenticated_at,
            github_domain: "cursor.com".to_string(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct CursorOAuthStore {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    accounts: HashMap<String, CursorAccountData>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default_account_id: Option<String>,
}

#[derive(Clone)]
pub struct CursorOAuthManager {
    accounts: Arc<RwLock<HashMap<String, CursorAccountData>>>,
    default_account_id: Arc<RwLock<Option<String>>>,
    access_tokens: Arc<RwLock<HashMap<String, CachedAccessToken>>>,
    refresh_locks: Arc<RwLock<HashMap<String, Arc<Mutex<()>>>>>,
    pending_flows: Arc<RwLock<HashMap<String, PendingBrowserFlow>>>,
    http_client: Client,
    storage_path: PathBuf,
}

impl CursorOAuthManager {
    pub fn new(data_dir: PathBuf) -> Self {
        let storage_path = data_dir.join("cursor_oauth_auth.json");
        let http_client = Client::builder()
            .http2_adaptive_window(true)
            .build()
            .unwrap_or_else(|_| Client::new());
        let manager = Self {
            accounts: Arc::new(RwLock::new(HashMap::new())),
            default_account_id: Arc::new(RwLock::new(None)),
            access_tokens: Arc::new(RwLock::new(HashMap::new())),
            refresh_locks: Arc::new(RwLock::new(HashMap::new())),
            pending_flows: Arc::new(RwLock::new(HashMap::new())),
            http_client,
            storage_path,
        };
        if let Err(e) = manager.load_from_disk_sync() {
            log::warn!("[CursorOAuth] 加载存储失败: {e}");
        }
        manager
    }

    pub async fn start_browser_flow(&self) -> Result<CursorOAuthStartResponse, CursorOAuthError> {
        let verifier = generate_code_verifier();
        let challenge = generate_code_challenge(&verifier);
        let state = uuid::Uuid::new_v4().to_string();
        let mut url =
            url::Url::parse(LOGIN_URL).map_err(|e| CursorOAuthError::ParseError(e.to_string()))?;
        url.query_pairs_mut()
            .append_pair("challenge", &challenge)
            .append_pair("uuid", &state)
            .append_pair("mode", "login")
            .append_pair("redirectTarget", "cli");

        let expires_at_ms =
            chrono::Utc::now().timestamp_millis() + BROWSER_FLOW_TIMEOUT_SECS * 1000;
        {
            let now_ms = chrono::Utc::now().timestamp_millis();
            let mut pending = self.pending_flows.write().await;
            pending.retain(|_, flow| flow.expires_at_ms > now_ms);
            pending.insert(
                state.clone(),
                PendingBrowserFlow {
                    verifier,
                    expires_at_ms,
                },
            );
        }

        Ok(CursorOAuthStartResponse {
            auth_url: url.to_string(),
            state,
        })
    }

    pub async fn poll_callback_result(
        &self,
        state: &str,
    ) -> Result<Option<GitHubAccount>, CursorOAuthError> {
        let flow = {
            let pending = self.pending_flows.read().await;
            pending.get(state).cloned()
        }
        .ok_or_else(|| {
            CursorOAuthError::TokenFetchFailed(
                "未找到对应的 Cursor OAuth 流程（state 不匹配或已过期），请重新登录".to_string(),
            )
        })?;

        if flow.expires_at_ms <= chrono::Utc::now().timestamp_millis() {
            self.pending_flows.write().await.remove(state);
            return Err(CursorOAuthError::Timeout);
        }

        let poll = match self.poll_auth_once(state, &flow.verifier).await? {
            Some(poll) => poll,
            None => return Ok(None),
        };
        let account = self.store_poll_response(state, poll).await?;
        self.pending_flows.write().await.remove(state);
        Ok(Some(GitHubAccount::from(&account)))
    }

    async fn poll_auth_once(
        &self,
        state: &str,
        verifier: &str,
    ) -> Result<Option<CursorPollResponse>, CursorOAuthError> {
        let mut url =
            url::Url::parse(POLL_URL).map_err(|e| CursorOAuthError::ParseError(e.to_string()))?;
        url.query_pairs_mut()
            .append_pair("uuid", state)
            .append_pair("verifier", verifier);
        let resp = self
            .http_client
            .get(url)
            .header("Accept", "application/json")
            .header(
                "User-Agent",
                format!("Cursor/{DEFAULT_CURSOR_CLIENT_VERSION} (cc-switch browser login)"),
            )
            .send()
            .await?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND
            || resp.status() == reqwest::StatusCode::ACCEPTED
        {
            return Ok(None);
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(CursorOAuthError::TokenFetchFailed(format!(
                "auth poll failed: {status} {body}"
            )));
        }
        let text = resp.text().await.map_err(CursorOAuthError::from)?;
        if text.trim().is_empty() {
            return Ok(None);
        }
        let parsed = serde_json::from_str::<CursorPollResponse>(&text)
            .map_err(|e| CursorOAuthError::ParseError(e.to_string()))?;
        if parsed.access_token().is_none() {
            return Ok(None);
        }
        Ok(Some(parsed))
    }

    async fn store_poll_response(
        &self,
        state: &str,
        poll: CursorPollResponse,
    ) -> Result<CursorAccountData, CursorOAuthError> {
        let access_token = poll
            .access_token()
            .ok_or_else(|| CursorOAuthError::TokenFetchFailed("响应缺少 accessToken".to_string()))?
            .to_string();
        let refresh_token = poll.refresh_token().ok_or_else(|| {
            CursorOAuthError::TokenFetchFailed(
                "Cursor 登录完成但未返回 refreshToken，请重新登录并选择常规 Cursor 登录"
                    .to_string(),
            )
        })?;
        let account_id = format!("cursor_{}", &sha256_hex(refresh_token)[..24]);
        let mut account = CursorAccountData {
            account_id: account_id.clone(),
            email: None,
            refresh_token: refresh_token.to_string(),
            id_token: poll.id_token.clone(),
            cursor_service_machine_id: Some(state.to_string()),
            cursor_client_version: Some(DEFAULT_CURSOR_CLIENT_VERSION.to_string()),
            cursor_config_version: Some(uuid::Uuid::new_v4().to_string()),
            cursor_client_id: Some(CURSOR_CLIENT_ID.to_string()),
            authenticated_at: chrono::Utc::now().timestamp(),
        };
        account.email = poll
            .display_email()
            .or_else(|| email_from_jwt(&access_token))
            .or_else(|| poll.auth_id().and_then(email_from_auth_id))
            .or(self
                .fetch_user_email(&account, &access_token)
                .await
                .ok()
                .flatten());

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
        self.cache_access_token(&account_id, &access_token).await;
        self.save_to_disk().await?;
        Ok(account)
    }

    async fn cache_access_token(&self, account_id: &str, access_token: &str) {
        let expires_at_ms = expiry_from_jwt_ms(access_token)
            .unwrap_or_else(|| chrono::Utc::now().timestamp_millis() + 55 * 60 * 1000);
        self.access_tokens.write().await.insert(
            account_id.to_string(),
            CachedAccessToken {
                token: access_token.to_string(),
                expires_at_ms,
            },
        );
    }

    pub async fn get_valid_token_for_account(
        &self,
        account_id: &str,
    ) -> Result<String, CursorOAuthError> {
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
            .ok_or_else(|| CursorOAuthError::AccountNotFound(account_id.to_string()))?;
        let token = self.refresh_token(&account).await?;
        let access_token = token.access_token.clone().ok_or_else(|| {
            CursorOAuthError::TokenFetchFailed("refresh response missing access_token".to_string())
        })?;

        if token.should_logout.unwrap_or(false) {
            return Err(CursorOAuthError::RefreshTokenInvalid);
        }

        let refreshed_email = if account.email.is_none() {
            email_from_jwt(&access_token)
                .or_else(|| token.id_token.as_deref().and_then(email_from_jwt))
                .or(self
                    .fetch_user_email(&account, &access_token)
                    .await
                    .ok()
                    .flatten())
        } else {
            None
        };

        {
            let mut accounts = self.accounts.write().await;
            if let Some(existing) = accounts.get_mut(account_id) {
                if let Some(refresh_token) = token.refresh_token.clone() {
                    existing.refresh_token = refresh_token;
                }
                if let Some(id_token) = token.id_token.clone() {
                    existing.id_token = Some(id_token);
                }
                if existing.email.is_none() {
                    existing.email = refreshed_email;
                }
            }
        }
        self.cache_access_token(account_id, &access_token).await;
        self.save_to_disk().await?;
        Ok(access_token)
    }

    async fn refresh_token(
        &self,
        account: &CursorAccountData,
    ) -> Result<CursorRefreshResponse, CursorOAuthError> {
        let resp = self
            .http_client
            .post(TOKEN_URL)
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({
                "grant_type": "refresh_token",
                "client_id": account.client_id(),
                "refresh_token": account.refresh_token,
            }))
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            if status == reqwest::StatusCode::BAD_REQUEST && body.contains("invalid") {
                return Err(CursorOAuthError::RefreshTokenInvalid);
            }
            return Err(CursorOAuthError::TokenFetchFailed(format!(
                "refresh failed: {status} {body}"
            )));
        }
        resp.json::<CursorRefreshResponse>()
            .await
            .map_err(|e| CursorOAuthError::ParseError(e.to_string()))
    }

    async fn fetch_user_email(
        &self,
        account: &CursorAccountData,
        access_token: &str,
    ) -> Result<Option<String>, CursorOAuthError> {
        let mut req = self
            .http_client
            .get(USER_INFO_URL)
            .bearer_auth(access_token)
            .header("Accept", "application/json")
            .header(
                "User-Agent",
                format!("Cursor/{DEFAULT_CURSOR_CLIENT_VERSION} (cc-switch user info)"),
            );
        for (key, value) in super::cursor_protocol::cursor_identity_headers(account, access_token) {
            req = req.header(key, value);
        }
        let resp = req.send().await?;
        if !resp.status().is_success() {
            return Ok(None);
        }
        let value = resp
            .json::<serde_json::Value>()
            .await
            .map_err(|e| CursorOAuthError::ParseError(e.to_string()))?;
        Ok(find_email_in_value(&value))
    }

    pub async fn get_valid_token(&self) -> Result<String, CursorOAuthError> {
        match self.resolve_default_account_id().await {
            Some(id) => self.get_valid_token_for_account(&id).await,
            None => Err(CursorOAuthError::AccountNotFound(
                "未找到可用 Cursor 账号".to_string(),
            )),
        }
    }

    pub async fn default_account_id(&self) -> Option<String> {
        self.resolve_default_account_id().await
    }

    pub async fn get_account(&self, account_id: &str) -> Option<CursorAccountData> {
        self.accounts.read().await.get(account_id).cloned()
    }

    pub async fn get_default_account(&self) -> Option<CursorAccountData> {
        let id = self.resolve_default_account_id().await?;
        self.get_account(&id).await
    }

    pub async fn invalidate_cached_token(&self, account_id: &str) {
        self.access_tokens.write().await.remove(account_id);
    }

    pub async fn remove_account(&self, account_id: &str) -> Result<(), CursorOAuthError> {
        {
            let mut accounts = self.accounts.write().await;
            if accounts.remove(account_id).is_none() {
                return Err(CursorOAuthError::AccountNotFound(account_id.to_string()));
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

    pub async fn set_default_account(&self, account_id: &str) -> Result<(), CursorOAuthError> {
        if !self.accounts.read().await.contains_key(account_id) {
            return Err(CursorOAuthError::AccountNotFound(account_id.to_string()));
        }
        *self.default_account_id.write().await = Some(account_id.to_string());
        self.save_to_disk().await
    }

    pub async fn clear_auth(&self) -> Result<(), CursorOAuthError> {
        self.accounts.write().await.clear();
        self.access_tokens.write().await.clear();
        *self.default_account_id.write().await = None;
        self.save_to_disk().await
    }

    pub async fn get_status(&self) -> CursorOAuthStatus {
        self.hydrate_missing_account_emails().await;
        let accounts = self.accounts.read().await;
        let default_account_id = self.resolve_default_account_id().await;
        CursorOAuthStatus {
            authenticated: !accounts.is_empty(),
            default_account_id: default_account_id.clone(),
            accounts: Self::sorted_public_accounts(&accounts, default_account_id.as_deref()),
        }
    }

    async fn hydrate_missing_account_emails(&self) {
        let account_items: Vec<(CursorAccountData, bool)> = {
            let accounts = self.accounts.read().await;
            let allow_single_account_local_fallback = accounts.len() == 1;
            accounts
                .values()
                .filter(|account| account.email.is_none())
                .cloned()
                .map(|account| (account, allow_single_account_local_fallback))
                .collect()
        };
        if account_items.is_empty() {
            return;
        }

        let mut changed = false;
        for (account, allow_single_account_local_fallback) in account_items {
            let mut email = None;
            if let Ok(token) = self.get_valid_token_for_account(&account.account_id).await {
                email = email_from_jwt(&token).or(self
                    .fetch_user_email(&account, &token)
                    .await
                    .ok()
                    .flatten());
            }
            if email.is_none() {
                email = Self::cursor_local_cached_email_for_account(
                    account.clone(),
                    allow_single_account_local_fallback,
                )
                .await;
            }
            let Some(email) = email else {
                continue;
            };

            let mut accounts = self.accounts.write().await;
            let Some(existing) = accounts.get_mut(&account.account_id) else {
                continue;
            };
            if existing.email.is_none() {
                existing.email = Some(email);
                changed = true;
            }
        }

        if changed {
            let _ = self.save_to_disk().await;
        }
    }

    async fn cursor_local_cached_email_for_account(
        account: CursorAccountData,
        allow_single_account_fallback: bool,
    ) -> Option<String> {
        tokio::task::spawn_blocking(move || {
            read_cursor_local_cached_email_for_account(&account, allow_single_account_fallback)
        })
        .await
        .ok()
        .flatten()
    }

    fn fallback_default_account_id(
        accounts: &HashMap<String, CursorAccountData>,
    ) -> Option<String> {
        accounts.keys().min().cloned()
    }

    fn sorted_public_accounts(
        accounts: &HashMap<String, CursorAccountData>,
        default_account_id: Option<&str>,
    ) -> Vec<GitHubAccount> {
        let mut out: Vec<GitHubAccount> = accounts.values().map(GitHubAccount::from).collect();
        out.sort_by(|a, b| {
            let a_default = default_account_id == Some(a.id.as_str());
            let b_default = default_account_id == Some(b.id.as_str());
            b_default
                .cmp(&a_default)
                .then_with(|| a.login.to_lowercase().cmp(&b.login.to_lowercase()))
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

    fn load_from_disk_sync(&self) -> Result<(), CursorOAuthError> {
        if !self.storage_path.exists() {
            return Ok(());
        }
        let content = fs::read_to_string(&self.storage_path)?;
        if content.trim().is_empty() {
            return Ok(());
        }
        let store: CursorOAuthStore = serde_json::from_str(&content)
            .map_err(|e| CursorOAuthError::ParseError(e.to_string()))?;
        if let Ok(mut accounts) = self.accounts.try_write() {
            *accounts = store.accounts;
            if let Ok(mut default) = self.default_account_id.try_write() {
                *default = store.default_account_id;
                if default
                    .as_ref()
                    .is_some_and(|id| !accounts.contains_key(id.as_str()))
                {
                    *default = Self::fallback_default_account_id(&accounts);
                }
            }
        }
        Ok(())
    }

    async fn save_to_disk(&self) -> Result<(), CursorOAuthError> {
        let accounts = self.accounts.read().await.clone();
        let default_account_id = self.resolve_default_account_id().await;
        let store = CursorOAuthStore {
            version: 1,
            accounts,
            default_account_id,
        };
        if let Some(parent) = self.storage_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_string_pretty(&store)
            .map_err(|e| CursorOAuthError::ParseError(e.to_string()))?;
        let tmp_path = self.storage_path.with_extension("json.tmp");
        {
            let mut file = fs::File::create(&tmp_path)?;
            file.write_all(content.as_bytes())?;
            file.sync_all()?;
        }
        fs::rename(tmp_path, &self.storage_path)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CursorOAuthStartResponse {
    pub auth_url: String,
    pub state: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CursorOAuthStatus {
    pub authenticated: bool,
    pub default_account_id: Option<String>,
    pub accounts: Vec<GitHubAccount>,
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

fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    hex::encode(hasher.finalize())
}

fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

fn decode_jwt_payload(token: &str) -> Option<serde_json::Value> {
    let payload = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload.as_bytes()).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn expiry_from_jwt_ms(token: &str) -> Option<i64> {
    decode_jwt_payload(token)?
        .get("exp")?
        .as_i64()
        .map(|exp| exp * 1000)
}

/// Extract the WorkOS user id from a Cursor access token's `sub` claim.
/// Cursor encodes it as e.g. `auth0|user_01XYZ`; the WorkOS session cookie
/// needs only the trailing id, not the provider prefix.
pub fn workos_user_id_from_token(token: &str) -> Option<String> {
    let claims = decode_jwt_payload(token)?;
    let sub = claims.get("sub")?.as_str()?;
    let id = sub.rsplit('|').next().unwrap_or(sub).trim();
    if id.is_empty() {
        None
    } else {
        Some(id.to_string())
    }
}

fn email_from_jwt(token: &str) -> Option<String> {
    let claims = decode_jwt_payload(token)?;
    ["email", "preferred_username", "upn"]
        .iter()
        .find_map(|key| claims.get(*key).and_then(|v| v.as_str()))
        .and_then(valid_email)
        .map(ToString::to_string)
}

fn email_from_auth_id(auth_id: &str) -> Option<String> {
    valid_email(auth_id)
        .or_else(|| auth_id.split('|').find_map(valid_email))
        .map(ToString::to_string)
}

fn valid_email(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if trimmed.len() < 3 || trimmed.contains(char::is_whitespace) {
        return None;
    }
    let (local, domain) = trimmed.split_once('@')?;
    if local.is_empty() || domain.is_empty() || !domain.contains('.') {
        return None;
    }
    Some(trimmed)
}

fn find_email_in_map(map: &HashMap<String, serde_json::Value>) -> Option<String> {
    map.values().find_map(find_email_in_value)
}

fn find_email_in_value(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(value) => valid_email(value).map(ToString::to_string),
        serde_json::Value::Array(values) => values.iter().find_map(find_email_in_value),
        serde_json::Value::Object(map) => {
            for key in [
                "email",
                "accountEmail",
                "account_email",
                "preferred_email",
                "preferredEmail",
                "preferredUsername",
                "preferred_username",
                "userEmail",
                "user_email",
            ] {
                if let Some(email) = map
                    .get(key)
                    .and_then(|value| value.as_str())
                    .and_then(valid_email)
                {
                    return Some(email.to_string());
                }
            }
            map.values().find_map(find_email_in_value)
        }
        _ => None,
    }
}

fn read_cursor_local_cached_email_for_account(
    account: &CursorAccountData,
    allow_single_account_fallback: bool,
) -> Option<String> {
    let storage_path = default_cursor_storage_path()?;
    let storage = read_cursor_local_storage(&storage_path).ok()?;
    let email = storage
        .get("cursorAuth/cachedEmail")
        .and_then(|value| valid_email(value))
        .map(ToString::to_string)
        .or_else(|| {
            storage
                .get("cursorAuth/accessToken")
                .and_then(|token| email_from_jwt(token))
        })?;

    let refresh_token_matches = storage
        .get("cursorAuth/refreshToken")
        .map(|refresh_token| refresh_token == &account.refresh_token)
        .unwrap_or(false);

    if refresh_token_matches || allow_single_account_fallback {
        Some(email)
    } else {
        None
    }
}

fn default_cursor_storage_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        return dirs::home_dir().map(|home| {
            home.join("Library/Application Support/Cursor/User/globalStorage/state.vscdb")
        });
    }

    #[cfg(target_os = "windows")]
    {
        return dirs::data_dir().map(|data_dir| {
            data_dir
                .join("Cursor")
                .join("User")
                .join("globalStorage")
                .join("state.vscdb")
        });
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        dirs::home_dir().map(|home| {
            home.join(".config")
                .join("Cursor")
                .join("User")
                .join("globalStorage")
                .join("state.vscdb")
        })
    }
}

fn read_cursor_local_storage(path: &Path) -> Result<HashMap<String, String>, CursorOAuthError> {
    if !path.exists() {
        return Ok(HashMap::new());
    }

    let conn = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| CursorOAuthError::IoError(e.to_string()))?;

    let keys = [
        "cursorAuth/accessToken",
        "cursorAuth/refreshToken",
        "cursorAuth/cachedEmail",
    ];
    let quoted_keys = keys
        .iter()
        .map(|key| format!("'{}'", key.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(",");

    let mut out = HashMap::new();
    for table in ["ItemTable", "cursorDiskKV"] {
        let sql = format!("SELECT key, value FROM {table} WHERE key IN ({quoted_keys})");
        let Ok(mut stmt) = conn.prepare(&sql) else {
            continue;
        };
        let Ok(rows) = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        }) else {
            continue;
        };
        for row in rows.flatten() {
            out.insert(row.0, coerce_cursor_storage_value(&row.1));
        }
    }

    Ok(out)
}

fn coerce_cursor_storage_value(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(trimmed) {
        if let Some(value) = parsed.as_str() {
            return value.to_string();
        }
        if let Some(value) = parsed.get("value").and_then(|value| value.as_str()) {
            return value.to_string();
        }
    }
    value.to_string()
}

impl From<&CursorOAuthStartResponse> for GitHubDeviceCodeResponse {
    fn from(value: &CursorOAuthStartResponse) -> Self {
        GitHubDeviceCodeResponse {
            device_code: value.state.clone(),
            user_code: String::new(),
            verification_uri: value.auth_url.clone(),
            expires_in: BROWSER_FLOW_TIMEOUT_SECS as u64,
            interval: 2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cursor_account(refresh_token: &str) -> CursorAccountData {
        CursorAccountData {
            account_id: "cursor_test_account_1234".to_string(),
            email: None,
            refresh_token: refresh_token.to_string(),
            id_token: None,
            cursor_service_machine_id: None,
            cursor_client_version: None,
            cursor_config_version: None,
            cursor_client_id: None,
            authenticated_at: 1,
        }
    }

    #[test]
    fn coerce_cursor_storage_value_reads_wrapped_value() {
        assert_eq!(
            coerce_cursor_storage_value(r#"{"value":"user@example.com"}"#),
            "user@example.com"
        );
        assert_eq!(
            coerce_cursor_storage_value(r#""user@example.com""#),
            "user@example.com"
        );
        assert_eq!(coerce_cursor_storage_value("plain"), "plain");
    }

    #[test]
    fn github_account_exposes_only_valid_cursor_email() {
        let mut account = cursor_account("rt");
        account.email = Some("not-an-email".to_string());
        let public = GitHubAccount::from(&account);
        assert_eq!(public.email, None);
        assert_eq!(public.login, "Cursor(cursor_t)");

        account.email = Some("user@example.com".to_string());
        let public = GitHubAccount::from(&account);
        assert_eq!(public.email.as_deref(), Some("user@example.com"));
        assert_eq!(public.login, "Cursor(user@example.com)");
    }
}
