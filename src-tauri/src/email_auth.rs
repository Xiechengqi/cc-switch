use crate::config::get_app_config_dir;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

const AUTH_REQUEST_TIMEOUT_SECS: u64 = 20;
const LOGIN_PURPOSE: &str = "login";
const VERIFICATION_SERVICE_BASE_URL: &str = "https://tokenswitch.org";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailAuthState {
    pub email: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_expires_at: Option<i64>,
    pub verified_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailAuthStatus {
    pub authenticated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailCodeRequestResponse {
    pub ok: bool,
    pub cooldown_secs: i64,
    pub masked_destination: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VerifyEmailCodeResponse {
    ok: bool,
    verified: bool,
    email: String,
    purpose: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailAuthUser {
    id: String,
    email: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailSessionMeResponse {
    pub authenticated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<EmailAuthUser>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installation_owner_email: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ErrorResponse {
    message: String,
}

fn state_path() -> PathBuf {
    get_app_config_dir().join("email_auth.json")
}

fn read_state() -> Result<Option<EmailAuthState>, String> {
    let path = state_path();
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&path).map_err(|e| format!("read email auth failed: {e}"))?;
    let state = serde_json::from_str(&raw).map_err(|e| format!("parse email auth failed: {e}"))?;
    Ok(Some(state))
}

fn write_state(state: &EmailAuthState) -> Result<(), String> {
    let path = state_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create email auth dir failed: {e}"))?;
    }
    let raw = serde_json::to_vec_pretty(state)
        .map_err(|e| format!("serialize email auth failed: {e}"))?;
    atomic_write(&path, &raw)
}

pub fn clear_state() -> Result<(), String> {
    let path = state_path();
    if !path.exists() {
        return Ok(());
    }
    fs::remove_file(path).map_err(|e| format!("remove email auth failed: {e}"))
}

pub fn get_status() -> Result<EmailAuthStatus, String> {
    let Some(state) = read_state()? else {
        return Ok(EmailAuthStatus {
            authenticated: false,
            email: None,
            expires_at: None,
        });
    };
    Ok(EmailAuthStatus {
        authenticated: !state.email.trim().is_empty(),
        email: Some(state.email),
        expires_at: state.expires_at,
    })
}

pub fn current_email() -> Result<Option<String>, String> {
    Ok(read_state()?.map(|state| state.email))
}

pub async fn request_code(email: &str) -> Result<EmailCodeRequestResponse, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(AUTH_REQUEST_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("create email auth client failed: {e}"))?;
    let url = format!("{VERIFICATION_SERVICE_BASE_URL}/v1/verification/email/send");
    let response = client
        .post(&url)
        .json(&serde_json::json!({
            "email": email,
            "purpose": LOGIN_PURPOSE,
        }))
        .send()
        .await
        .map_err(|e| format!("request email code failed: {e}"))?;
    handle_json_response(response).await
}

pub async fn verify_code(email: &str, code: &str) -> Result<EmailAuthStatus, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(AUTH_REQUEST_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("create email auth client failed: {e}"))?;
    let url = format!("{VERIFICATION_SERVICE_BASE_URL}/v1/verification/email/verify");
    let response = client
        .post(&url)
        .json(&serde_json::json!({
            "email": email,
            "purpose": LOGIN_PURPOSE,
            "code": code,
        }))
        .send()
        .await
        .map_err(|e| format!("verify email code failed: {e}"))?;
    let body: VerifyEmailCodeResponse = handle_json_response(response).await?;
    if !body.ok || !body.verified {
        return Err("email verification was not accepted".to_string());
    }
    let state = EmailAuthState {
        email: body.email,
        access_token: None,
        refresh_token: None,
        expires_at: None,
        refresh_expires_at: None,
        verified_at: chrono::Utc::now().timestamp(),
    };
    write_state(&state)?;
    Ok(EmailAuthStatus {
        authenticated: true,
        email: Some(state.email),
        expires_at: None,
    })
}

pub async fn ensure_access_token() -> Result<Option<String>, String> {
    let Some(state) = read_state()? else {
        return Ok(None);
    };
    let now = chrono::Utc::now().timestamp();
    match (state.access_token, state.expires_at) {
        (Some(token), Some(expires_at)) if expires_at > now + 30 => Ok(Some(token)),
        _ => Ok(None),
    }
}

pub async fn session_me() -> Result<EmailSessionMeResponse, String> {
    let Some(state) = read_state()? else {
        return Ok(EmailSessionMeResponse {
            authenticated: false,
            user: None,
            expires_at: None,
            installation_owner_email: None,
        });
    };

    Ok(EmailSessionMeResponse {
        authenticated: !state.email.trim().is_empty(),
        user: Some(EmailAuthUser {
            id: state.email.clone(),
            email: state.email.clone(),
        }),
        expires_at: None,
        installation_owner_email: Some(state.email),
    })
}

async fn handle_json_response<T: for<'de> Deserialize<'de>>(
    response: reqwest::Response,
) -> Result<T, String> {
    if response.status().is_success() {
        return response
            .json::<T>()
            .await
            .map_err(|e| format!("parse email auth response failed: {e}"));
    }
    let status = response.status();
    let text = response
        .text()
        .await
        .unwrap_or_else(|_| format!("HTTP {status}"));
    serde_json::from_str::<ErrorResponse>(&text)
        .map(|err| err.message)
        .map_err(|_| text)
        .and_then(Err)
}

fn atomic_write(path: &Path, data: &[u8]) -> Result<(), String> {
    let tmp_path = path.with_extension("tmp");
    let mut file = create_file(&tmp_path)?;
    file.write_all(data)
        .and_then(|_| file.flush())
        .map_err(|e| format!("write email auth state failed: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("chmod email auth state failed: {e}"))?;
    }
    fs::rename(&tmp_path, path).map_err(|e| format!("replace email auth state failed: {e}"))
}

fn create_file(path: &Path) -> Result<fs::File, String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| format!("open email auth state file failed: {e}"))
    }
    #[cfg(not(unix))]
    {
        fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path)
            .map_err(|e| format!("open email auth state file failed: {e}"))
    }
}
