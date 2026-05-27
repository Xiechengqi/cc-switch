//! Antigravity OAuth Authentication Module
//!
//! Implements Antigravity Google OAuth browser flow with multi-account management.
//! Accounts are managed by cc-switch and can be bound to Antigravity OAuth
//! providers through `meta.authBinding`.

use base64::Engine as _;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;

use super::copilot_auth::GitHubAccount;

const ANTIGRAVITY_CLIENT_ID_ENV: &str = "CC_SWITCH_ANTIGRAVITY_CLIENT_ID";
const ANTIGRAVITY_CLIENT_SECRET_ENV: &str = "CC_SWITCH_ANTIGRAVITY_CLIENT_SECRET";
const DEFAULT_ANTIGRAVITY_CLIENT_ID: &str =
    "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com";
const DEFAULT_ANTIGRAVITY_CLIENT_SECRET: &str = "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";
const ANTIGRAVITY_AUTHORIZE_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const ANTIGRAVITY_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const ANTIGRAVITY_USERINFO_URL: &str = "https://www.googleapis.com/oauth2/v1/userinfo";
const ANTIGRAVITY_LOAD_CODE_ASSIST_URL: &str =
    "https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist";
const ANTIGRAVITY_ONBOARD_USER_URL: &str =
    "https://daily-cloudcode-pa.googleapis.com/v1internal:onboardUser";
const CALLBACK_PORT: u16 = 54547;
const CALLBACK_PATH: &str = "/callback";
const CALLBACK_TIMEOUT_SECS: u64 = 300;
const TOKEN_REFRESH_BUFFER_MS: i64 = 60_000;
const ANTIGRAVITY_USER_AGENT: &str = "cc-switch-antigravity-oauth";
const ANTIGRAVITY_NODE_API_CLIENT_UA: &str = "google-api-nodejs-client/10.3.0";
const ANTIGRAVITY_GOOG_API_CLIENT_UA: &str = "gl-node/22.21.1";
const ANTIGRAVITY_SCOPES: &[&str] = &[
    "https://www.googleapis.com/auth/cloud-platform",
    "https://www.googleapis.com/auth/userinfo.email",
    "https://www.googleapis.com/auth/userinfo.profile",
    "https://www.googleapis.com/auth/cclog",
    "https://www.googleapis.com/auth/experimentsandconfigs",
];

static GLOBAL_ANTIGRAVITY_OAUTH_MANAGER: OnceLock<Arc<RwLock<AntigravityOAuthManager>>> =
    OnceLock::new();

pub fn set_global_antigravity_oauth_manager(manager: Arc<RwLock<AntigravityOAuthManager>>) {
    let _ = GLOBAL_ANTIGRAVITY_OAUTH_MANAGER.set(manager);
}

fn env_or_default(name: &str, default: &str) -> String {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn antigravity_client_id() -> Result<String, AntigravityOAuthError> {
    Ok(env_or_default(
        ANTIGRAVITY_CLIENT_ID_ENV,
        DEFAULT_ANTIGRAVITY_CLIENT_ID,
    ))
}

fn antigravity_client_secret() -> Result<String, AntigravityOAuthError> {
    Ok(env_or_default(
        ANTIGRAVITY_CLIENT_SECRET_ENV,
        DEFAULT_ANTIGRAVITY_CLIENT_SECRET,
    ))
}

#[derive(Debug, thiserror::Error)]
pub enum AntigravityOAuthError {
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

impl From<reqwest::Error> for AntigravityOAuthError {
    fn from(err: reqwest::Error) -> Self {
        AntigravityOAuthError::NetworkError(err.to_string())
    }
}

impl From<std::io::Error> for AntigravityOAuthError {
    fn from(err: std::io::Error) -> Self {
        AntigravityOAuthError::IoError(err.to_string())
    }
}

#[derive(Debug, Clone, Deserialize)]
struct OAuthTokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
struct UserInfoResponse {
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    picture: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct LoadCodeAssistResponse {
    #[serde(default, rename = "cloudaicompanionProject")]
    cloudaicompanion_project: Option<ProjectRef>,
    #[serde(default, rename = "projectId")]
    project_id: Option<ProjectRef>,
    #[serde(default)]
    project: Option<ProjectRef>,
    #[serde(default, rename = "allowedTiers")]
    allowed_tiers: Vec<TierRef>,
}

impl LoadCodeAssistResponse {
    fn extracted_project_id(&self) -> Option<String> {
        self.cloudaicompanion_project
            .clone()
            .and_then(ProjectRef::into_id)
            .or_else(|| self.project_id.clone().and_then(ProjectRef::into_id))
            .or_else(|| self.project.clone().and_then(ProjectRef::into_id))
    }

    fn default_tier_id(&self) -> String {
        self.allowed_tiers
            .iter()
            .find(|tier| tier.is_default)
            .and_then(|tier| tier.id.as_deref())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("free-tier")
            .to_string()
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum ProjectRef {
    Id(String),
    Object {
        id: Option<String>,
        #[serde(default, rename = "projectId")]
        project_id: Option<String>,
    },
}

impl ProjectRef {
    fn into_id(self) -> Option<String> {
        match self {
            ProjectRef::Id(id) => Some(id.trim().to_string()),
            ProjectRef::Object { id, project_id } => {
                id.or(project_id).map(|value| value.trim().to_string())
            }
        }
        .filter(|value| !value.is_empty())
    }
}

#[derive(Debug, Clone, Deserialize)]
struct TierRef {
    #[serde(default)]
    id: Option<String>,
    #[serde(default, rename = "isDefault")]
    is_default: bool,
}

#[derive(Debug, Clone)]
struct AntigravityOnboarding {
    project_id: String,
    tier_id: String,
}

#[derive(Debug, Clone)]
struct AntigravityOnboardingCandidate {
    project_id: Option<String>,
    tier_id: String,
}

#[derive(Debug, Clone, Deserialize)]
struct OnboardUserResponse {
    #[serde(default)]
    done: bool,
    #[serde(default)]
    response: Option<OnboardUserPayload>,
}

#[derive(Debug, Clone, Deserialize)]
struct OnboardUserPayload {
    #[serde(default, rename = "cloudaicompanionProject")]
    cloudaicompanion_project: Option<ProjectRef>,
    #[serde(default, rename = "projectId")]
    project_id: Option<ProjectRef>,
    #[serde(default)]
    project: Option<ProjectRef>,
}

impl OnboardUserPayload {
    fn extracted_project_id(self) -> Option<String> {
        self.cloudaicompanion_project
            .and_then(ProjectRef::into_id)
            .or_else(|| self.project_id.and_then(ProjectRef::into_id))
            .or_else(|| self.project.and_then(ProjectRef::into_id))
    }
}

#[derive(Debug, Clone)]
struct CachedAccessToken {
    token: String,
    expires_at_ms: i64,
}

impl CachedAccessToken {
    fn is_expiring_soon(&self) -> bool {
        let now = chrono::Utc::now().timestamp_millis();
        self.expires_at_ms - now < TOKEN_REFRESH_BUFFER_MS
    }
}

#[derive(Debug, Clone)]
struct PendingOAuthFlow {
    expires_at_ms: i64,
}

#[derive(Debug)]
enum FlowResult {
    Pending,
    Ready(Result<GitHubAccount, String>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AntigravityAccountData {
    pub account_id: String,
    pub email: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
    pub refresh_token: String,
    pub project_id: String,
    pub tier_id: String,
    pub authenticated_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_token_expires_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_refreshed_at: Option<i64>,
}

impl From<&AntigravityAccountData> for GitHubAccount {
    fn from(data: &AntigravityAccountData) -> Self {
        GitHubAccount {
            id: data.account_id.clone(),
            login: data.email.clone(),
            email: Some(data.email.clone()),
            avatar_url: data.avatar_url.clone(),
            authenticated_at: data.authenticated_at,
            github_domain: "google.com".to_string(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct AntigravityOAuthStore {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    accounts: HashMap<String, AntigravityAccountData>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default_account_id: Option<String>,
}

#[derive(Clone)]
struct AntigravityOAuthEndpoints {
    token_url: String,
    userinfo_url: String,
    load_code_assist_url: String,
    onboard_user_url: String,
}

impl Default for AntigravityOAuthEndpoints {
    fn default() -> Self {
        Self {
            token_url: ANTIGRAVITY_TOKEN_URL.to_string(),
            userinfo_url: ANTIGRAVITY_USERINFO_URL.to_string(),
            load_code_assist_url: ANTIGRAVITY_LOAD_CODE_ASSIST_URL.to_string(),
            onboard_user_url: ANTIGRAVITY_ONBOARD_USER_URL.to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AntigravityOAuthCredentials {
    pub access_token: String,
    pub refresh_token: String,
    pub expiry_date: i64,
}

#[derive(Clone)]
pub struct AntigravityOAuthManager {
    accounts: Arc<RwLock<HashMap<String, AntigravityAccountData>>>,
    default_account_id: Arc<RwLock<Option<String>>>,
    access_tokens: Arc<RwLock<HashMap<String, CachedAccessToken>>>,
    refresh_locks: Arc<RwLock<HashMap<String, Arc<Mutex<()>>>>>,
    pending_flows: Arc<RwLock<HashMap<String, PendingOAuthFlow>>>,
    flow_results: Arc<RwLock<HashMap<String, FlowResult>>>,
    active_flow_handle: Arc<Mutex<Option<JoinHandle<()>>>>,
    http_client: Client,
    endpoints: AntigravityOAuthEndpoints,
    storage_path: PathBuf,
}

impl AntigravityOAuthManager {
    pub fn new(data_dir: PathBuf) -> Self {
        let storage_path = data_dir.join("antigravity_oauth_auth.json");

        let manager = Self {
            accounts: Arc::new(RwLock::new(HashMap::new())),
            default_account_id: Arc::new(RwLock::new(None)),
            access_tokens: Arc::new(RwLock::new(HashMap::new())),
            refresh_locks: Arc::new(RwLock::new(HashMap::new())),
            pending_flows: Arc::new(RwLock::new(HashMap::new())),
            flow_results: Arc::new(RwLock::new(HashMap::new())),
            active_flow_handle: Arc::new(Mutex::new(None)),
            http_client: Client::new(),
            endpoints: AntigravityOAuthEndpoints::default(),
            storage_path,
        };

        if let Err(e) = manager.load_from_disk_sync() {
            log::warn!("[AntigravityOAuth] 加载存储失败: {e}");
        }

        manager
    }

    #[cfg(test)]
    fn new_for_test(
        data_dir: PathBuf,
        http_client: Client,
        endpoints: AntigravityOAuthEndpoints,
    ) -> Self {
        Self {
            accounts: Arc::new(RwLock::new(HashMap::new())),
            default_account_id: Arc::new(RwLock::new(None)),
            access_tokens: Arc::new(RwLock::new(HashMap::new())),
            refresh_locks: Arc::new(RwLock::new(HashMap::new())),
            pending_flows: Arc::new(RwLock::new(HashMap::new())),
            flow_results: Arc::new(RwLock::new(HashMap::new())),
            active_flow_handle: Arc::new(Mutex::new(None)),
            http_client,
            endpoints,
            storage_path: data_dir.join("antigravity_oauth_auth.json"),
        }
    }

    fn generate_state() -> String {
        use rand::RngCore;
        let mut bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut bytes);
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    }

    pub async fn start_browser_flow(
        &self,
    ) -> Result<AntigravityOAuthStartResponse, AntigravityOAuthError> {
        use tokio::net::TcpListener;

        let state = Self::generate_state();
        let redirect_uri = format!("http://localhost:{CALLBACK_PORT}{CALLBACK_PATH}");
        let scope = ANTIGRAVITY_SCOPES.join(" ");
        let client_id = antigravity_client_id()?;
        let auth_url = format!(
            "{ANTIGRAVITY_AUTHORIZE_URL}?client_id={}&redirect_uri={}&response_type=code&scope={}&access_type=offline&prompt=consent&state={}",
            urlencoding::encode(&client_id),
            urlencoding::encode(&redirect_uri),
            urlencoding::encode(&scope),
            urlencoding::encode(&state),
        );

        {
            let mut handle_guard = self.active_flow_handle.lock().await;
            if let Some(prev) = handle_guard.take() {
                prev.abort();
                let _ = prev.await;
            }
        }
        {
            let mut results = self.flow_results.write().await;
            results.clear();
        }

        let addr = format!("127.0.0.1:{CALLBACK_PORT}");
        let listener = TcpListener::bind(&addr).await.map_err(|e| {
            AntigravityOAuthError::CallbackServerError(format!(
                "无法绑定回调端口 {CALLBACK_PORT}: {e}"
            ))
        })?;

        let expires_at_ms =
            chrono::Utc::now().timestamp_millis() + (CALLBACK_TIMEOUT_SECS as i64) * 1000;

        {
            let mut pending = self.pending_flows.write().await;
            let now_ms = chrono::Utc::now().timestamp_millis();
            pending.retain(|_, flow| flow.expires_at_ms > now_ms);
            pending.insert(state.clone(), PendingOAuthFlow { expires_at_ms });
        }
        {
            let mut results = self.flow_results.write().await;
            results.insert(state.clone(), FlowResult::Pending);
        }

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

        Ok(AntigravityOAuthStartResponse {
            auth_url,
            state,
            callback_port: CALLBACK_PORT,
        })
    }

    pub async fn poll_callback_result(
        &self,
        state: &str,
    ) -> Result<Option<GitHubAccount>, AntigravityOAuthError> {
        let mut results = self.flow_results.write().await;

        match results.get(state) {
            None => Err(AntigravityOAuthError::TokenFetchFailed(
                "未找到对应的 OAuth 流程（state 不匹配或已过期），请重新登录".to_string(),
            )),
            Some(FlowResult::Pending) => Ok(None),
            Some(FlowResult::Ready(_)) => {
                let entry = results.remove(state).unwrap();
                if let FlowResult::Ready(r) = entry {
                    match r {
                        Ok(account) => Ok(Some(account)),
                        Err(e) => Err(AntigravityOAuthError::TokenFetchFailed(e)),
                    }
                } else {
                    unreachable!()
                }
            }
        }
    }

    async fn run_callback_on_listener(
        &self,
        listener: tokio::net::TcpListener,
        state: &str,
    ) -> Result<GitHubAccount, AntigravityOAuthError> {
        let timeout = tokio::time::Duration::from_secs(CALLBACK_TIMEOUT_SECS);
        let result = tokio::time::timeout(timeout, Self::accept_callback(&listener)).await;

        match result {
            Ok(Ok((code, received_state))) => {
                if received_state != state {
                    return Err(AntigravityOAuthError::TokenFetchFailed(format!(
                        "state 不匹配: 期望 {state}, 收到 {received_state}"
                    )));
                }
                self.handle_callback(&code, &received_state).await
            }
            Ok(Err(e)) => Err(e),
            Err(_) => {
                let mut pending = self.pending_flows.write().await;
                pending.remove(state);
                Err(AntigravityOAuthError::Timeout)
            }
        }
    }

    async fn accept_callback(
        listener: &tokio::net::TcpListener,
    ) -> Result<(String, String), AntigravityOAuthError> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (mut stream, _) = listener
            .accept()
            .await
            .map_err(|e| AntigravityOAuthError::CallbackServerError(format!("accept 失败: {e}")))?;

        let mut buf = vec![0u8; 4096];
        let n = stream.read(&mut buf).await.map_err(|e| {
            AntigravityOAuthError::CallbackServerError(format!("读取请求失败: {e}"))
        })?;

        let request = String::from_utf8_lossy(&buf[..n]);
        let (code, state) = parse_callback_request(&request)?;

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

    async fn handle_callback(
        &self,
        code: &str,
        state: &str,
    ) -> Result<GitHubAccount, AntigravityOAuthError> {
        let flow = {
            let mut pending = self.pending_flows.write().await;
            pending.remove(state).ok_or_else(|| {
                AntigravityOAuthError::TokenFetchFailed(
                    "未找到对应的 OAuth 流程（state 不匹配或已过期），请重新登录".to_string(),
                )
            })?
        };

        if flow.expires_at_ms <= chrono::Utc::now().timestamp_millis() {
            return Err(AntigravityOAuthError::Timeout);
        }

        let tokens = self.exchange_code_for_tokens(code).await?;
        let refresh_token = tokens.refresh_token.clone().ok_or_else(|| {
            AntigravityOAuthError::TokenFetchFailed("响应缺少 refresh_token".to_string())
        })?;
        let userinfo = self.fetch_user_info(&tokens.access_token).await?;
        let email = userinfo.email.ok_or_else(|| {
            AntigravityOAuthError::ParseError("无法从 Google userinfo 提取 email".to_string())
        })?;
        let onboarding = self.complete_onboarding(&tokens.access_token).await?;

        let access_token_expires_at_ms = compute_expires_at_ms(tokens.expires_in);
        {
            let mut tokens_cache = self.access_tokens.write().await;
            tokens_cache.insert(
                email.clone(),
                CachedAccessToken {
                    token: tokens.access_token.clone(),
                    expires_at_ms: access_token_expires_at_ms,
                },
            );
        }

        let account = self
            .add_account_internal(
                email.clone(),
                refresh_token,
                onboarding.project_id,
                onboarding.tier_id,
                userinfo.name,
                userinfo.picture,
                Some(access_token_expires_at_ms),
                Some(chrono::Utc::now().timestamp_millis()),
            )
            .await?;

        Ok(account)
    }

    async fn exchange_code_for_tokens(
        &self,
        code: &str,
    ) -> Result<OAuthTokenResponse, AntigravityOAuthError> {
        let redirect_uri = format!("http://localhost:{CALLBACK_PORT}{CALLBACK_PATH}");
        let client_id = antigravity_client_id()?;
        let client_secret = antigravity_client_secret()?;

        let response = self
            .http_client
            .post(&self.endpoints.token_url)
            .header("User-Agent", ANTIGRAVITY_USER_AGENT)
            .form(&[
                ("client_id", client_id.as_str()),
                ("client_secret", client_secret.as_str()),
                ("code", code),
                ("grant_type", "authorization_code"),
                ("redirect_uri", redirect_uri.as_str()),
            ])
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(AntigravityOAuthError::TokenFetchFailed(format!(
                "Token 交换失败: {status} - {text}"
            )));
        }

        response
            .json()
            .await
            .map_err(|e| AntigravityOAuthError::ParseError(e.to_string()))
    }

    async fn fetch_user_info(
        &self,
        access_token: &str,
    ) -> Result<UserInfoResponse, AntigravityOAuthError> {
        let response = self
            .http_client
            .get(&self.endpoints.userinfo_url)
            .header("Authorization", format!("Bearer {access_token}"))
            .header("User-Agent", ANTIGRAVITY_USER_AGENT)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(AntigravityOAuthError::TokenFetchFailed(format!(
                "userinfo 查询失败: {status} - {text}"
            )));
        }

        response
            .json()
            .await
            .map_err(|e| AntigravityOAuthError::ParseError(e.to_string()))
    }

    fn client_metadata() -> serde_json::Value {
        serde_json::json!({
            "ideType": 9,
            "platform": antigravity_platform_enum(),
            "pluginType": 2,
        })
    }

    fn control_plane_metadata(user_agent: &str) -> serde_json::Value {
        serde_json::json!({
            "ide_type": "ANTIGRAVITY",
            "ide_version": antigravity_version_from_user_agent(user_agent),
            "ide_name": "antigravity",
        })
    }

    fn antigravity_load_code_assist_headers(
        &self,
        access_token: &str,
    ) -> Result<reqwest::header::HeaderMap, AntigravityOAuthError> {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::AUTHORIZATION,
            reqwest::header::HeaderValue::from_str(&format!("Bearer {access_token}"))
                .map_err(|e| AntigravityOAuthError::ParseError(e.to_string()))?,
        );
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("application/json"),
        );
        headers.insert(
            reqwest::header::USER_AGENT,
            reqwest::header::HeaderValue::from_str(&antigravity_request_user_agent())
                .map_err(|e| AntigravityOAuthError::ParseError(e.to_string()))?,
        );
        let metadata_json = serde_json::to_string(&Self::client_metadata())
            .map_err(|e| AntigravityOAuthError::ParseError(e.to_string()))?;
        headers.insert(
            reqwest::header::HeaderName::from_static("client-metadata"),
            reqwest::header::HeaderValue::from_str(&metadata_json)
                .map_err(|e| AntigravityOAuthError::ParseError(e.to_string()))?,
        );
        Ok(headers)
    }

    fn antigravity_onboard_user_headers(
        &self,
        access_token: &str,
    ) -> Result<reqwest::header::HeaderMap, AntigravityOAuthError> {
        let mut headers = self.antigravity_load_code_assist_headers(access_token)?;
        headers.insert(
            reqwest::header::USER_AGENT,
            reqwest::header::HeaderValue::from_str(&antigravity_load_code_assist_user_agent())
                .map_err(|e| AntigravityOAuthError::ParseError(e.to_string()))?,
        );
        headers.insert(
            reqwest::header::HeaderName::from_static("x-goog-api-client"),
            reqwest::header::HeaderValue::from_static(ANTIGRAVITY_GOOG_API_CLIENT_UA),
        );
        Ok(headers)
    }

    async fn load_code_assist(
        &self,
        access_token: &str,
    ) -> Result<AntigravityOnboardingCandidate, AntigravityOAuthError> {
        let response = self
            .http_client
            .post(&self.endpoints.load_code_assist_url)
            .headers(self.antigravity_load_code_assist_headers(access_token)?)
            .json(&serde_json::json!({ "metadata": Self::client_metadata() }))
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(AntigravityOAuthError::TokenFetchFailed(format!(
                "loadCodeAssist 失败: {status} - {text}"
            )));
        }

        let payload: LoadCodeAssistResponse = response
            .json()
            .await
            .map_err(|e| AntigravityOAuthError::ParseError(e.to_string()))?;

        Ok(AntigravityOnboardingCandidate {
            project_id: payload.extracted_project_id(),
            tier_id: payload.default_tier_id(),
        })
    }

    async fn onboard_user_once(
        &self,
        access_token: &str,
        tier_id: &str,
    ) -> Result<OnboardUserResponse, AntigravityOAuthError> {
        let user_agent = antigravity_load_code_assist_user_agent();
        let response = self
            .http_client
            .post(&self.endpoints.onboard_user_url)
            .headers(self.antigravity_onboard_user_headers(access_token)?)
            .json(&serde_json::json!({
                "tier_id": tier_id,
                "metadata": Self::control_plane_metadata(&user_agent),
            }))
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(AntigravityOAuthError::TokenFetchFailed(format!(
                "onboardUser 失败: {status} - {text}"
            )));
        }

        response
            .json()
            .await
            .map_err(|e| AntigravityOAuthError::ParseError(e.to_string()))
    }

    async fn complete_onboarding(
        &self,
        access_token: &str,
    ) -> Result<AntigravityOnboarding, AntigravityOAuthError> {
        let candidate = self.load_code_assist(access_token).await?;
        if let Some(project_id) = candidate.project_id {
            return Ok(AntigravityOnboarding {
                project_id,
                tier_id: candidate.tier_id,
            });
        }

        for _ in 0..10 {
            let result = self
                .onboard_user_once(access_token, &candidate.tier_id)
                .await?;
            if result.done {
                if let Some(project_id) = result
                    .response
                    .and_then(OnboardUserPayload::extracted_project_id)
                {
                    return Ok(AntigravityOnboarding {
                        project_id,
                        tier_id: candidate.tier_id,
                    });
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }

        Err(AntigravityOAuthError::Timeout)
    }

    async fn refresh_with_token(
        &self,
        refresh_token: &str,
    ) -> Result<OAuthTokenResponse, AntigravityOAuthError> {
        let client_id = antigravity_client_id()?;
        let client_secret = antigravity_client_secret()?;

        let response = self
            .http_client
            .post(&self.endpoints.token_url)
            .header("User-Agent", ANTIGRAVITY_USER_AGENT)
            .form(&[
                ("client_id", client_id.as_str()),
                ("client_secret", client_secret.as_str()),
                ("refresh_token", refresh_token),
                ("grant_type", "refresh_token"),
            ])
            .send()
            .await?;

        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(AntigravityOAuthError::RefreshTokenInvalid);
        }

        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(AntigravityOAuthError::TokenFetchFailed(format!(
                "Refresh 失败: {status} - {text}"
            )));
        }

        response
            .json()
            .await
            .map_err(|e| AntigravityOAuthError::ParseError(e.to_string()))
    }

    pub async fn get_valid_token_for_account(
        &self,
        account_id: &str,
    ) -> Result<String, AntigravityOAuthError> {
        {
            let tokens = self.access_tokens.read().await;
            if let Some(cached) = tokens.get(account_id) {
                if !cached.is_expiring_soon() {
                    return Ok(cached.token.clone());
                }
            }
        }

        let refresh_lock = self.get_refresh_lock(account_id).await;
        let _guard = refresh_lock.lock().await;

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
                .ok_or_else(|| AntigravityOAuthError::AccountNotFound(account_id.to_string()))?
        };

        let new_tokens = self.refresh_with_token(&refresh_token).await?;
        let access_token = new_tokens.access_token.clone();
        let expires_at_ms = compute_expires_at_ms(new_tokens.expires_in);
        let refreshed_at_ms = chrono::Utc::now().timestamp_millis();
        {
            let mut accounts = self.accounts.write().await;
            if let Some(account) = accounts.get_mut(account_id) {
                if let Some(new_refresh) = new_tokens.refresh_token.clone() {
                    if new_refresh != refresh_token {
                        account.refresh_token = new_refresh;
                    }
                }
                account.access_token_expires_at_ms = Some(expires_at_ms);
                account.last_refreshed_at = Some(refreshed_at_ms);
            }
        }
        self.save_to_disk().await?;

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

    pub async fn default_account_id(&self) -> Option<String> {
        self.resolve_default_account_id().await
    }

    pub async fn project_id_for_account(
        &self,
        account_id: &str,
    ) -> Result<String, AntigravityOAuthError> {
        let accounts = self.accounts.read().await;
        accounts
            .get(account_id)
            .map(|account| account.project_id.clone())
            .filter(|project_id| !project_id.trim().is_empty())
            .ok_or_else(|| AntigravityOAuthError::AccountNotFound(account_id.to_string()))
    }

    pub async fn export_cli_credentials_for_account(
        &self,
        account_id: &str,
    ) -> Result<AntigravityOAuthCredentials, AntigravityOAuthError> {
        let access_token = self.get_valid_token_for_account(account_id).await?;
        let refresh_token = {
            let accounts = self.accounts.read().await;
            accounts
                .get(account_id)
                .map(|a| a.refresh_token.clone())
                .ok_or_else(|| AntigravityOAuthError::AccountNotFound(account_id.to_string()))?
        };
        let expiry_date = {
            let tokens = self.access_tokens.read().await;
            tokens
                .get(account_id)
                .map(|token| token.expires_at_ms)
                .unwrap_or_else(|| chrono::Utc::now().timestamp_millis() + 3_600_000)
        };

        Ok(AntigravityOAuthCredentials {
            access_token,
            refresh_token,
            expiry_date,
        })
    }

    pub async fn list_accounts(&self) -> Vec<GitHubAccount> {
        let accounts = self.accounts.read().await.clone();
        let default_id = self.resolve_default_account_id().await;
        Self::sorted_accounts(&accounts, default_id.as_deref())
    }

    pub async fn invalidate_cached_token(&self, account_id: &str) {
        let mut tokens = self.access_tokens.write().await;
        let _ = tokens.remove(account_id);
    }

    pub async fn remove_account(&self, account_id: &str) -> Result<(), AntigravityOAuthError> {
        {
            let mut accounts = self.accounts.write().await;
            if accounts.remove(account_id).is_none() {
                return Err(AntigravityOAuthError::AccountNotFound(
                    account_id.to_string(),
                ));
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

    pub async fn set_default_account(&self, account_id: &str) -> Result<(), AntigravityOAuthError> {
        {
            let accounts = self.accounts.read().await;
            if !accounts.contains_key(account_id) {
                return Err(AntigravityOAuthError::AccountNotFound(
                    account_id.to_string(),
                ));
            }
        }
        {
            let mut default = self.default_account_id.write().await;
            *default = Some(account_id.to_string());
        }
        self.save_to_disk().await?;
        Ok(())
    }

    pub async fn clear_auth(&self) -> Result<(), AntigravityOAuthError> {
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

    pub async fn get_status(&self) -> AntigravityOAuthStatus {
        let accounts_map = self.accounts.read().await.clone();
        let default_id = self.resolve_default_account_id().await;
        let account_list = Self::sorted_accounts(&accounts_map, default_id.as_deref());
        AntigravityOAuthStatus {
            accounts: account_list.clone(),
            default_account_id: default_id,
            authenticated: !account_list.is_empty(),
            username: account_list.first().map(|a| a.login.clone()),
        }
    }

    async fn add_account_internal(
        &self,
        email: String,
        refresh_token: String,
        project_id: String,
        tier_id: String,
        display_name: Option<String>,
        avatar_url: Option<String>,
        access_token_expires_at_ms: Option<i64>,
        last_refreshed_at: Option<i64>,
    ) -> Result<GitHubAccount, AntigravityOAuthError> {
        let now = chrono::Utc::now().timestamp();
        let data = AntigravityAccountData {
            account_id: email.clone(),
            email,
            display_name,
            avatar_url,
            refresh_token,
            project_id,
            tier_id,
            authenticated_at: now,
            access_token_expires_at_ms,
            last_refreshed_at,
        };
        let account = GitHubAccount::from(&data);

        {
            let mut accounts = self.accounts.write().await;
            accounts.insert(data.account_id.clone(), data);
        }
        {
            let mut default = self.default_account_id.write().await;
            if default.is_none() {
                *default = Some(account.id.clone());
            }
        }
        self.save_to_disk().await?;
        Ok(account)
    }

    fn fallback_default_account_id(
        accounts: &HashMap<String, AntigravityAccountData>,
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
        accounts: &HashMap<String, AntigravityAccountData>,
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

    fn write_store_atomic(&self, content: &str) -> Result<(), AntigravityOAuthError> {
        if let Some(parent) = self.storage_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let parent = self
            .storage_path
            .parent()
            .ok_or_else(|| AntigravityOAuthError::IoError("无效的存储路径".to_string()))?;
        let file_name = self
            .storage_path
            .file_name()
            .ok_or_else(|| AntigravityOAuthError::IoError("无效的存储文件名".to_string()))?
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

    fn load_from_disk_sync(&self) -> Result<(), AntigravityOAuthError> {
        if !self.storage_path.exists() {
            return Ok(());
        }

        let content = std::fs::read_to_string(&self.storage_path)?;
        let store: AntigravityOAuthStore = serde_json::from_str(&content)
            .map_err(|e| AntigravityOAuthError::ParseError(e.to_string()))?;

        if let Ok(mut accounts) = self.accounts.try_write() {
            *accounts = store.accounts;
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

    async fn save_to_disk(&self) -> Result<(), AntigravityOAuthError> {
        let accounts = self.accounts.read().await.clone();
        let default = self.resolve_default_account_id().await;

        let store = AntigravityOAuthStore {
            version: 1,
            accounts,
            default_account_id: default,
        };

        let content = serde_json::to_string_pretty(&store)
            .map_err(|e| AntigravityOAuthError::ParseError(e.to_string()))?;

        self.write_store_atomic(&content)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AntigravityOAuthStatus {
    pub accounts: Vec<GitHubAccount>,
    pub default_account_id: Option<String>,
    pub authenticated: bool,
    pub username: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AntigravityOAuthStartResponse {
    pub auth_url: String,
    pub state: String,
    pub callback_port: u16,
}

fn compute_expires_at_ms(expires_in: Option<i64>) -> i64 {
    let expires_in = expires_in.unwrap_or(3600);
    chrono::Utc::now().timestamp_millis() + expires_in * 1000
}

const ANTIGRAVITY_IDE_VERSION: &str = "1.23.2";

fn antigravity_request_user_agent() -> String {
    format!(
        "antigravity/{ANTIGRAVITY_IDE_VERSION} {}/{}",
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

fn antigravity_load_code_assist_user_agent() -> String {
    format!(
        "{} {}",
        antigravity_request_user_agent(),
        ANTIGRAVITY_NODE_API_CLIENT_UA
    )
}

fn antigravity_version_from_user_agent(user_agent: &str) -> String {
    let user_agent = user_agent.trim();
    let Some(rest) = user_agent.strip_prefix("antigravity/") else {
        return ANTIGRAVITY_IDE_VERSION.to_string();
    };
    rest.split_whitespace()
        .next()
        .filter(|value| !value.is_empty())
        .unwrap_or(ANTIGRAVITY_IDE_VERSION)
        .to_string()
}

fn antigravity_platform_enum() -> i32 {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => 2,
        ("macos", _) => 1,
        ("linux", "aarch64") => 4,
        ("linux", _) => 3,
        ("windows", _) => 5,
        _ => 0,
    }
}

fn parse_callback_request(request: &str) -> Result<(String, String), AntigravityOAuthError> {
    let first_line = request
        .lines()
        .next()
        .ok_or_else(|| AntigravityOAuthError::CallbackServerError("空请求".to_string()))?;

    let path = first_line.split_whitespace().nth(1).ok_or_else(|| {
        AntigravityOAuthError::CallbackServerError("无法解析请求路径".to_string())
    })?;

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
            return Err(AntigravityOAuthError::TokenFetchFailed(format!(
                "OAuth 错误: {error} - {desc}"
            )));
        }

        let code = params
            .get("code")
            .ok_or_else(|| {
                AntigravityOAuthError::CallbackServerError("回调缺少 code 参数".to_string())
            })?
            .to_string();
        let state = params
            .get("state")
            .ok_or_else(|| {
                AntigravityOAuthError::CallbackServerError("回调缺少 state 参数".to_string())
            })?
            .to_string();

        let code = urlencoding::decode(&code)
            .unwrap_or_else(|_| code.clone().into())
            .to_string();
        let state = urlencoding::decode(&state)
            .unwrap_or_else(|_| state.clone().into())
            .to_string();

        Ok((code, state))
    } else {
        Err(AntigravityOAuthError::CallbackServerError(
            "回调请求缺少查询参数".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[derive(Debug, Clone)]
    struct RecordedRequest {
        path: String,
        body: String,
        raw: String,
    }

    async fn test_server(
        responses: Vec<&'static str>,
    ) -> (String, Arc<Mutex<Vec<RecordedRequest>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("local addr");
        let base_url = format!("http://{addr}");
        let records = Arc::new(Mutex::new(Vec::new()));
        let records_for_task = Arc::clone(&records);
        let mut responses: VecDeque<String> = responses.into_iter().map(str::to_string).collect();

        tokio::spawn(async move {
            while let Some(body) = responses.pop_front() {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let mut buf = vec![0u8; 16 * 1024];
                let Ok(n) = stream.read(&mut buf).await else {
                    break;
                };
                let raw = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = raw
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("")
                    .to_string();
                let body_start = raw.find("\r\n\r\n").map(|idx| idx + 4).unwrap_or(raw.len());
                records_for_task.lock().await.push(RecordedRequest {
                    path,
                    body: raw[body_start..].to_string(),
                    raw,
                });

                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.flush().await;
            }
        });

        (base_url, records)
    }

    fn test_manager(base_url: &str) -> AntigravityOAuthManager {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        AntigravityOAuthManager::new_for_test(
            temp_dir.path().to_path_buf(),
            Client::new(),
            AntigravityOAuthEndpoints {
                token_url: format!("{base_url}/token"),
                userinfo_url: format!("{base_url}/userinfo"),
                load_code_assist_url: format!("{base_url}/load"),
                onboard_user_url: format!("{base_url}/onboard"),
            },
        )
    }

    #[tokio::test]
    async fn complete_onboarding_uses_existing_project_without_onboard() {
        let (base_url, records) = test_server(vec![
            r#"{
            "cloudaicompanionProject": "project-from-load",
            "allowedTiers": [{"id": "paid-tier", "isDefault": true}]
        }"#,
        ])
        .await;
        let manager = test_manager(&base_url);

        let onboarding = manager.complete_onboarding("access-token").await.unwrap();

        assert_eq!(onboarding.project_id, "project-from-load");
        assert_eq!(onboarding.tier_id, "paid-tier");
        let records = records.lock().await;
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].path, "/load");
        assert!(records[0].body.contains(r#""ideType":9"#));
        assert!(records[0].raw.to_lowercase().contains("client-metadata"));
        assert!(!records[0].raw.to_lowercase().contains("x-goog-api-client"));
    }

    #[tokio::test]
    async fn complete_onboarding_falls_back_to_daily_onboard_when_project_missing() {
        assert_eq!(
            ANTIGRAVITY_ONBOARD_USER_URL,
            "https://daily-cloudcode-pa.googleapis.com/v1internal:onboardUser"
        );

        let (base_url, records) = test_server(vec![
            r#"{"allowedTiers":[{"id":"free-tier","isDefault":true}]}"#,
            r#"{
                "done": true,
                "response": {
                    "cloudaicompanionProject": {
                        "id": "project-from-onboard"
                    }
                }
            }"#,
        ])
        .await;
        let manager = test_manager(&base_url);

        let onboarding = manager.complete_onboarding("access-token").await.unwrap();

        assert_eq!(onboarding.project_id, "project-from-onboard");
        assert_eq!(onboarding.tier_id, "free-tier");
        let records = records.lock().await;
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].path, "/load");
        assert_eq!(records[1].path, "/onboard");
        assert!(records[1].body.contains(r#""tier_id":"free-tier""#));
        assert!(!records[1].body.contains("tierId"));
        assert!(records[1].body.contains(r#""ide_type":"ANTIGRAVITY""#));
        assert!(records[1]
            .raw
            .contains("x-goog-api-client: gl-node/22.21.1"));
    }

    #[test]
    fn load_code_assist_project_extraction_accepts_known_variants() {
        let payload: LoadCodeAssistResponse = serde_json::from_str(
            r#"{
                "projectId": {"projectId": "project-from-project-id"},
                "allowedTiers": []
            }"#,
        )
        .unwrap();
        assert_eq!(
            payload.extracted_project_id(),
            Some("project-from-project-id".to_string())
        );
        assert_eq!(payload.default_tier_id(), "free-tier");

        let payload: LoadCodeAssistResponse =
            serde_json::from_str(r#"{"project":{"id":"project-from-project"}}"#).unwrap();
        assert_eq!(
            payload.extracted_project_id(),
            Some("project-from-project".to_string())
        );

        let payload: OnboardUserPayload =
            serde_json::from_str(r#"{"projectId":{"projectId":"project-from-onboard"}}"#).unwrap();
        assert_eq!(
            payload.extracted_project_id(),
            Some("project-from-onboard".to_string())
        );
    }
}
