use super::ollama_cloud::{parse_models_response, parse_tags_response, OllamaModel};
use crate::provider::Provider;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{collections::HashMap, fs, path::PathBuf, sync::Arc};
use tokio::sync::RwLock;

const MODELS_URL: &str = "https://ollama.com/v1/models";
const TAGS_URL: &str = "https://ollama.com/api/tags";

#[derive(Debug, thiserror::Error)]
pub enum OllamaCloudError {
    #[error("account not found: {0}")]
    AccountNotFound(String),
    #[error("missing api key")]
    MissingApiKey,
    #[error("invalid api key format")]
    InvalidApiKeyFormat,
    #[error("network error: {0}")]
    Network(String),
    #[error("parse error: {0}")]
    Parse(String),
    #[error("io error: {0}")]
    Io(String),
    #[error("auth failed ({0}): {1}")]
    Auth(u16, String),
}

impl From<std::io::Error> for OllamaCloudError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err.to_string())
    }
}

impl From<reqwest::Error> for OllamaCloudError {
    fn from(err: reqwest::Error) -> Self {
        Self::Network(err.to_string())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaCloudAccount {
    pub id: String,
    pub api_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OllamaCloudManagedAccount {
    pub id: String,
    pub label: Option<String>,
    pub created_at: i64,
    pub is_default: bool,
    pub masked_key: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OllamaCloudStatus {
    pub authenticated: bool,
    pub default_account_id: Option<String>,
    pub accounts: Vec<OllamaCloudManagedAccount>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct OllamaCloudStore {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    accounts: HashMap<String, OllamaCloudAccount>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default_account_id: Option<String>,
}

pub struct OllamaCloudAccountManager {
    accounts: Arc<RwLock<HashMap<String, OllamaCloudAccount>>>,
    default_account_id: Arc<RwLock<Option<String>>>,
    storage_path: PathBuf,
    http: Client,
}

impl OllamaCloudAccountManager {
    pub fn new(data_dir: PathBuf) -> Self {
        let manager = Self {
            accounts: Arc::new(RwLock::new(HashMap::new())),
            default_account_id: Arc::new(RwLock::new(None)),
            storage_path: data_dir.join("ollama_cloud_auth.json"),
            http: Client::new(),
        };
        if let Err(err) = manager.load_from_disk_sync() {
            log::warn!("[OllamaCloud] load store failed: {err}");
        }
        manager
    }

    pub fn validate_api_key_format(key: &str) -> Result<(), OllamaCloudError> {
        let key = key.trim();
        if key.is_empty() {
            return Err(OllamaCloudError::MissingApiKey);
        }
        if key.len() < 16 || key.contains(char::is_whitespace) {
            return Err(OllamaCloudError::InvalidApiKeyFormat);
        }
        Ok(())
    }

    pub async fn import_api_key(
        &self,
        api_key: String,
        label: Option<String>,
    ) -> Result<OllamaCloudManagedAccount, OllamaCloudError> {
        let api_key = api_key.trim().to_string();
        Self::validate_api_key_format(&api_key)?;
        let id = account_id_for_key(&api_key);
        let account = OllamaCloudAccount {
            id: id.clone(),
            api_key,
            label: clean_opt(label),
            created_at: chrono::Utc::now().timestamp(),
        };
        {
            let mut accounts = self.accounts.write().await;
            accounts.insert(id.clone(), account);
        }
        {
            let mut default_id = self.default_account_id.write().await;
            if default_id.is_none() {
                *default_id = Some(id.clone());
            }
        }
        self.save_to_disk().await?;
        self.account_public(&id).await
    }

    pub async fn list_accounts(&self) -> Vec<OllamaCloudManagedAccount> {
        let accounts = self.accounts.read().await;
        let default_id = self.resolve_default_account_id_locked(&accounts).await;
        let mut list: Vec<_> = accounts
            .values()
            .map(|a| public_account(a, default_id.as_deref()))
            .collect();
        list.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(a.id.cmp(&b.id)));
        list
    }

    pub async fn get_status(&self) -> OllamaCloudStatus {
        let accounts = self.list_accounts().await;
        OllamaCloudStatus {
            authenticated: !accounts.is_empty(),
            default_account_id: self.default_account_id().await,
            accounts,
        }
    }

    pub async fn default_account_id(&self) -> Option<String> {
        let accounts = self.accounts.read().await;
        self.resolve_default_account_id_locked(&accounts).await
    }

    pub async fn remove_account(&self, account_id: &str) -> Result<(), OllamaCloudError> {
        {
            let mut accounts = self.accounts.write().await;
            if accounts.remove(account_id).is_none() {
                return Err(OllamaCloudError::AccountNotFound(account_id.to_string()));
            }
            let mut default_id = self.default_account_id.write().await;
            if default_id.as_deref() == Some(account_id) {
                *default_id = fallback_default_account_id(&accounts);
            }
        }
        self.save_to_disk().await
    }

    pub async fn set_default_account(&self, account_id: &str) -> Result<(), OllamaCloudError> {
        {
            let accounts = self.accounts.read().await;
            if !accounts.contains_key(account_id) {
                return Err(OllamaCloudError::AccountNotFound(account_id.to_string()));
            }
        }
        *self.default_account_id.write().await = Some(account_id.to_string());
        self.save_to_disk().await
    }

    pub async fn get_api_key_for_account(
        &self,
        account_id: Option<String>,
    ) -> Result<String, OllamaCloudError> {
        let resolved_id = match account_id {
            Some(id) if !id.trim().is_empty() => id,
            _ => self
                .default_account_id()
                .await
                .ok_or_else(|| OllamaCloudError::AccountNotFound("default".to_string()))?,
        };
        let accounts = self.accounts.read().await;
        accounts
            .get(&resolved_id)
            .map(|account| account.api_key.clone())
            .ok_or(OllamaCloudError::AccountNotFound(resolved_id))
    }

    pub async fn get_api_key_for_provider(
        &self,
        provider: &Provider,
    ) -> Result<String, OllamaCloudError> {
        let account_id = provider
            .meta
            .as_ref()
            .and_then(|meta| meta.managed_account_id_for("ollama_cloud"));
        self.get_api_key_for_account(account_id).await
    }

    pub async fn test_connection(&self, key: &str) -> Result<Vec<OllamaModel>, OllamaCloudError> {
        Self::validate_api_key_format(key)?;
        self.fetch_models_with_key(key).await
    }

    pub async fn list_models(
        &self,
        account_id: Option<String>,
    ) -> Result<Vec<OllamaModel>, OllamaCloudError> {
        let key = self.get_api_key_for_account(account_id).await?;
        self.fetch_models_with_key(&key).await
    }

    pub async fn list_tags(
        &self,
        account_id: Option<String>,
    ) -> Result<Vec<OllamaModel>, OllamaCloudError> {
        let key = self.get_api_key_for_account(account_id).await?;
        self.fetch_tags_with_key(&key).await
    }

    async fn fetch_models_with_key(&self, key: &str) -> Result<Vec<OllamaModel>, OllamaCloudError> {
        let json = self.get_json(MODELS_URL, key).await?;
        Ok(parse_models_response(&json))
    }

    async fn fetch_tags_with_key(&self, key: &str) -> Result<Vec<OllamaModel>, OllamaCloudError> {
        let json = self.get_json(TAGS_URL, key).await?;
        Ok(parse_tags_response(&json))
    }

    async fn get_json(&self, url: &str, key: &str) -> Result<Value, OllamaCloudError> {
        let response = self.http.get(url).bearer_auth(key).send().await?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(OllamaCloudError::Auth(status.as_u16(), text));
        }
        serde_json::from_str(&text).map_err(|err| OllamaCloudError::Parse(err.to_string()))
    }

    async fn account_public(
        &self,
        account_id: &str,
    ) -> Result<OllamaCloudManagedAccount, OllamaCloudError> {
        let accounts = self.accounts.read().await;
        let default_id = self.resolve_default_account_id_locked(&accounts).await;
        accounts
            .get(account_id)
            .map(|a| public_account(a, default_id.as_deref()))
            .ok_or_else(|| OllamaCloudError::AccountNotFound(account_id.to_string()))
    }

    async fn resolve_default_account_id_locked(
        &self,
        accounts: &HashMap<String, OllamaCloudAccount>,
    ) -> Option<String> {
        let current = self.default_account_id.read().await.clone();
        current
            .filter(|id| accounts.contains_key(id))
            .or_else(|| fallback_default_account_id(accounts))
    }

    fn load_from_disk_sync(&self) -> Result<(), OllamaCloudError> {
        if !self.storage_path.exists() {
            return Ok(());
        }
        let content = fs::read_to_string(&self.storage_path)?;
        let store: OllamaCloudStore =
            serde_json::from_str(&content).map_err(|e| OllamaCloudError::Parse(e.to_string()))?;
        if let Ok(mut accounts) = self.accounts.try_write() {
            *accounts = store.accounts;
        }
        if let Ok(mut default_id) = self.default_account_id.try_write() {
            *default_id = store.default_account_id;
        }
        Ok(())
    }

    async fn save_to_disk(&self) -> Result<(), OllamaCloudError> {
        let store = OllamaCloudStore {
            version: 1,
            accounts: self.accounts.read().await.clone(),
            default_account_id: self.default_account_id.read().await.clone(),
        };
        let json = serde_json::to_string_pretty(&store)
            .map_err(|e| OllamaCloudError::Parse(e.to_string()))?;
        if let Some(parent) = self.storage_path.parent() {
            fs::create_dir_all(parent)?;
        }
        write_secret_file(&self.storage_path, json.as_bytes())?;
        Ok(())
    }
}

fn clean_opt(value: Option<String>) -> Option<String> {
    value.and_then(|v| {
        let trimmed = v.trim().to_string();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    })
}

fn account_id_for_key(key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(key.as_bytes());
    let hash = hex::encode(hasher.finalize());
    format!("ollama_cloud_{}", &hash[..24])
}

fn fallback_default_account_id(accounts: &HashMap<String, OllamaCloudAccount>) -> Option<String> {
    accounts
        .values()
        .max_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)))
        .map(|a| a.id.clone())
}

fn public_account(
    account: &OllamaCloudAccount,
    default_id: Option<&str>,
) -> OllamaCloudManagedAccount {
    OllamaCloudManagedAccount {
        id: account.id.clone(),
        label: account.label.clone(),
        created_at: account.created_at,
        is_default: default_id == Some(account.id.as_str()),
        masked_key: mask_key(&account.api_key),
    }
}

fn mask_key(key: &str) -> String {
    if key.chars().count() > 12 {
        let prefix: String = key.chars().take(6).collect();
        let suffix: String = key
            .chars()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("{prefix}...{suffix}")
    } else {
        "***".to_string()
    }
}

#[cfg(unix)]
fn write_secret_file(path: &PathBuf, bytes: &[u8]) -> Result<(), std::io::Error> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let mut file = fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn write_secret_file(path: &PathBuf, bytes: &[u8]) -> Result<(), std::io::Error> {
    fs::write(path, bytes)
}
