use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::HashMap, fs, io::Write, path::PathBuf, sync::Arc};
use tokio::sync::{Mutex, RwLock};

const LOGIN_URL: &str = "https://chat.deepseek.com/api/v0/users/login";
const USER_AGENT: &str = "DeepSeek/2.0.4 Android/35";

#[derive(Debug, thiserror::Error)]
pub enum DeepSeekAccountError {
    #[error("account not found: {0}")]
    AccountNotFound(String),
    #[error("missing email or mobile")]
    MissingIdentifier,
    #[error("network error: {0}")]
    Network(String),
    #[error("parse error: {0}")]
    Parse(String),
    #[error("io error: {0}")]
    Io(String),
    #[error("login failed: {0}")]
    Login(String),
}

impl From<std::io::Error> for DeepSeekAccountError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err.to_string())
    }
}

impl From<reqwest::Error> for DeepSeekAccountError {
    fn from(err: reqwest::Error) -> Self {
        Self::Network(err.to_string())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeepSeekAccountData {
    pub account_id: String,
    pub login: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mobile: Option<String>,
    pub password: String,
    pub authenticated_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeepSeekManagedAccount {
    pub id: String,
    pub login: String,
    pub authenticated_at: i64,
    pub is_default: bool,
    pub has_password: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeepSeekAccountStatus {
    pub authenticated: bool,
    pub default_account_id: Option<String>,
    pub accounts: Vec<DeepSeekManagedAccount>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct DeepSeekAccountStore {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    accounts: HashMap<String, DeepSeekAccountData>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default_account_id: Option<String>,
}

#[derive(Debug, Clone)]
struct CachedToken {
    token: String,
}

pub struct DeepSeekAccountManager {
    accounts: Arc<RwLock<HashMap<String, DeepSeekAccountData>>>,
    default_account_id: Arc<RwLock<Option<String>>>,
    tokens: Arc<RwLock<HashMap<String, CachedToken>>>,
    login_locks: Arc<RwLock<HashMap<String, Arc<Mutex<()>>>>>,
    storage_path: PathBuf,
    http: Client,
}

impl DeepSeekAccountManager {
    pub fn new(data_dir: PathBuf) -> Self {
        let manager = Self {
            accounts: Arc::new(RwLock::new(HashMap::new())),
            default_account_id: Arc::new(RwLock::new(None)),
            tokens: Arc::new(RwLock::new(HashMap::new())),
            login_locks: Arc::new(RwLock::new(HashMap::new())),
            storage_path: data_dir.join("deepseek_account_auth.json"),
            http: Client::builder()
                .user_agent(USER_AGENT)
                .build()
                .unwrap_or_else(|_| Client::new()),
        };
        if let Err(err) = manager.load_from_disk_sync() {
            log::warn!("[DeepSeekAccount] load store failed: {err}");
        }
        manager
    }

    pub async fn add_account(
        &self,
        email: Option<String>,
        mobile: Option<String>,
        password: String,
    ) -> Result<DeepSeekManagedAccount, DeepSeekAccountError> {
        let email = clean_opt(email);
        let mobile = clean_opt(mobile);
        let password = password.trim().to_string();
        let account_id = email
            .clone()
            .or_else(|| mobile.clone())
            .ok_or(DeepSeekAccountError::MissingIdentifier)?;
        if password.is_empty() {
            return Err(DeepSeekAccountError::Login(
                "password is required".to_string(),
            ));
        }
        let data = DeepSeekAccountData {
            account_id: account_id.clone(),
            login: account_id.clone(),
            email,
            mobile,
            password,
            authenticated_at: chrono::Utc::now().timestamp(),
        };
        let token = self.login(&data).await?;
        {
            let mut accounts = self.accounts.write().await;
            accounts.insert(account_id.clone(), data);
        }
        {
            let mut tokens = self.tokens.write().await;
            tokens.insert(account_id.clone(), CachedToken { token });
        }
        {
            let mut default_id = self.default_account_id.write().await;
            if default_id.is_none() {
                *default_id = Some(account_id.clone());
            }
        }
        self.save_to_disk().await?;
        self.account_public(&account_id).await
    }

    pub async fn list_accounts(&self) -> Vec<DeepSeekManagedAccount> {
        let accounts = self.accounts.read().await;
        let default_id = self.resolve_default_account_id_locked(&accounts).await;
        let mut list: Vec<_> = accounts
            .values()
            .map(|a| public_account(a, default_id.as_deref()))
            .collect();
        list.sort_by(|a, b| {
            b.authenticated_at
                .cmp(&a.authenticated_at)
                .then(a.id.cmp(&b.id))
        });
        list
    }

    pub async fn get_status(&self) -> DeepSeekAccountStatus {
        let accounts = self.list_accounts().await;
        DeepSeekAccountStatus {
            authenticated: !accounts.is_empty(),
            default_account_id: self.default_account_id().await,
            accounts,
        }
    }

    pub async fn default_account_id(&self) -> Option<String> {
        let accounts = self.accounts.read().await;
        self.resolve_default_account_id_locked(&accounts).await
    }

    pub async fn get_valid_token(&self) -> Result<String, DeepSeekAccountError> {
        let account_id = self
            .default_account_id()
            .await
            .ok_or_else(|| DeepSeekAccountError::AccountNotFound("default".to_string()))?;
        self.get_valid_token_for_account(&account_id).await
    }

    pub async fn get_valid_token_for_account(
        &self,
        account_id: &str,
    ) -> Result<String, DeepSeekAccountError> {
        {
            let tokens = self.tokens.read().await;
            if let Some(cached) = tokens.get(account_id) {
                return Ok(cached.token.clone());
            }
        }
        let lock = self.login_lock(account_id).await;
        let _guard = lock.lock().await;
        {
            let tokens = self.tokens.read().await;
            if let Some(cached) = tokens.get(account_id) {
                return Ok(cached.token.clone());
            }
        }
        let account = {
            let accounts = self.accounts.read().await;
            accounts
                .get(account_id)
                .cloned()
                .ok_or_else(|| DeepSeekAccountError::AccountNotFound(account_id.to_string()))?
        };
        let token = self.login(&account).await?;
        let mut tokens = self.tokens.write().await;
        tokens.insert(
            account_id.to_string(),
            CachedToken {
                token: token.clone(),
            },
        );
        Ok(token)
    }

    pub async fn invalidate_cached_token(&self, account_id: &str) {
        self.tokens.write().await.remove(account_id);
    }

    pub async fn remove_account(&self, account_id: &str) -> Result<(), DeepSeekAccountError> {
        {
            let mut accounts = self.accounts.write().await;
            if accounts.remove(account_id).is_none() {
                return Err(DeepSeekAccountError::AccountNotFound(
                    account_id.to_string(),
                ));
            }
            let mut default_id = self.default_account_id.write().await;
            if default_id.as_deref() == Some(account_id) {
                *default_id = fallback_default_account_id(&accounts);
            }
        }
        self.tokens.write().await.remove(account_id);
        self.login_locks.write().await.remove(account_id);
        self.save_to_disk().await
    }

    pub async fn set_default_account(&self, account_id: &str) -> Result<(), DeepSeekAccountError> {
        {
            let accounts = self.accounts.read().await;
            if !accounts.contains_key(account_id) {
                return Err(DeepSeekAccountError::AccountNotFound(
                    account_id.to_string(),
                ));
            }
        }
        *self.default_account_id.write().await = Some(account_id.to_string());
        self.save_to_disk().await
    }

    async fn login(&self, account: &DeepSeekAccountData) -> Result<String, DeepSeekAccountError> {
        let mut payload = json!({
            "password": account.password,
            "device_id": "cc_switch_deepseek_account",
            "os": "android"
        });
        if let Some(email) = &account.email {
            payload["email"] = Value::String(email.clone());
        } else if let Some(mobile) = &account.mobile {
            payload["mobile"] = Value::String(mobile.clone());
        } else {
            return Err(DeepSeekAccountError::MissingIdentifier);
        }
        let resp = self
            .http
            .post(LOGIN_URL)
            .headers(deepseek_base_headers())
            .json(&payload)
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(DeepSeekAccountError::Login(format!(
                "HTTP {status}: {text}"
            )));
        }
        let value: Value =
            serde_json::from_str(&text).map_err(|e| DeepSeekAccountError::Parse(e.to_string()))?;
        ensure_ok(&value, "login")?;
        value
            .pointer("/data/biz_data/user/token")
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|token| !token.trim().is_empty())
            .ok_or_else(|| DeepSeekAccountError::Login("missing token".to_string()))
    }

    async fn login_lock(&self, account_id: &str) -> Arc<Mutex<()>> {
        {
            let locks = self.login_locks.read().await;
            if let Some(lock) = locks.get(account_id) {
                return Arc::clone(lock);
            }
        }
        let mut locks = self.login_locks.write().await;
        Arc::clone(
            locks
                .entry(account_id.to_string())
                .or_insert_with(|| Arc::new(Mutex::new(()))),
        )
    }

    async fn account_public(
        &self,
        account_id: &str,
    ) -> Result<DeepSeekManagedAccount, DeepSeekAccountError> {
        let accounts = self.accounts.read().await;
        let default_id = self.resolve_default_account_id_locked(&accounts).await;
        accounts
            .get(account_id)
            .map(|a| public_account(a, default_id.as_deref()))
            .ok_or_else(|| DeepSeekAccountError::AccountNotFound(account_id.to_string()))
    }

    async fn resolve_default_account_id_locked(
        &self,
        accounts: &HashMap<String, DeepSeekAccountData>,
    ) -> Option<String> {
        let configured = self.default_account_id.read().await.clone();
        if configured
            .as_ref()
            .is_some_and(|id| accounts.contains_key(id.as_str()))
        {
            return configured;
        }
        fallback_default_account_id(accounts)
    }

    fn load_from_disk_sync(&self) -> Result<(), DeepSeekAccountError> {
        if !self.storage_path.exists() {
            return Ok(());
        }
        let content = fs::read_to_string(&self.storage_path)?;
        let store: DeepSeekAccountStore = serde_json::from_str(&content)
            .map_err(|e| DeepSeekAccountError::Parse(e.to_string()))?;
        *self.accounts.blocking_write() = store.accounts;
        *self.default_account_id.blocking_write() = store.default_account_id;
        Ok(())
    }

    async fn save_to_disk(&self) -> Result<(), DeepSeekAccountError> {
        let store = DeepSeekAccountStore {
            version: 1,
            accounts: self.accounts.read().await.clone(),
            default_account_id: self.default_account_id.read().await.clone(),
        };
        let content = serde_json::to_string_pretty(&store)
            .map_err(|e| DeepSeekAccountError::Parse(e.to_string()))?;
        if let Some(parent) = self.storage_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = self.storage_path.with_extension("json.tmp");
        {
            let mut file = fs::File::create(&tmp)?;
            file.write_all(content.as_bytes())?;
            file.sync_all()?;
        }
        fs::rename(tmp, &self.storage_path)?;
        Ok(())
    }
}

pub fn deepseek_base_headers() -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::ACCEPT,
        reqwest::header::HeaderValue::from_static("application/json"),
    );
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json"),
    );
    headers.insert(
        "accept-charset",
        reqwest::header::HeaderValue::from_static("UTF-8"),
    );
    headers.insert(
        "x-client-platform",
        reqwest::header::HeaderValue::from_static("android"),
    );
    headers.insert(
        "x-client-version",
        reqwest::header::HeaderValue::from_static("2.0.4"),
    );
    headers.insert(
        "x-client-locale",
        reqwest::header::HeaderValue::from_static("zh_CN"),
    );
    headers
}

pub fn ensure_ok(value: &Value, op: &str) -> Result<(), DeepSeekAccountError> {
    let code = value.get("code").and_then(Value::as_i64).unwrap_or(0);
    let biz_code = value
        .pointer("/data/biz_code")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    if code == 0 && biz_code == 0 {
        return Ok(());
    }
    let msg = value
        .pointer("/data/biz_msg")
        .or_else(|| value.get("msg"))
        .and_then(Value::as_str)
        .unwrap_or("unknown error");
    Err(DeepSeekAccountError::Login(format!(
        "{op} failed: code={code} biz_code={biz_code} msg={msg}"
    )))
}

fn clean_opt(value: Option<String>) -> Option<String> {
    value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn fallback_default_account_id(accounts: &HashMap<String, DeepSeekAccountData>) -> Option<String> {
    accounts
        .iter()
        .max_by(|(id_a, a), (id_b, b)| {
            a.authenticated_at
                .cmp(&b.authenticated_at)
                .then_with(|| id_b.cmp(id_a))
        })
        .map(|(id, _)| id.clone())
}

fn public_account(
    account: &DeepSeekAccountData,
    default_account_id: Option<&str>,
) -> DeepSeekManagedAccount {
    DeepSeekManagedAccount {
        id: account.account_id.clone(),
        login: account.login.clone(),
        authenticated_at: account.authenticated_at,
        is_default: default_account_id == Some(account.account_id.as_str()),
        has_password: !account.password.is_empty(),
    }
}
