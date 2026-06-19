//! OpenAI Official session account manager.
//!
//! This provider imports the JSON returned by `https://chatgpt.com/api/auth/session`.
//! It is intentionally separate from `codex_oauth_auth`: imported browser
//! sessions do not have the same refresh ownership guarantees as cc-switch's
//! OAuth device-code accounts.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::hash::Hasher;
use std::path::PathBuf;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::RwLock;
use twox_hash::XxHash64;

use crate::proxy::providers::copilot_auth::GitHubAccount;

const TOKEN_EXPIRY_BUFFER_MS: i64 = 60_000;
const CHATGPT_SESSION_URL: &str = "https://chatgpt.com/api/auth/session";
const OAUTH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const OPENAI_SESSION_USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/142.0.0.0 Safari/537.36";
const CODEX_USER_AGENT: &str = "cc-switch-openai-session";

#[derive(Error, Debug)]
pub enum OpenAISessionError {
    #[error("账号不存在: {0}")]
    AccountNotFound(String),

    #[error("Session 缺少 accessToken；请粘贴 chatgpt.com 的 __Secure-next-auth.session-token Cookie，或包含 accessToken 的 session JSON")]
    MissingAccessToken,

    #[error("Session accessToken 已过期且没有可刷新凭据；请重新导入 __Secure-next-auth.session-token Cookie，或改用 openai device/openai cli")]
    SessionExpired,

    #[error("Session accessToken 被 Codex 服务拒绝，且没有 sessionToken/refreshToken 可刷新；请重新导入 __Secure-next-auth.session-token Cookie，或改用 openai device/openai cli")]
    SessionTokenInvalidated,

    #[error("解析失败: {0}")]
    ParseError(String),

    #[error("存储失败: {0}")]
    StorageError(String),

    #[error("网络错误: {0}")]
    NetworkError(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OpenAISessionAccountData {
    pub account_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id_token: Option<String>,
    pub expires_at_ms: i64,
    pub authenticated_at: i64,
    pub imported_at: i64,
}

impl From<&OpenAISessionAccountData> for GitHubAccount {
    fn from(data: &OpenAISessionAccountData) -> Self {
        let login = data
            .email
            .clone()
            .or_else(|| data.name.clone())
            .unwrap_or_else(|| format!("ChatGPT ({})", data.account_id));
        GitHubAccount {
            id: data.account_id.clone(),
            login,
            email: data.email.clone(),
            avatar_url: None,
            authenticated_at: data.authenticated_at,
            github_domain: "chatgpt.com".to_string(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct OpenAISessionStore {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    accounts: HashMap<String, OpenAISessionAccountData>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default_account_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAISessionStatus {
    pub accounts: Vec<GitHubAccount>,
    pub default_account_id: Option<String>,
    pub authenticated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAISessionImportOutcome {
    pub account: GitHubAccount,
    pub action: OpenAISessionImportAction,
    pub expires_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenAISessionImportAction {
    Created,
    Updated,
}

#[derive(Debug, Clone)]
struct ParsedChatGptSession {
    account_id: String,
    email: Option<String>,
    name: Option<String>,
    access_token: String,
    refresh_token: Option<String>,
    session_token: Option<String>,
    id_token: Option<String>,
    expires_at_ms: i64,
}

pub struct OpenAISessionManager {
    accounts: Arc<RwLock<HashMap<String, OpenAISessionAccountData>>>,
    default_account_id: Arc<RwLock<Option<String>>>,
    storage_path: PathBuf,
    http_client: Client,
}

impl OpenAISessionManager {
    pub fn new(data_dir: PathBuf) -> Self {
        let storage_path = data_dir.join("openai_official_session_auth.json");
        let manager = Self {
            accounts: Arc::new(RwLock::new(HashMap::new())),
            default_account_id: Arc::new(RwLock::new(None)),
            storage_path,
            http_client: Client::new(),
        };
        if let Err(err) = manager.load_from_disk_sync() {
            log::warn!("[OpenAISession] 加载存储失败: {err}");
        }
        manager
    }

    pub async fn import_session_json(
        &self,
        raw_json: &str,
    ) -> Result<OpenAISessionImportOutcome, OpenAISessionError> {
        let parsed = self.parse_or_exchange_chatgpt_session(raw_json).await?;
        let now = Utc::now().timestamp();
        let data = OpenAISessionAccountData {
            account_id: parsed.account_id.clone(),
            email: parsed.email,
            name: parsed.name,
            access_token: parsed.access_token,
            refresh_token: parsed.refresh_token,
            session_token: parsed.session_token,
            id_token: parsed.id_token,
            expires_at_ms: parsed.expires_at_ms,
            authenticated_at: now,
            imported_at: now,
        };

        let action = {
            let mut accounts = self.accounts.write().await;
            let action = if accounts.contains_key(&data.account_id) {
                OpenAISessionImportAction::Updated
            } else {
                OpenAISessionImportAction::Created
            };
            accounts.insert(data.account_id.clone(), data.clone());
            action
        };

        {
            let mut default = self.default_account_id.write().await;
            if default.is_none() {
                *default = Some(data.account_id.clone());
            }
        }

        self.save_to_disk().await?;
        Ok(OpenAISessionImportOutcome {
            account: GitHubAccount::from(&data),
            action,
            expires_at_ms: data.expires_at_ms,
        })
    }

    pub async fn get_status(&self) -> OpenAISessionStatus {
        let accounts = self.list_accounts().await;
        let default_account_id = self.resolve_default_account_id().await;
        OpenAISessionStatus {
            authenticated: !accounts.is_empty(),
            accounts,
            default_account_id,
        }
    }

    pub async fn list_accounts(&self) -> Vec<GitHubAccount> {
        let accounts = self.accounts.read().await.clone();
        let default = self.resolve_default_account_id().await;
        let mut list: Vec<GitHubAccount> = accounts.values().map(GitHubAccount::from).collect();
        list.sort_by(|a, b| {
            let a_default = default.as_deref() == Some(a.id.as_str());
            let b_default = default.as_deref() == Some(b.id.as_str());
            b_default
                .cmp(&a_default)
                .then_with(|| b.authenticated_at.cmp(&a.authenticated_at))
                .then_with(|| a.login.cmp(&b.login))
        });
        list
    }

    pub async fn default_account_id(&self) -> Option<String> {
        self.resolve_default_account_id().await
    }

    pub async fn get_valid_token(&self) -> Result<String, OpenAISessionError> {
        let account_id = self
            .resolve_default_account_id()
            .await
            .ok_or_else(|| OpenAISessionError::AccountNotFound("default".to_string()))?;
        self.get_valid_token_for_account(&account_id).await
    }

    pub async fn get_valid_token_with_chatgpt_account_id(
        &self,
    ) -> Result<(String, String), OpenAISessionError> {
        let account_id = self
            .resolve_default_account_id()
            .await
            .ok_or_else(|| OpenAISessionError::AccountNotFound("default".to_string()))?;
        self.get_valid_token_with_chatgpt_account_id_for_account(&account_id)
            .await
    }

    pub async fn get_valid_token_for_account(
        &self,
        account_id: &str,
    ) -> Result<String, OpenAISessionError> {
        let account_id = account_id.trim();
        let account = self.ensure_fresh_account(account_id).await?;
        Ok(account.access_token)
    }

    pub async fn get_valid_token_with_chatgpt_account_id_for_account(
        &self,
        account_id: &str,
    ) -> Result<(String, String), OpenAISessionError> {
        let account_id = account_id.trim();
        let account = self.ensure_fresh_account(account_id).await?;
        let chatgpt_account_id =
            resolve_chatgpt_account_id(&account).unwrap_or_else(|| account.account_id.clone());
        Ok((account.access_token.clone(), chatgpt_account_id))
    }

    pub async fn invalidate_cached_token(&self, account_id: &str) {
        let account_id = account_id.trim();
        let mut accounts = self.accounts.write().await;
        if let Some(account) = accounts.get_mut(account_id) {
            account.expires_at_ms = 0;
            log::info!("[OpenAISession] 已标记 access_token 失效 (account={account_id})");
        }
    }

    pub async fn remove_account(&self, account_id: &str) -> Result<(), OpenAISessionError> {
        let account_id = account_id.trim();
        let removed = {
            let mut accounts = self.accounts.write().await;
            accounts.remove(account_id).is_some()
        };
        if !removed {
            return Err(OpenAISessionError::AccountNotFound(account_id.to_string()));
        }
        {
            let mut default = self.default_account_id.write().await;
            if default.as_deref() == Some(account_id) {
                *default = None;
            }
        }
        self.save_to_disk().await
    }

    pub async fn set_default_account(&self, account_id: &str) -> Result<(), OpenAISessionError> {
        let account_id = account_id.trim();
        if !self.accounts.read().await.contains_key(account_id) {
            return Err(OpenAISessionError::AccountNotFound(account_id.to_string()));
        }
        *self.default_account_id.write().await = Some(account_id.to_string());
        self.save_to_disk().await
    }

    pub async fn clear_auth(&self) -> Result<(), OpenAISessionError> {
        self.accounts.write().await.clear();
        *self.default_account_id.write().await = None;
        self.save_to_disk().await
    }

    async fn resolve_default_account_id(&self) -> Option<String> {
        let configured = self.default_account_id.read().await.clone();
        let accounts = self.accounts.read().await;
        if let Some(id) = configured {
            if accounts.contains_key(&id) {
                return Some(id);
            }
        }
        accounts.keys().min().cloned()
    }

    async fn ensure_fresh_account(
        &self,
        account_id: &str,
    ) -> Result<OpenAISessionAccountData, OpenAISessionError> {
        let account = {
            let accounts = self.accounts.read().await;
            accounts
                .get(account_id)
                .cloned()
                .ok_or_else(|| OpenAISessionError::AccountNotFound(account_id.to_string()))?
        };
        let now_ms = Utc::now().timestamp_millis();
        if account.expires_at_ms - now_ms > TOKEN_EXPIRY_BUFFER_MS {
            return Ok(account);
        }

        let refreshed = if let Some(refresh_token) = account
            .refresh_token
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            self.refresh_oauth_account(&account, refresh_token).await?
        } else if let Some(session_token) = account
            .session_token
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            self.exchange_session_cookie(session_token, Some(&account))
                .await?
        } else if account.expires_at_ms <= 0 {
            return Err(OpenAISessionError::SessionTokenInvalidated);
        } else {
            return Err(OpenAISessionError::SessionExpired);
        };

        {
            let mut accounts = self.accounts.write().await;
            accounts.insert(account_id.to_string(), refreshed.clone());
        }
        self.save_to_disk().await?;
        Ok(refreshed)
    }

    async fn parse_or_exchange_chatgpt_session(
        &self,
        raw_input: &str,
    ) -> Result<ParsedChatGptSession, OpenAISessionError> {
        let trimmed = raw_input.trim();
        if trimmed.is_empty() {
            return Err(OpenAISessionError::MissingAccessToken);
        }

        match serde_json::from_str::<Value>(trimmed) {
            Ok(value) => {
                let session_token = session_token_from_value(&value).map(str::to_string);
                if first_json_string(&value, ACCESS_TOKEN_PATHS).is_none() {
                    if let Some(session_token) = session_token.as_deref() {
                        let exchanged = self.exchange_session_cookie(session_token, None).await?;
                        return Ok(parsed_from_account(exchanged));
                    }
                }
                parse_chatgpt_session_value(value)
            }
            Err(_) => {
                let session_token = normalize_session_cookie_header(trimmed);
                let exchanged = self.exchange_session_cookie(&session_token, None).await?;
                Ok(parsed_from_account(exchanged))
            }
        }
    }

    async fn exchange_session_cookie(
        &self,
        session_token: &str,
        previous: Option<&OpenAISessionAccountData>,
    ) -> Result<OpenAISessionAccountData, OpenAISessionError> {
        let cookie = normalize_session_cookie_header(session_token);
        let response = self
            .http_client
            .get(CHATGPT_SESSION_URL)
            .header("Accept", "application/json")
            .header("User-Agent", OPENAI_SESSION_USER_AGENT)
            .header("Cookie", &cookie)
            .send()
            .await
            .map_err(|err| OpenAISessionError::NetworkError(err.to_string()))?;

        if matches!(
            response.status(),
            reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
        ) {
            return Err(OpenAISessionError::SessionTokenInvalidated);
        }
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(OpenAISessionError::NetworkError(format!(
                "Session exchange failed: {status} {body}"
            )));
        }

        let value: Value = response
            .json()
            .await
            .map_err(|err| OpenAISessionError::ParseError(err.to_string()))?;
        let mut parsed = parse_chatgpt_session_value(value)?;
        parsed.session_token = Some(cookie);
        Ok(account_from_parsed(parsed, previous))
    }

    async fn refresh_oauth_account(
        &self,
        previous: &OpenAISessionAccountData,
        refresh_token: &str,
    ) -> Result<OpenAISessionAccountData, OpenAISessionError> {
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
            .await
            .map_err(|err| OpenAISessionError::NetworkError(err.to_string()))?;

        if matches!(
            response.status(),
            reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
        ) {
            return Err(OpenAISessionError::SessionTokenInvalidated);
        }
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(OpenAISessionError::NetworkError(format!(
                "Refresh failed: {status} {body}"
            )));
        }

        let value: Value = response
            .json()
            .await
            .map_err(|err| OpenAISessionError::ParseError(err.to_string()))?;
        let mut parsed = parse_chatgpt_session_value(value)?;
        parsed.session_token = previous.session_token.clone();
        Ok(account_from_parsed(parsed, Some(previous)))
    }

    fn load_from_disk_sync(&self) -> Result<(), OpenAISessionError> {
        if !self.storage_path.exists() {
            return Ok(());
        }
        let content = fs::read_to_string(&self.storage_path)
            .map_err(|e| OpenAISessionError::StorageError(e.to_string()))?;
        let store: OpenAISessionStore = serde_json::from_str(&content)
            .map_err(|e| OpenAISessionError::ParseError(e.to_string()))?;
        *self.accounts.blocking_write() = store.accounts;
        *self.default_account_id.blocking_write() = store.default_account_id;
        Ok(())
    }

    async fn save_to_disk(&self) -> Result<(), OpenAISessionError> {
        let store = OpenAISessionStore {
            version: 1,
            accounts: self.accounts.read().await.clone(),
            default_account_id: self.resolve_default_account_id().await,
        };
        let content = serde_json::to_string_pretty(&store)
            .map_err(|e| OpenAISessionError::ParseError(e.to_string()))?;
        if let Some(parent) = self.storage_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| OpenAISessionError::StorageError(e.to_string()))?;
        }
        let tmp = self.storage_path.with_extension("json.tmp");
        fs::write(&tmp, content).map_err(|e| OpenAISessionError::StorageError(e.to_string()))?;
        fs::rename(&tmp, &self.storage_path)
            .map_err(|e| OpenAISessionError::StorageError(e.to_string()))?;
        Ok(())
    }
}

const ACCESS_TOKEN_PATHS: &[&[&str]] = &[
    &["accessToken"],
    &["access_token"],
    &["token"],
    &["tokens", "access_token"],
    &["tokens", "accessToken"],
    &["token", "access_token"],
    &["token", "accessToken"],
    &["credentials", "accessToken"],
    &["credentials", "access_token"],
    &["OPENAI_API_KEY"],
];
const REFRESH_TOKEN_PATHS: &[&[&str]] = &[
    &["refreshToken"],
    &["refresh_token"],
    &["tokens", "refresh_token"],
    &["tokens", "refreshToken"],
    &["token", "refresh_token"],
    &["token", "refreshToken"],
    &["credentials", "refreshToken"],
    &["credentials", "refresh_token"],
];
const SESSION_TOKEN_PATHS: &[&[&str]] = &[
    &["sessionToken"],
    &["session_token"],
    &["tokens", "session_token"],
    &["tokens", "sessionToken"],
    &["token", "session_token"],
    &["token", "sessionToken"],
    &["credentials", "sessionToken"],
    &["credentials", "session_token"],
    &["cookie"],
    &["session-token"],
    &["__Secure-next-auth.session-token"],
];
const ID_TOKEN_PATHS: &[&[&str]] = &[
    &["idToken"],
    &["id_token"],
    &["tokens", "id_token"],
    &["tokens", "idToken"],
    &["token", "id_token"],
    &["token", "idToken"],
];
const CHATGPT_ACCOUNT_ID_PATHS: &[&[&str]] = &[
    &["chatgptAccountId"],
    &["chatgpt_account_id"],
    &["accountId"],
    &["account_id"],
    &["tokens", "chatgpt_account_id"],
    &["tokens", "chatgptAccountId"],
    &["providerSpecificData", "chatgptAccountId"],
    &["providerSpecificData", "chatgpt_account_id"],
    &["meta", "chatgpt_account_id"],
];

#[cfg(test)]
fn parse_chatgpt_session(raw_json: &str) -> Result<ParsedChatGptSession, OpenAISessionError> {
    let value: Value = serde_json::from_str(raw_json)
        .map_err(|e| OpenAISessionError::ParseError(e.to_string()))?;
    parse_chatgpt_session_value(value)
}

fn parse_chatgpt_session_value(value: Value) -> Result<ParsedChatGptSession, OpenAISessionError> {
    let access_token = first_json_string(&value, ACCESS_TOKEN_PATHS)
        .map(str::to_string)
        .ok_or(OpenAISessionError::MissingAccessToken)?;
    let refresh_token = first_json_string(&value, REFRESH_TOKEN_PATHS).map(str::to_string);
    let session_token = session_token_from_value(&value).map(str::to_string);
    let id_token = first_json_string(&value, ID_TOKEN_PATHS).map(str::to_string);
    let email = value
        .pointer("/user/email")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
        .or_else(|| {
            decode_jwt_claims(id_token.as_deref().unwrap_or(&access_token)).and_then(|claims| {
                claims
                    .get("email")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
        });
    let name = value
        .pointer("/user/name")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string);

    let jwt_exp_ms = decode_jwt_claims(&access_token).and_then(|claims| {
        claims
            .get("exp")
            .and_then(Value::as_i64)
            .map(|seconds| seconds * 1000)
    });
    let expires_at_ms = jwt_exp_ms
        .or_else(|| {
            first_json_string(&value, &[&["expires"], &["expiresAt"], &["expires_at"]])
                .and_then(parse_expiry_ms)
        })
        .unwrap_or_else(|| Utc::now().timestamp_millis() + 60 * 60 * 1000);

    let now_ms = Utc::now().timestamp_millis();
    if expires_at_ms <= now_ms + TOKEN_EXPIRY_BUFFER_MS {
        return Err(OpenAISessionError::SessionExpired);
    }

    let account_id = id_token
        .as_deref()
        .and_then(decode_jwt_claims)
        .and_then(|claims| chatgpt_account_id_from_claims(&claims))
        .or_else(|| first_json_string(&value, CHATGPT_ACCOUNT_ID_PATHS).map(str::to_string))
        .or_else(|| {
            decode_jwt_claims(&access_token)
                .and_then(|claims| chatgpt_account_id_from_claims(&claims))
        })
        .or_else(|| {
            value
                .pointer("/user/id")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .or_else(|| email.clone())
        .unwrap_or_else(|| stable_session_id(&access_token));

    Ok(ParsedChatGptSession {
        account_id,
        email,
        name,
        access_token,
        refresh_token,
        session_token,
        id_token,
        expires_at_ms,
    })
}

fn first_json_string<'a>(value: &'a Value, paths: &[&[&str]]) -> Option<&'a str> {
    paths
        .iter()
        .find_map(|path| string_at_path(value, path))
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn string_at_path<'a>(value: &'a Value, path: &[&str]) -> Option<&'a str> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    current.as_str()
}

fn session_token_from_value(value: &Value) -> Option<&str> {
    first_json_string(value, SESSION_TOKEN_PATHS)
}

fn normalize_session_cookie_header(raw: &str) -> String {
    let mut value = raw.trim();
    if let Some(stripped) = value.strip_prefix("Cookie:") {
        value = stripped.trim();
    } else if let Some(stripped) = value.strip_prefix("cookie:") {
        value = stripped.trim();
    }
    if value.contains("__Secure-next-auth.session-token")
        || value.contains("__Secure-next-auth.session-token.")
    {
        value.to_string()
    } else {
        format!("__Secure-next-auth.session-token={value}")
    }
}

fn account_from_parsed(
    parsed: ParsedChatGptSession,
    previous: Option<&OpenAISessionAccountData>,
) -> OpenAISessionAccountData {
    OpenAISessionAccountData {
        account_id: previous
            .map(|item| item.account_id.clone())
            .unwrap_or(parsed.account_id),
        email: parsed
            .email
            .or_else(|| previous.and_then(|item| item.email.clone())),
        name: parsed
            .name
            .or_else(|| previous.and_then(|item| item.name.clone())),
        access_token: parsed.access_token,
        refresh_token: parsed
            .refresh_token
            .or_else(|| previous.and_then(|item| item.refresh_token.clone())),
        session_token: parsed
            .session_token
            .or_else(|| previous.and_then(|item| item.session_token.clone())),
        id_token: parsed
            .id_token
            .or_else(|| previous.and_then(|item| item.id_token.clone())),
        expires_at_ms: parsed.expires_at_ms,
        authenticated_at: previous
            .map(|item| item.authenticated_at)
            .unwrap_or_else(|| Utc::now().timestamp()),
        imported_at: Utc::now().timestamp(),
    }
}

fn parsed_from_account(account: OpenAISessionAccountData) -> ParsedChatGptSession {
    ParsedChatGptSession {
        account_id: account.account_id,
        email: account.email,
        name: account.name,
        access_token: account.access_token,
        refresh_token: account.refresh_token,
        session_token: account.session_token,
        id_token: account.id_token,
        expires_at_ms: account.expires_at_ms,
    }
}

fn parse_expiry_ms(value: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

fn decode_jwt_claims(token: &str) -> Option<Value> {
    let mut parts = token.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    let decoded = URL_SAFE_NO_PAD.decode(payload).ok()?;
    serde_json::from_slice(&decoded).ok()
}

fn resolve_chatgpt_account_id(account: &OpenAISessionAccountData) -> Option<String> {
    decode_jwt_claims(&account.access_token)
        .and_then(|claims| chatgpt_account_id_from_claims(&claims))
}

fn chatgpt_account_id_from_claims(claims: &Value) -> Option<String> {
    claims
        .get("chatgpt_account_id")
        .or_else(|| {
            claims
                .get("https://api.openai.com/auth")
                .and_then(|value| value.get("chatgpt_account_id"))
        })
        .or_else(|| {
            claims
                .get("https://api.openai.com/profile")
                .and_then(|value| value.get("chatgpt_account_id"))
        })
        .or_else(|| {
            claims
                .get("openai_auth")
                .and_then(|value| value.get("chatgpt_account_id"))
        })
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn stable_session_id(token: &str) -> String {
    let mut hasher = XxHash64::with_seed(0);
    hasher.write(token.as_bytes());
    format!("session-{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    fn jwt(account_id: &str, exp: i64) -> String {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#);
        let payload = URL_SAFE_NO_PAD
            .encode(format!(r#"{{"chatgpt_account_id":"{account_id}","exp":{exp}}}"#).as_bytes());
        format!("{header}.{payload}.")
    }

    fn jwt_with_openai_auth_account(account_id: &str, user_id: &str, exp: i64) -> String {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#);
        let payload = URL_SAFE_NO_PAD.encode(
            serde_json::json!({
                "https://api.openai.com/auth": {
                    "chatgpt_account_id": account_id,
                    "chatgpt_user_id": user_id
                },
                "https://api.openai.com/profile": {
                    "email": "u@example.com"
                },
                "exp": exp
            })
            .to_string()
            .as_bytes(),
        );
        format!("{header}.{payload}.")
    }

    #[test]
    fn parses_chatgpt_session_json() {
        let exp = Utc::now().timestamp() + 3600;
        let token = jwt("acct-1", exp);
        let raw = serde_json::json!({
            "accessToken": token,
            "expires": DateTime::<Utc>::from_timestamp(exp, 0).unwrap().to_rfc3339(),
            "user": {"email": "u@example.com", "name": "User"}
        })
        .to_string();

        let parsed = parse_chatgpt_session(&raw).unwrap();
        assert_eq!(parsed.account_id, "acct-1");
        assert_eq!(parsed.email.as_deref(), Some("u@example.com"));
        assert_eq!(parsed.expires_at_ms, exp * 1000);
    }

    #[test]
    fn parses_nested_openai_auth_chatgpt_account_id() {
        let exp = Utc::now().timestamp() + 3600;
        let token = jwt_with_openai_auth_account("workspace-1", "user-1", exp);
        let raw = serde_json::json!({
            "accessToken": token,
            "user": {"id": "user-1", "email": "u@example.com"}
        })
        .to_string();

        let parsed = parse_chatgpt_session(&raw).unwrap();
        assert_eq!(parsed.account_id, "workspace-1");

        let legacy_data = OpenAISessionAccountData {
            account_id: "user-1".to_string(),
            email: Some("u@example.com".to_string()),
            name: None,
            access_token: token,
            refresh_token: None,
            session_token: None,
            id_token: None,
            expires_at_ms: exp * 1000,
            authenticated_at: Utc::now().timestamp(),
            imported_at: Utc::now().timestamp(),
        };
        assert_eq!(
            resolve_chatgpt_account_id(&legacy_data).as_deref(),
            Some("workspace-1")
        );
    }

    #[test]
    fn parses_codex_native_auth_json() {
        let exp = Utc::now().timestamp() + 3600;
        let access = jwt("acct-access", exp);
        let id_token = jwt_with_openai_auth_account("acct-id", "user-1", exp);
        let raw = serde_json::json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "access_token": access,
                "refresh_token": "refresh-token",
                "id_token": id_token,
                "account_id": "acct-id"
            }
        })
        .to_string();

        let parsed = parse_chatgpt_session(&raw).unwrap();
        assert_eq!(parsed.account_id, "acct-id");
        assert_eq!(parsed.refresh_token.as_deref(), Some("refresh-token"));
        assert_eq!(parsed.id_token.as_deref(), Some(id_token.as_str()));
    }

    #[test]
    fn parses_session_token_from_json() {
        let exp = Utc::now().timestamp() + 3600;
        let token = jwt("acct-1", exp);
        let raw = serde_json::json!({
            "accessToken": token,
            "sessionToken": "__Secure-next-auth.session-token=session-value"
        })
        .to_string();

        let parsed = parse_chatgpt_session(&raw).unwrap();
        assert_eq!(
            parsed.session_token.as_deref(),
            Some("__Secure-next-auth.session-token=session-value")
        );
    }

    #[test]
    fn refreshed_account_preserves_existing_storage_identity() {
        let exp = Utc::now().timestamp() + 3600;
        let previous = OpenAISessionAccountData {
            account_id: "bound-account".to_string(),
            email: Some("old@example.com".to_string()),
            name: None,
            access_token: jwt("old-chatgpt", exp),
            refresh_token: Some("refresh-token".to_string()),
            session_token: None,
            id_token: None,
            expires_at_ms: exp * 1000,
            authenticated_at: Utc::now().timestamp(),
            imported_at: Utc::now().timestamp(),
        };
        let parsed = ParsedChatGptSession {
            account_id: "new-token-derived-id".to_string(),
            email: Some("new@example.com".to_string()),
            name: None,
            access_token: jwt("new-chatgpt", exp),
            refresh_token: Some("new-refresh-token".to_string()),
            session_token: None,
            id_token: None,
            expires_at_ms: exp * 1000,
        };

        let refreshed = account_from_parsed(parsed, Some(&previous));

        assert_eq!(refreshed.account_id, "bound-account");
        assert_eq!(refreshed.email.as_deref(), Some("new@example.com"));
        assert_eq!(
            refreshed.refresh_token.as_deref(),
            Some("new-refresh-token")
        );
    }

    #[test]
    fn normalizes_bare_session_cookie_value() {
        assert_eq!(
            normalize_session_cookie_header("abc123"),
            "__Secure-next-auth.session-token=abc123"
        );
        assert_eq!(
            normalize_session_cookie_header("Cookie: __Secure-next-auth.session-token.0=a; __Secure-next-auth.session-token.1=b"),
            "__Secure-next-auth.session-token.0=a; __Secure-next-auth.session-token.1=b"
        );
    }

    #[test]
    fn rejects_expired_session() {
        let exp = Utc::now().timestamp() - 60;
        let token = jwt("acct-1", exp);
        let raw = serde_json::json!({ "accessToken": token }).to_string();
        assert!(matches!(
            parse_chatgpt_session(&raw),
            Err(OpenAISessionError::SessionExpired)
        ));
    }
}
