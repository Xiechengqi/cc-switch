//! Kiro OAuth Authentication Module
//!
//! Implements Kiro AWS Builder ID device-code authentication with multi-account
//! management. Providers bind to accounts through `meta.authBinding` using
//! `auth_provider = "kiro_oauth"`.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

use super::copilot_auth::{GitHubAccount, GitHubDeviceCodeResponse};

const TOKEN_REFRESH_BUFFER_MS: i64 = 60_000;
const DEFAULT_REGION: &str = "us-east-1";
const DEFAULT_START_URL: &str = "https://view.awsapps.com/start";
const KIRO_CLIENT_NAME: &str = "kiro-oauth-client";
const KIRO_CLIENT_TYPE: &str = "public";
const KIRO_ISSUER_URL: &str = "https://identitycenter.amazonaws.com/ssoins-722374e8c3c8e6c6";
const KIRO_AUTH_METHOD_BUILDER_ID: &str = "builder-id";
// Default Kiro profile ARNs used by the official Kiro IDE. AWS Builder ID
// device-flow logins usually return no profileArn of their own, but
// getUsageLimits (which also carries the account email) requires one, so we
// fall back to these shared defaults to keep email + usage working.
const BUILDER_ID_PROFILE_ARN: &str =
    "arn:aws:codewhisperer:us-east-1:638616132270:profile/AAAACCCCXXXX";
const SOCIAL_PROFILE_ARN: &str =
    "arn:aws:codewhisperer:us-east-1:699475941385:profile/EHGA3GRVQMUK";
const ENTERPRISE_FALLBACK_PROFILE_ACCOUNT_ID: &str = "610548660232";
const ENTERPRISE_FALLBACK_PROFILE_ID: &str = "VNECVYCYYAWN";

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
    #[error("旧版 Kiro Portal OAuth 账号不可用，请重新添加 AWS Builder ID 账号")]
    LegacyAccountUnsupported,
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
#[serde(rename_all = "camelCase")]
struct RegisterClientResponse {
    client_id: String,
    client_secret: String,
    #[serde(default)]
    client_secret_expires_at: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeviceAuthorizationResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    expires_in: u64,
    #[serde(default)]
    interval: Option<u64>,
}

#[derive(Debug, Clone)]
struct PendingDeviceFlow {
    client_id: String,
    client_secret: String,
    client_secret_expires_at: Option<i64>,
    region: String,
    start_url: String,
    expires_at_ms: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BuilderIdTokenResponse {
    #[serde(default, alias = "access_token")]
    access_token: Option<String>,
    #[serde(default, alias = "refresh_token")]
    refresh_token: Option<String>,
    #[serde(default, alias = "expires_in")]
    expires_in: Option<i64>,
    #[serde(default, alias = "profile_arn")]
    profile_arn: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default, alias = "error_description")]
    error_description: Option<String>,
    #[serde(flatten)]
    extra: Value,
}

impl BuilderIdTokenResponse {
    fn first_email(&self) -> Option<String> {
        first_email([
            self.extra
                .get("email")
                .and_then(Value::as_str)
                .map(str::to_string),
            self.extra
                .get("accountEmail")
                .and_then(Value::as_str)
                .map(str::to_string),
            self.extra
                .get("userEmail")
                .and_then(Value::as_str)
                .map(str::to_string),
            self.extra
                .get("idToken")
                .and_then(Value::as_str)
                .and_then(email_from_jwt),
            self.extra
                .get("id_token")
                .and_then(Value::as_str)
                .and_then(email_from_jwt),
            self.access_token.as_deref().and_then(email_from_jwt),
            self.refresh_token.as_deref().and_then(email_from_jwt),
        ])
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KiroUsageLimitsResponse {
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub account_email: Option<String>,
    #[serde(default)]
    pub user_email: Option<String>,
    #[serde(default)]
    pub next_date_reset: Option<f64>,
    #[serde(default)]
    pub subscription_info: Option<KiroSubscriptionInfo>,
    #[serde(default)]
    pub usage_breakdown_list: Vec<KiroUsageBreakdown>,
    #[serde(default)]
    pub overage_configuration: Option<KiroOverageConfiguration>,
    #[serde(default, flatten)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KiroSubscriptionInfo {
    #[serde(default)]
    pub subscription_title: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub account_email: Option<String>,
    #[serde(default)]
    pub user_email: Option<String>,
    #[serde(default)]
    pub overage_capability: Option<String>,
    #[serde(default, flatten)]
    pub extra: HashMap<String, Value>,
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
    pub fn account_email(&self) -> Option<&str> {
        [
            self.email.as_deref(),
            self.account_email.as_deref(),
            self.user_email.as_deref(),
            self.subscription_info
                .as_ref()
                .and_then(|info| info.email.as_deref()),
            self.subscription_info
                .as_ref()
                .and_then(|info| info.account_email.as_deref()),
            self.subscription_info
                .as_ref()
                .and_then(|info| info.user_email.as_deref()),
        ]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find_map(valid_email)
        .or_else(|| self.extra.values().find_map(find_email_in_value))
        .or_else(|| {
            self.subscription_info
                .as_ref()
                .and_then(|info| info.extra.values().find_map(find_email_in_value))
        })
    }

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret_expires_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    pub authenticated_at: i64,
}

impl KiroAccountData {
    fn is_builder_id(&self) -> bool {
        self.client_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .is_some()
            && self
                .client_secret
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .is_some()
    }
}

pub fn is_enterprise_account(account: &KiroAccountData) -> bool {
    account
        .auth_method
        .as_deref()
        .into_iter()
        .chain(account.provider.as_deref())
        .any(|value| {
            let value = value.to_ascii_lowercase();
            value == "enterprise" || value == "external_idp" || value == "externalidp"
        })
}

fn is_social_account(account: &KiroAccountData) -> bool {
    account
        .auth_method
        .as_deref()
        .into_iter()
        .chain(account.provider.as_deref())
        .any(|method| {
            let method = method.to_ascii_lowercase();
            method == "social" || method == "google" || method == "github"
        })
}

pub fn enterprise_fallback_profile_arn(region: &str) -> String {
    let region = if region.starts_with("eu-") {
        "eu-central-1"
    } else {
        "us-east-1"
    };
    format!(
        "arn:aws:codewhisperer:{region}:{ENTERPRISE_FALLBACK_PROFILE_ACCOUNT_ID}:profile/{ENTERPRISE_FALLBACK_PROFILE_ID}"
    )
}

pub fn default_profile_arn(account: &KiroAccountData) -> String {
    if is_enterprise_account(account) {
        let region = if account.api_region.trim().is_empty() {
            DEFAULT_REGION
        } else {
            account.api_region.as_str()
        };
        enterprise_fallback_profile_arn(region)
    } else if is_social_account(account) {
        SOCIAL_PROFILE_ARN.to_string()
    } else {
        BUILDER_ID_PROFILE_ARN.to_string()
    }
}

pub fn resolve_profile_arn(account: &KiroAccountData) -> String {
    account
        .profile_arn
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| default_profile_arn(account))
}

impl From<&KiroAccountData> for GitHubAccount {
    fn from(data: &KiroAccountData) -> Self {
        let display_email = data.email.as_deref().and_then(valid_email);
        let login = if data.is_builder_id() {
            display_email
                .map(|email| format!("Kiro({email})"))
                .unwrap_or_else(|| format!("Kiro Builder ID ({})", short_id(&data.account_id)))
        } else {
            display_email
                .map(|email| format!("Kiro Legacy({email})"))
                .unwrap_or_else(|| format!("Kiro Legacy ({})", short_id(&data.account_id)))
        };

        GitHubAccount {
            id: data.account_id.clone(),
            email: display_email.map(str::to_string),
            login,
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
    temporarily_unavailable_until: Arc<RwLock<HashMap<String, i64>>>,
    refresh_locks: Arc<RwLock<HashMap<String, Arc<Mutex<()>>>>>,
    pending_device_flows: Arc<RwLock<HashMap<String, PendingDeviceFlow>>>,
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
            temporarily_unavailable_until: Arc::new(RwLock::new(HashMap::new())),
            refresh_locks: Arc::new(RwLock::new(HashMap::new())),
            pending_device_flows: Arc::new(RwLock::new(HashMap::new())),
            http_client: Client::new(),
            storage_path,
        };
        if let Err(e) = manager.load_from_disk_sync() {
            log::warn!("[KiroOAuth] 加载存储失败: {e}");
        }
        manager
    }

    pub async fn start_device_flow(&self) -> Result<GitHubDeviceCodeResponse, KiroOAuthError> {
        let region = DEFAULT_REGION.to_string();
        let start_url = DEFAULT_START_URL.to_string();
        let client = self.register_client(&region).await?;
        let device = self
            .request_device_authorization(&region, &client, &start_url)
            .await?;
        let expires_at_ms =
            chrono::Utc::now().timestamp_millis() + (device.expires_in as i64 * 1000);

        {
            let mut pending = self.pending_device_flows.write().await;
            let now_ms = chrono::Utc::now().timestamp_millis();
            pending.retain(|_, flow| flow.expires_at_ms > now_ms);
            pending.insert(
                device.device_code.clone(),
                PendingDeviceFlow {
                    client_id: client.client_id,
                    client_secret: client.client_secret,
                    client_secret_expires_at: client.client_secret_expires_at,
                    region,
                    start_url,
                    expires_at_ms,
                },
            );
        }

        Ok(GitHubDeviceCodeResponse {
            device_code: device.device_code,
            user_code: device.user_code,
            verification_uri: device
                .verification_uri_complete
                .unwrap_or(device.verification_uri),
            expires_in: device.expires_in,
            interval: device.interval.unwrap_or(5),
        })
    }

    async fn register_client(
        &self,
        region: &str,
    ) -> Result<RegisterClientResponse, KiroOAuthError> {
        let url = format!("https://oidc.{region}.amazonaws.com/client/register");
        let response = self
            .http_client
            .post(url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .json(&serde_json::json!({
                "clientName": KIRO_CLIENT_NAME,
                "clientType": KIRO_CLIENT_TYPE,
                "scopes": [
                    "codewhisperer:completions",
                    "codewhisperer:analysis",
                    "codewhisperer:conversations"
                ],
                "grantTypes": [
                    "urn:ietf:params:oauth:grant-type:device_code",
                    "refresh_token"
                ],
                "issuerUrl": KIRO_ISSUER_URL
            }))
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(KiroOAuthError::TokenFetchFailed(format!(
                "client registration failed: {status} {body}"
            )));
        }

        response
            .json::<RegisterClientResponse>()
            .await
            .map_err(|e| KiroOAuthError::ParseError(e.to_string()))
    }

    async fn request_device_authorization(
        &self,
        region: &str,
        client: &RegisterClientResponse,
        start_url: &str,
    ) -> Result<DeviceAuthorizationResponse, KiroOAuthError> {
        let url = format!("https://oidc.{region}.amazonaws.com/device_authorization");
        let response = self
            .http_client
            .post(url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .json(&serde_json::json!({
                "clientId": client.client_id,
                "clientSecret": client.client_secret,
                "startUrl": start_url
            }))
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(KiroOAuthError::TokenFetchFailed(format!(
                "device authorization failed: {status} {body}"
            )));
        }

        response
            .json::<DeviceAuthorizationResponse>()
            .await
            .map_err(|e| KiroOAuthError::ParseError(e.to_string()))
    }

    pub async fn poll_for_token(
        &self,
        device_code: &str,
    ) -> Result<Option<GitHubAccount>, KiroOAuthError> {
        let flow = {
            let pending = self.pending_device_flows.read().await;
            pending
                .get(device_code)
                .cloned()
                .ok_or_else(|| KiroOAuthError::TokenFetchFailed("设备码流程已过期".to_string()))?
        };

        if flow.expires_at_ms <= chrono::Utc::now().timestamp_millis() {
            self.pending_device_flows.write().await.remove(device_code);
            return Err(KiroOAuthError::Timeout);
        }

        let token = self
            .poll_builder_id_token(
                &flow.region,
                &flow.client_id,
                &flow.client_secret,
                device_code,
            )
            .await?;
        let Some(access_token) = token.access_token.clone() else {
            return Ok(None);
        };

        self.pending_device_flows.write().await.remove(device_code);
        let account = self.store_token_response(token, flow, access_token).await?;
        Ok(Some(GitHubAccount::from(&account)))
    }

    async fn poll_builder_id_token(
        &self,
        region: &str,
        client_id: &str,
        client_secret: &str,
        device_code: &str,
    ) -> Result<BuilderIdTokenResponse, KiroOAuthError> {
        let url = format!("https://oidc.{region}.amazonaws.com/token");
        let response = self
            .http_client
            .post(url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .json(&serde_json::json!({
                "clientId": client_id,
                "clientSecret": client_secret,
                "deviceCode": device_code,
                "grantType": "urn:ietf:params:oauth:grant-type:device_code"
            }))
            .send()
            .await?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let token: BuilderIdTokenResponse = match serde_json::from_str(&body) {
            Ok(token) => token,
            Err(_) if !status.is_success() => {
                return Err(KiroOAuthError::TokenFetchFailed(format!(
                    "token poll failed: {status} {body}"
                )));
            }
            Err(err) => return Err(KiroOAuthError::ParseError(format!("{err}: {body}"))),
        };

        if let Some(error) = token.error.as_deref() {
            return match error {
                "authorization_pending" | "slow_down" => Err(KiroOAuthError::AuthorizationPending),
                "expired_token" => Err(KiroOAuthError::Timeout),
                "access_denied" => Err(KiroOAuthError::UserCancelled),
                _ => Err(KiroOAuthError::TokenFetchFailed(format!(
                    "{}: {}",
                    error,
                    token.error_description.unwrap_or_default()
                ))),
            };
        }

        if !status.is_success() {
            return Err(KiroOAuthError::TokenFetchFailed(format!(
                "token poll failed: {status} {body}"
            )));
        }

        Ok(token)
    }

    async fn store_token_response(
        &self,
        token: BuilderIdTokenResponse,
        flow: PendingDeviceFlow,
        access_token: String,
    ) -> Result<KiroAccountData, KiroOAuthError> {
        let refresh_token = token.refresh_token.clone().ok_or_else(|| {
            KiroOAuthError::TokenFetchFailed("响应缺少 refresh_token".to_string())
        })?;
        let account_id = format!("kiro_{}", sha256_hex(&refresh_token)[..24].to_string());
        let mut account = KiroAccountData {
            account_id: account_id.clone(),
            email: token.first_email(),
            refresh_token,
            profile_arn: token.profile_arn.clone(),
            auth_region: flow.region.clone(),
            api_region: flow.region.clone(),
            machine_id: Some(machine_id_from_refresh_token(
                token.refresh_token.as_deref().unwrap_or_default(),
            )),
            client_id: Some(flow.client_id),
            client_secret: Some(flow.client_secret),
            client_secret_expires_at: flow.client_secret_expires_at,
            start_url: Some(flow.start_url),
            auth_method: Some(KIRO_AUTH_METHOD_BUILDER_ID.to_string()),
            provider: Some("BuilderId".to_string()),
            authenticated_at: chrono::Utc::now().timestamp(),
        };

        if account.email.is_none() {
            if let Some(email) = self
                .fetch_account_email_from_usage(&account, &access_token)
                .await
            {
                account.email = Some(email);
            }
        }

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
        self.cache_access_token(
            &account_id,
            access_token,
            token.expires_in,
            token.refresh_token.as_deref(),
        )
        .await;
        self.save_to_disk().await?;
        Ok(account)
    }

    async fn fetch_account_email_from_usage(
        &self,
        account: &KiroAccountData,
        token: &str,
    ) -> Option<String> {
        tokio::time::timeout(std::time::Duration::from_secs(8), async {
            let response = self.send_usage_limits_request(account, token).await.ok()?;
            if !response.status().is_success() {
                return None;
            }
            response
                .json::<KiroUsageLimitsResponse>()
                .await
                .ok()
                .and_then(|usage| usage.account_email().map(str::to_string))
        })
        .await
        .ok()
        .flatten()
    }

    async fn cache_access_token(
        &self,
        account_id: &str,
        access_token: String,
        expires_in: Option<i64>,
        refresh_token: Option<&str>,
    ) {
        let expires_at_ms = expires_in
            .map(|s| chrono::Utc::now().timestamp_millis() + s * 1000)
            .unwrap_or_else(|| chrono::Utc::now().timestamp_millis() + 15 * 60 * 1000);
        self.access_tokens.write().await.insert(
            account_id.to_string(),
            CachedAccessToken {
                token: access_token,
                expires_at_ms,
            },
        );
        if let Some(refresh_token) = refresh_token {
            let mut accounts = self.accounts.write().await;
            if let Some(existing) = accounts.get_mut(account_id) {
                existing.refresh_token = refresh_token.to_string();
                existing.machine_id = Some(machine_id_from_refresh_token(refresh_token));
            }
        }
    }

    pub async fn get_valid_token_for_account(
        &self,
        account_id: &str,
    ) -> Result<String, KiroOAuthError> {
        self.require_builder_account(account_id).await?;

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

        let account = self.require_builder_account(account_id).await?;

        let token = self.refresh_builder_id_token(&account).await?;
        let access_token = token.access_token.clone().ok_or_else(|| {
            KiroOAuthError::TokenFetchFailed("refresh response missing accessToken".to_string())
        })?;
        let refreshed_email = token.first_email();
        let new_refresh_token = token.refresh_token.clone();

        {
            let mut accounts = self.accounts.write().await;
            if let Some(existing) = accounts.get_mut(account_id) {
                if existing.email.is_none() {
                    existing.email = refreshed_email;
                }
                if let Some(profile_arn) = token.profile_arn.clone() {
                    existing.profile_arn = Some(profile_arn);
                }
            }
        }
        self.cache_access_token(
            account_id,
            access_token.clone(),
            token.expires_in,
            new_refresh_token.as_deref(),
        )
        .await;
        self.save_to_disk().await?;
        Ok(access_token)
    }

    async fn refresh_builder_id_token(
        &self,
        account: &KiroAccountData,
    ) -> Result<BuilderIdTokenResponse, KiroOAuthError> {
        let client_id = account
            .client_id
            .as_deref()
            .ok_or_else(|| KiroOAuthError::RefreshTokenInvalid)?;
        let client_secret = account
            .client_secret
            .as_deref()
            .ok_or_else(|| KiroOAuthError::RefreshTokenInvalid)?;
        let region = if account.auth_region.trim().is_empty() {
            DEFAULT_REGION
        } else {
            account.auth_region.as_str()
        };
        let url = format!("https://oidc.{region}.amazonaws.com/token");

        let response = self
            .http_client
            .post(&url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .json(&serde_json::json!({
                "clientId": client_id,
                "clientSecret": client_secret,
                "refreshToken": account.refresh_token,
                "grantType": "refresh_token"
            }))
            .send()
            .await?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let token: BuilderIdTokenResponse = match serde_json::from_str(&body) {
            Ok(token) => token,
            Err(_) if !status.is_success() => {
                return Err(KiroOAuthError::TokenFetchFailed(format!(
                    "refresh failed: {status} {body}"
                )));
            }
            Err(err) => return Err(KiroOAuthError::ParseError(format!("{err}: {body}"))),
        };

        if let Some(error) = token.error.as_deref() {
            if error == "invalid_grant" {
                return Err(KiroOAuthError::RefreshTokenInvalid);
            }
            return Err(KiroOAuthError::TokenFetchFailed(format!(
                "{}: {}",
                error,
                token.error_description.unwrap_or_default()
            )));
        }

        if !status.is_success() {
            return Err(KiroOAuthError::TokenFetchFailed(format!(
                "refresh failed: {status} {body}"
            )));
        }

        Ok(token)
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
        self.accounts
            .read()
            .await
            .get(account_id)
            .filter(|account| account.is_builder_id())
            .cloned()
    }

    async fn require_builder_account(
        &self,
        account_id: &str,
    ) -> Result<KiroAccountData, KiroOAuthError> {
        let account = self
            .accounts
            .read()
            .await
            .get(account_id)
            .cloned()
            .ok_or_else(|| KiroOAuthError::AccountNotFound(account_id.to_string()))?;
        if !account.is_builder_id() {
            return Err(KiroOAuthError::LegacyAccountUnsupported);
        }
        Ok(account)
    }

    pub async fn get_default_account(&self) -> Option<KiroAccountData> {
        let id = self.resolve_default_account_id().await?;
        self.get_account(&id).await
    }

    pub async fn get_available_account_excluding(
        &self,
        excluded_account_ids: &HashSet<String>,
    ) -> Option<KiroAccountData> {
        self.prune_temporarily_unavailable_accounts().await;
        let default_account_id = self.resolve_default_account_id().await;
        let unavailable = self.temporarily_unavailable_until.read().await.clone();
        let now = chrono::Utc::now().timestamp();
        let accounts = self.accounts.read().await;
        let mut candidates: Vec<_> = accounts
            .values()
            .filter(|account| account.is_builder_id())
            .filter(|account| !excluded_account_ids.contains(&account.account_id))
            .filter(|account| {
                unavailable
                    .get(&account.account_id)
                    .map(|until| *until <= now)
                    .unwrap_or(true)
            })
            .cloned()
            .collect();
        candidates.sort_by(|a, b| {
            let a_default = default_account_id.as_deref() == Some(a.account_id.as_str());
            let b_default = default_account_id.as_deref() == Some(b.account_id.as_str());
            b_default
                .cmp(&a_default)
                .then_with(|| a.email.cmp(&b.email))
                .then_with(|| a.account_id.cmp(&b.account_id))
        });
        candidates.into_iter().next()
    }

    pub async fn mark_account_temporarily_unavailable(&self, account_id: &str, cooldown_secs: i64) {
        let until = chrono::Utc::now().timestamp() + cooldown_secs.max(1);
        self.temporarily_unavailable_until
            .write()
            .await
            .insert(account_id.to_string(), until);
    }

    async fn prune_temporarily_unavailable_accounts(&self) {
        let now = chrono::Utc::now().timestamp();
        self.temporarily_unavailable_until
            .write()
            .await
            .retain(|_, until| *until > now);
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

        let usage = response
            .json::<KiroUsageLimitsResponse>()
            .await
            .map_err(|e| KiroOAuthError::ParseError(e.to_string()))?;
        if let Some(email) = usage.account_email().map(str::to_string) {
            self.update_account_email_if_missing(account_id, email)
                .await?;
        }
        Ok(usage)
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
        let machine_id = account
            .machine_id
            .clone()
            .unwrap_or_else(|| machine_id_from_refresh_token(&account.refresh_token));
        let user_agent = format!(
            "aws-sdk-js/1.0.0 ua/2.1 os/macos lang/js md/nodejs#22.22.0 api/codewhispererruntime#1.0.0 m/N,E KiroIDE-2.3.0-{machine_id}"
        );
        let amz_user_agent = format!("aws-sdk-js/1.0.0 KiroIDE-2.3.0-{machine_id}");

        let q_host = format!("q.{region}.amazonaws.com");
        let profile_arn = resolve_profile_arn(account);

        for url in [
            usage_limits_url(&q_host, Some(&profile_arn)),
            usage_limits_url(&q_host, None),
        ] {
            let resp = self
                .send_usage_limits_get(&url, &q_host, &amz_user_agent, &user_agent, token)
                .await?;
            if resp.status().is_success() {
                return Ok(resp);
            }
        }

        // Fallback: 9router uses the legacy CodeWhisperer host with the same
        // query parameters, and it works for Builder ID sessions that have no
        // account-specific profileArn.
        let cw_host = format!("codewhisperer.{region}.amazonaws.com");
        let cw_url = usage_limits_url(&cw_host, None);
        let resp = self
            .send_usage_limits_get(&cw_url, &cw_host, &amz_user_agent, &user_agent, token)
            .await?;
        if resp.status().is_success() {
            return Ok(resp);
        }
        Ok(resp)
    }

    async fn send_usage_limits_get(
        &self,
        url: &str,
        host: &str,
        amz_user_agent: &str,
        user_agent: &str,
        token: &str,
    ) -> Result<reqwest::Response, KiroOAuthError> {
        self.http_client
            .get(url)
            .header("x-amz-user-agent", amz_user_agent)
            .header("user-agent", user_agent)
            .header("host", host)
            .header("Accept", "application/json")
            .header("amz-sdk-invocation-id", uuid::Uuid::new_v4().to_string())
            .header("amz-sdk-request", "attempt=1; max=1")
            .header("Authorization", format!("Bearer {token}"))
            .header("Connection", "close")
            .send()
            .await
            .map_err(KiroOAuthError::from)
    }

    pub async fn ensure_profile_arn_for_account(
        &self,
        account_id: &str,
        token: &str,
    ) -> Option<String> {
        let account = self.get_account(account_id).await?;
        if account
            .profile_arn
            .as_deref()
            .map(str::trim)
            .is_some_and(|value| !value.is_empty())
        {
            return Some(resolve_profile_arn(&account));
        }

        if !is_enterprise_account(&account) {
            return Some(resolve_profile_arn(&account));
        }

        match self.fetch_enterprise_profile_arn(&account, token).await {
            Ok(Some(profile_arn)) => {
                let mut changed = false;
                {
                    let mut accounts = self.accounts.write().await;
                    if let Some(existing) = accounts.get_mut(account_id) {
                        existing.profile_arn = Some(profile_arn.clone());
                        changed = true;
                    }
                }
                if changed {
                    if let Err(err) = self.save_to_disk().await {
                        log::warn!("[KiroOAuth] Enterprise profileArn 持久化失败: {err}");
                    }
                }
                Some(profile_arn)
            }
            Ok(None) => Some(resolve_profile_arn(&account)),
            Err(err) => {
                log::warn!("[KiroOAuth] Enterprise profileArn 获取失败: {err}");
                Some(resolve_profile_arn(&account))
            }
        }
    }

    async fn fetch_enterprise_profile_arn(
        &self,
        account: &KiroAccountData,
        token: &str,
    ) -> Result<Option<String>, KiroOAuthError> {
        let region = if account.api_region.trim().is_empty() {
            DEFAULT_REGION
        } else {
            account.api_region.as_str()
        };
        let host = if region.starts_with("eu-") {
            "codewhisperer.eu-central-1.amazonaws.com".to_string()
        } else {
            "codewhisperer.us-east-1.amazonaws.com".to_string()
        };
        let machine_id = account
            .machine_id
            .clone()
            .unwrap_or_else(|| machine_id_from_refresh_token(&account.refresh_token));
        let user_agent = format!(
            "aws-sdk-js/1.0.34 ua/2.1 os/macos lang/js md/nodejs#22.22.0 api/codewhispererruntime#1.0.34 m/E KiroIDE-2.3.0-{machine_id}"
        );
        let amz_user_agent = format!("aws-sdk-js/1.0.34 KiroIDE-2.3.0-{machine_id}");

        let response = self
            .http_client
            .post(format!("https://{host}/ListAvailableProfiles"))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .header("x-amz-user-agent", amz_user_agent)
            .header("user-agent", user_agent)
            .header("host", host)
            .header("amz-sdk-invocation-id", uuid::Uuid::new_v4().to_string())
            .header("amz-sdk-request", "attempt=1; max=1")
            .header("Authorization", format!("Bearer {token}"))
            .header("Connection", "close")
            .json(&serde_json::json!({}))
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            log::warn!(
                "[KiroOAuth] ListAvailableProfiles failed: {status} {}",
                body.chars().take(200).collect::<String>()
            );
            return Ok(None);
        }

        let value = response
            .json::<Value>()
            .await
            .map_err(|err| KiroOAuthError::ParseError(err.to_string()))?;
        Ok(value
            .get("profiles")
            .and_then(Value::as_array)
            .and_then(|profiles| {
                profiles.iter().find_map(|profile| {
                    profile
                        .get("arn")
                        .or_else(|| profile.get("profileArn"))
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|arn| !arn.is_empty())
                        .map(str::to_string)
                })
            }))
    }

    pub async fn update_account_email_if_missing(
        &self,
        account_id: &str,
        email: String,
    ) -> Result<(), KiroOAuthError> {
        if !email.contains('@') {
            return Ok(());
        }
        let mut changed = false;
        {
            let mut accounts = self.accounts.write().await;
            if let Some(account) = accounts.get_mut(account_id) {
                if account.email.is_none() {
                    account.email = Some(email);
                    changed = true;
                }
            }
        }
        if changed {
            self.save_to_disk().await?;
        }
        Ok(())
    }

    pub async fn remove_account(&self, account_id: &str) -> Result<(), KiroOAuthError> {
        {
            let mut accounts = self.accounts.write().await;
            if accounts.remove(account_id).is_none() {
                return Err(KiroOAuthError::AccountNotFound(account_id.to_string()));
            }
        }
        self.access_tokens.write().await.remove(account_id);
        self.temporarily_unavailable_until
            .write()
            .await
            .remove(account_id);
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
        let is_usable_account = self
            .accounts
            .read()
            .await
            .get(account_id)
            .map(|account| account.is_builder_id())
            .unwrap_or(false);
        if !is_usable_account {
            return Err(KiroOAuthError::AccountNotFound(account_id.to_string()));
        }
        *self.default_account_id.write().await = Some(account_id.to_string());
        self.save_to_disk().await
    }

    pub async fn logout(&self) -> Result<(), KiroOAuthError> {
        self.accounts.write().await.clear();
        self.access_tokens.write().await.clear();
        self.temporarily_unavailable_until.write().await.clear();
        self.pending_device_flows.write().await.clear();
        *self.default_account_id.write().await = None;
        self.save_to_disk().await
    }

    pub async fn get_status(&self) -> KiroOAuthStatus {
        self.hydrate_missing_account_emails().await;
        let accounts = self.accounts.read().await;
        let default_account_id = self.resolve_default_account_id().await;
        let legacy_count = accounts
            .values()
            .filter(|account| !account.is_builder_id())
            .count();
        let public_accounts =
            Self::sorted_public_accounts(&accounts, default_account_id.as_deref());
        KiroOAuthStatus {
            authenticated: !public_accounts.is_empty(),
            default_account_id: default_account_id.clone(),
            accounts: public_accounts,
            migration_error: if legacy_count > 0 {
                Some(format!(
                    "检测到 {legacy_count} 个旧版 Kiro Portal OAuth 账号。Kiro 已切换为 AWS Builder ID 设备码登录，请重新添加账号。"
                ))
            } else {
                None
            },
        }
    }

    async fn hydrate_missing_account_emails(&self) {
        let account_ids: Vec<String> = {
            let accounts = self.accounts.read().await;
            accounts
                .values()
                .filter(|account| account.email.is_none() && account.is_builder_id())
                .map(|account| account.account_id.clone())
                .collect()
        };
        if account_ids.is_empty() {
            return;
        }

        let mut changed = false;
        for account_id in account_ids {
            let Ok(token) = self.get_valid_token_for_account(&account_id).await else {
                continue;
            };
            let Some(mut account) = self.get_account(&account_id).await else {
                continue;
            };
            if account.email.is_some() {
                continue;
            }
            let Some(email) = self.fetch_account_email_from_usage(&account, &token).await else {
                continue;
            };
            account.email = Some(email);
            self.accounts
                .write()
                .await
                .insert(account_id.clone(), account);
            changed = true;
        }

        if changed {
            let _ = self.save_to_disk().await;
        }
    }

    fn fallback_default_account_id(accounts: &HashMap<String, KiroAccountData>) -> Option<String> {
        accounts
            .values()
            .filter(|account| account.is_builder_id())
            .map(|account| account.account_id.clone())
            .min()
    }

    fn sorted_public_accounts(
        accounts: &HashMap<String, KiroAccountData>,
        default_account_id: Option<&str>,
    ) -> Vec<GitHubAccount> {
        let mut out: Vec<_> = accounts
            .values()
            .filter(|account| account.is_builder_id())
            .map(GitHubAccount::from)
            .collect();
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
            if accounts
                .get(&id)
                .map(|account| account.is_builder_id())
                .unwrap_or(false)
            {
                return Some(id);
            }
        }
        Self::fallback_default_account_id(&accounts)
    }

    async fn save_to_disk(&self) -> Result<(), KiroOAuthError> {
        let accounts = self.accounts.read().await.clone();
        let default_account_id = self.resolve_default_account_id().await;
        let store = KiroOAuthStore {
            version: 2,
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
            if account.client_id.is_some() && account.auth_method.is_none() {
                account.auth_method = Some(KIRO_AUTH_METHOD_BUILDER_ID.to_string());
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
pub struct KiroOAuthStatus {
    pub authenticated: bool,
    pub default_account_id: Option<String>,
    pub accounts: Vec<GitHubAccount>,
    pub migration_error: Option<String>,
}

pub fn machine_id_from_refresh_token(refresh_token: &str) -> String {
    sha256_hex(&format!("KotlinNativeAPI/{refresh_token}"))
}

fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn usage_limits_url(host: &str, profile_arn: Option<&str>) -> String {
    let mut url = format!(
        "https://{host}/getUsageLimits?origin=AI_EDITOR&resourceType=AGENTIC_REQUEST&isEmailRequired=true"
    );
    if let Some(profile_arn) = profile_arn.filter(|v| !v.trim().is_empty()) {
        url.push_str("&profileArn=");
        url.push_str(&urlencoding::encode(profile_arn));
    }
    url
}

fn first_email(values: impl IntoIterator<Item = Option<String>>) -> Option<String> {
    values
        .into_iter()
        .flatten()
        .map(|value| value.trim().to_string())
        .find_map(|value| valid_email(&value).map(str::to_string))
}

fn email_from_jwt(token: &str) -> Option<String> {
    let mut parts = token.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
    let claims: Value = serde_json::from_slice(&bytes).ok()?;

    first_email_claim(&claims, &["email", "preferred_username", "username", "upn"]).or_else(|| {
        claims
            .get("identities")
            .and_then(Value::as_array)
            .and_then(|identities| {
                identities.iter().find_map(|identity| {
                    first_email_claim(
                        identity,
                        &[
                            "email",
                            "userId",
                            "user_id",
                            "providerName",
                            "provider_user_id",
                        ],
                    )
                })
            })
    })
}

fn first_email_claim(claims: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        claims
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .and_then(valid_email)
            .map(str::to_string)
    })
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

fn find_email_in_value(value: &Value) -> Option<&str> {
    match value {
        Value::String(value) => valid_email(value),
        Value::Array(values) => values.iter().find_map(find_email_in_value),
        Value::Object(map) => {
            for key in [
                "email",
                "accountEmail",
                "account_email",
                "preferredEmail",
                "preferred_email",
                "preferredUsername",
                "preferred_username",
                "userEmail",
                "user_email",
            ] {
                if let Some(email) = map.get(key).and_then(Value::as_str).and_then(valid_email) {
                    return Some(email);
                }
            }
            map.values().find_map(find_email_in_value)
        }
        _ => None,
    }
}

fn short_id(value: &str) -> String {
    if value.len() > 12 {
        format!("{}...{}", &value[..6], &value[value.len() - 4..])
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builder_account(email: Option<&str>) -> KiroAccountData {
        KiroAccountData {
            account_id: "kiro_test_account_1234".to_string(),
            email: email.map(str::to_string),
            refresh_token: "rt".to_string(),
            profile_arn: None,
            auth_region: DEFAULT_REGION.to_string(),
            api_region: DEFAULT_REGION.to_string(),
            machine_id: None,
            client_id: Some("client".to_string()),
            client_secret: Some("secret".to_string()),
            client_secret_expires_at: None,
            start_url: Some(DEFAULT_START_URL.to_string()),
            auth_method: Some(KIRO_AUTH_METHOD_BUILDER_ID.to_string()),
            provider: Some("BuilderId".to_string()),
            authenticated_at: 1,
        }
    }

    #[test]
    fn github_account_exposes_only_valid_kiro_email() {
        let public = GitHubAccount::from(&builder_account(Some("builder-subject")));
        assert_eq!(public.email, None);
        assert_eq!(public.login, "Kiro Builder ID (kiro_t...1234)");

        let public = GitHubAccount::from(&builder_account(Some("user@example.com")));
        assert_eq!(public.email.as_deref(), Some("user@example.com"));
        assert_eq!(public.login, "Kiro(user@example.com)");
    }

    #[test]
    fn usage_response_finds_nested_valid_email() {
        let usage = KiroUsageLimitsResponse {
            email: Some("subject-without-at".to_string()),
            account_email: None,
            user_email: None,
            next_date_reset: None,
            subscription_info: Some(KiroSubscriptionInfo {
                subscription_title: None,
                email: None,
                account_email: None,
                user_email: None,
                overage_capability: None,
                extra: HashMap::from([(
                    "profile".to_string(),
                    serde_json::json!({ "preferredEmail": "user@example.com" }),
                )]),
            }),
            usage_breakdown_list: Vec::new(),
            overage_configuration: None,
            extra: HashMap::new(),
        };

        assert_eq!(usage.account_email(), Some("user@example.com"));
    }

    #[test]
    fn usage_limits_url_includes_required_query_params() {
        let url = usage_limits_url(
            "q.us-east-1.amazonaws.com",
            Some("arn:aws:codewhisperer:us-east-1:123:profile/ABC"),
        );

        assert!(url.starts_with("https://q.us-east-1.amazonaws.com/getUsageLimits?"));
        assert!(url.contains("origin=AI_EDITOR"));
        assert!(url.contains("resourceType=AGENTIC_REQUEST"));
        assert!(url.contains("isEmailRequired=true"));
        assert!(url.contains("profileArn=arn%3Aaws%3Acodewhisperer"));
    }

    #[test]
    fn profile_arn_resolution_uses_social_and_enterprise_defaults() {
        let mut social = builder_account(Some("user@example.com"));
        social.auth_method = Some("social".to_string());
        social.provider = Some("Github".to_string());
        assert_eq!(resolve_profile_arn(&social), SOCIAL_PROFILE_ARN);

        let mut enterprise = builder_account(Some("user@example.com"));
        enterprise.auth_method = Some("external_idp".to_string());
        enterprise.provider = Some("Enterprise".to_string());
        enterprise.api_region = "eu-west-1".to_string();
        assert_eq!(
            resolve_profile_arn(&enterprise),
            "arn:aws:codewhisperer:eu-central-1:610548660232:profile/VNECVYCYYAWN"
        );

        enterprise.profile_arn =
            Some("arn:aws:codewhisperer:us-east-1:123:profile/REAL".to_string());
        assert_eq!(
            resolve_profile_arn(&enterprise),
            "arn:aws:codewhisperer:us-east-1:123:profile/REAL"
        );
    }
}
