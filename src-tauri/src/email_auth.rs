use crate::config::get_app_config_dir;
use crate::tunnel::config::{current_tunnel_config, TunnelConfig};
use crate::tunnel::identity;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

const AUTH_REQUEST_TIMEOUT_SECS: u64 = 20;
const LOGIN_PURPOSE: &str = "login";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailAuthState {
    pub email: String,
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: i64,
    pub refresh_expires_at: i64,
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
    user: EmailAuthUser,
    access_token: String,
    refresh_token: String,
    expires_at: String,
    refresh_expires_at: String,
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
    let raw = serde_json::to_vec_pretty(state).map_err(|e| format!("serialize email auth failed: {e}"))?;
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
    let now = chrono::Utc::now().timestamp();
    Ok(EmailAuthStatus {
        authenticated: state.refresh_expires_at > now,
        email: Some(state.email),
        expires_at: Some(state.expires_at),
    })
}

pub fn current_email() -> Result<Option<String>, String> {
    Ok(read_state()?.map(|state| state.email))
}

pub async fn request_code(email: &str) -> Result<EmailCodeRequestResponse, String> {
    let config = current_config();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(AUTH_REQUEST_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("create email auth client failed: {e}"))?;
    let identity = identity::ensure_identity(&client, &config)
        .await
        .map_err(|e| e.to_string())?;
    let timestamp_ms = chrono::Utc::now().timestamp_millis();
    let nonce = uuid::Uuid::new_v4().to_string();
    let signature = identity::sign_action_payload(
        &identity,
        &identity.installation_id,
        "auth_request_code",
        &serde_json::json!({ "email": email.trim().to_ascii_lowercase(), "purpose": LOGIN_PURPOSE }),
        timestamp_ms,
        &nonce,
    )
    .map_err(|e| e.to_string())?;

    let url = format!("{}/v1/auth/email/request-code", config.get_server_addr());
    let response = client
        .post(&url)
        .json(&serde_json::json!({
            "email": email,
            "installationId": identity.installation_id,
            "timestampMs": timestamp_ms,
            "nonce": nonce,
            "signature": signature,
        }))
        .send()
        .await
        .map_err(|e| format!("request email code failed: {e}"))?;
    handle_json_response(response).await
}

pub async fn verify_code(email: &str, code: &str) -> Result<EmailAuthStatus, String> {
    let config = current_config();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(AUTH_REQUEST_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("create email auth client failed: {e}"))?;
    let identity = identity::ensure_identity(&client, &config)
        .await
        .map_err(|e| e.to_string())?;
    let url = format!("{}/v1/auth/email/verify-code", config.get_server_addr());
    let response = client
        .post(&url)
        .json(&serde_json::json!({
            "email": email,
            "code": code,
            "installationId": identity.installation_id,
        }))
        .send()
        .await
        .map_err(|e| format!("verify email code failed: {e}"))?;
    let body: VerifyEmailCodeResponse = handle_json_response(response).await?;
    let state = EmailAuthState {
        email: body.user.email,
        access_token: body.access_token,
        refresh_token: body.refresh_token,
        expires_at: parse_timestamp(&body.expires_at)?,
        refresh_expires_at: parse_timestamp(&body.refresh_expires_at)?,
    };
    write_state(&state)?;
    Ok(EmailAuthStatus {
        authenticated: true,
        email: Some(state.email),
        expires_at: Some(state.expires_at),
    })
}

pub async fn ensure_access_token() -> Result<Option<String>, String> {
    let Some(state) = read_state()? else {
        return Ok(None);
    };
    let now = chrono::Utc::now().timestamp();
    if state.expires_at > now + 30 {
        return Ok(Some(state.access_token));
    }
    if state.refresh_expires_at <= now {
        clear_state()?;
        return Ok(None);
    }

    let config = current_config();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(AUTH_REQUEST_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("create email auth client failed: {e}"))?;
    let identity = identity::ensure_identity(&client, &config)
        .await
        .map_err(|e| e.to_string())?;
    let url = format!("{}/v1/auth/session/refresh", config.get_server_addr());
    let response = client
        .post(&url)
        .json(&serde_json::json!({
            "refreshToken": state.refresh_token,
            "installationId": identity.installation_id,
        }))
        .send()
        .await
        .map_err(|e| format!("refresh email auth failed: {e}"))?;
    let body: VerifyEmailCodeResponse = handle_json_response(response).await?;
    let refreshed = EmailAuthState {
        email: body.user.email,
        access_token: body.access_token,
        refresh_token: body.refresh_token,
        expires_at: parse_timestamp(&body.expires_at)?,
        refresh_expires_at: parse_timestamp(&body.refresh_expires_at)?,
    };
    write_state(&refreshed)?;
    Ok(Some(refreshed.access_token))
}

pub async fn session_me() -> Result<EmailSessionMeResponse, String> {
    let config = current_config();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(AUTH_REQUEST_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("create email auth client failed: {e}"))?;
    let identity = identity::ensure_identity(&client, &config)
        .await
        .map_err(|e| e.to_string())?;
    let mut request = client
        .get(format!(
            "{}/v1/auth/session/me?installationId={}",
            config.get_server_addr(),
            identity.installation_id
        ));
    if let Some(token) = ensure_access_token().await? {
        request = request.bearer_auth(token);
    }
    let response = request
        .send()
        .await
        .map_err(|e| format!("query email auth status failed: {e}"))?;
    handle_json_response(response).await
}

fn current_config() -> TunnelConfig {
    current_tunnel_config().unwrap_or_default()
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

fn parse_timestamp(value: &str) -> Result<i64, String> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|value| value.timestamp())
        .map_err(|e| format!("parse email auth timestamp failed: {e}"))
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
        return fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| format!("open email auth state file failed: {e}"));
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
