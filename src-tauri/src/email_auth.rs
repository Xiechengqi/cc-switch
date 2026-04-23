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
    pub verification_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_token_expires_at: Option<i64>,
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
    verification_token: String,
    verification_token_expires_at: i64,
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
#[serde(rename_all = "camelCase")]
struct BindOwnerEmailResponse {
    ok: bool,
    owner_email: String,
    already_bound: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstallationOwnerEmailResponse {
    ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    owner_email: Option<String>,
}

pub fn humanize_remote_owner_binding_error(message: &str) -> String {
    let normalized = message.trim();
    if normalized.is_empty() {
        return "绑定设备邮箱失败，请重新发送并验证邮箱验证码后重试".to_string();
    }
    if normalized.contains("verification token is required")
        || normalized.contains("verification token expired or not found")
        || normalized.contains("redeem verification token failed")
        || normalized.contains("verification token does not match")
    {
        return "当前邮箱验证码登录凭证已过期，请重新发送并验证邮箱验证码后重试".to_string();
    }
    if normalized.contains("this installation is locked to a different owner email") {
        return "当前设备已绑定其他邮箱，不能切换到新的邮箱".to_string();
    }
    if normalized.contains("installation owner email binding is required") {
        return "当前设备尚未完成邮箱绑定，请重新发送并验证邮箱验证码后重试".to_string();
    }
    if normalized.contains("installation not found")
        || normalized.contains("signature verification failed")
    {
        return "当前设备身份已失效，请重新发送并验证邮箱验证码后重试".to_string();
    }
    if let Some(detail) = normalized.strip_prefix("bind installation owner email failed: ") {
        return humanize_remote_owner_binding_error(detail);
    }
    normalized.to_string()
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
        verification_token: Some(body.verification_token),
        verification_token_expires_at: Some(body.verification_token_expires_at),
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

pub async fn session_me() -> Result<EmailSessionMeResponse, String> {
    let Some(state) = read_state()? else {
        return Ok(EmailSessionMeResponse {
            authenticated: false,
            user: None,
            expires_at: None,
            installation_owner_email: None,
        });
    };

    let installation_owner_email = match crate::tunnel::config::current_tunnel_config()
        .or(Some(Default::default()))
    {
        Some(config) => match fetch_remote_owner_binding(&config).await {
            Ok(value) => value,
            Err(err) => {
                log::debug!("[EmailAuth] failed to query remote installation owner email: {err}");
                None
            }
        },
        None => None,
    };

    Ok(EmailSessionMeResponse {
        authenticated: !state.email.trim().is_empty(),
        user: Some(EmailAuthUser {
            id: state.email.clone(),
            email: state.email.clone(),
        }),
        expires_at: None,
        installation_owner_email,
    })
}

async fn fetch_remote_owner_binding(
    config: &crate::tunnel::config::TunnelConfig,
) -> Result<Option<String>, String> {
    fetch_remote_owner_binding_inner(config, true).await
}

async fn fetch_remote_owner_binding_inner(
    config: &crate::tunnel::config::TunnelConfig,
    allow_identity_reset_retry: bool,
) -> Result<Option<String>, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(AUTH_REQUEST_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("create owner email client failed: {e}"))?;
    let identity = crate::tunnel::identity::ensure_identity(&client, config)
        .await
        .map_err(|e| e.to_string())?;
    let timestamp_ms = chrono::Utc::now().timestamp_millis();
    let nonce = uuid::Uuid::new_v4().to_string();
    let signature = crate::tunnel::identity::sign_action_payload(
        &identity,
        &identity.installation_id,
        "get_installation_owner_email",
        &serde_json::json!({}),
        timestamp_ms,
        &nonce,
    )
    .map_err(|e| e.to_string())?;
    let url = format!("{}/v1/installations/owner-email", config.get_server_addr());
    let response = client
        .get(&url)
        .query(&[
            ("installationId", identity.installation_id.as_str()),
            ("timestampMs", &timestamp_ms.to_string()),
            ("nonce", nonce.as_str()),
            ("signature", signature.as_str()),
        ])
        .send()
        .await
        .map_err(|e| format!("query installation owner email failed: {e}"))?;
    if !response.status().is_success() {
        let status = response.status();
        let text = response
            .text()
            .await
            .unwrap_or_else(|_| format!("HTTP {status}"));
        let message = serde_json::from_str::<ErrorResponse>(&text)
            .map(|err| err.message)
            .unwrap_or(text);
        if allow_identity_reset_retry
            && crate::tunnel::identity::should_reset_identity_for_api_error(&message)
        {
            crate::tunnel::identity::reset_identity().map_err(|e| e.to_string())?;
            return Box::pin(fetch_remote_owner_binding_inner(config, false)).await;
        }
        return Err(message);
    }
    let body: InstallationOwnerEmailResponse = response
        .json()
        .await
        .map_err(|e| format!("parse installation owner email response failed: {e}"))?;
    if !body.ok {
        return Err("installation owner email query was not accepted".to_string());
    }
    Ok(body
        .owner_email
        .map(|value| value.trim().to_ascii_lowercase()))
}

pub async fn ensure_remote_owner_binding(
    config: &crate::tunnel::config::TunnelConfig,
    expected_email: &str,
) -> Result<(), String> {
    let Some(state) = read_state()? else {
        return Err("创建 share 前请先完成邮箱验证码登录".to_string());
    };
    let email = state.email.trim().to_ascii_lowercase();
    let expected_email = expected_email.trim().to_ascii_lowercase();
    if email.is_empty() || email != expected_email {
        return Err("当前邮箱登录状态与 share owner 不一致，请重新登录".to_string());
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(AUTH_REQUEST_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("create bind owner client failed: {e}"))?;
    let identity = crate::tunnel::identity::ensure_identity(&client, config)
        .await
        .map_err(|e| e.to_string())?;
    let timestamp_ms = chrono::Utc::now().timestamp_millis();
    let nonce = uuid::Uuid::new_v4().to_string();
    let verification_token = state
        .verification_token
        .as_deref()
        .filter(|value| !value.trim().is_empty());
    let payload = serde_json::json!({
        "email": email,
        "verificationToken": verification_token,
    });
    let signature = crate::tunnel::identity::sign_action_payload(
        &identity,
        &identity.installation_id,
        "bind_installation_owner_email",
        &payload,
        timestamp_ms,
        &nonce,
    )
    .map_err(|e| e.to_string())?;
    let url = format!(
        "{}/v1/installations/bind-owner-email",
        config.get_server_addr()
    );
    let response = client
        .post(&url)
        .json(&serde_json::json!({
            "installationId": identity.installation_id,
            "email": email,
            "verificationToken": verification_token,
            "timestampMs": timestamp_ms,
            "nonce": nonce,
            "signature": signature,
        }))
        .send()
        .await
        .map_err(|e| {
            humanize_remote_owner_binding_error(&format!(
                "bind installation owner email failed: {e}"
            ))
        })?;
    if !response.status().is_success() {
        let status = response.status();
        let text = response
            .text()
            .await
            .unwrap_or_else(|_| format!("HTTP {status}"));
        let message = serde_json::from_str::<ErrorResponse>(&text)
            .map(|err| err.message)
            .unwrap_or(text);
        return Err(humanize_remote_owner_binding_error(&message));
    }
    let body: BindOwnerEmailResponse = response
        .json()
        .await
        .map_err(|e| format!("parse bind owner response failed: {e}"))?;
    if !body.ok || body.owner_email.trim().to_ascii_lowercase() != email {
        return Err("绑定设备邮箱失败，请重新发送并验证邮箱验证码后重试".to_string());
    }

    if !body.already_bound {
        let mut next_state = state;
        next_state.verification_token = None;
        next_state.verification_token_expires_at = None;
        write_state(&next_state)?;
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::humanize_remote_owner_binding_error;

    #[test]
    fn humanize_remote_owner_binding_error_maps_expired_proof() {
        let message = humanize_remote_owner_binding_error(
            "redeem verification token failed: verification token expired or not found",
        );
        assert!(message.contains("当前邮箱验证码登录凭证已过期"));
    }

    #[test]
    fn humanize_remote_owner_binding_error_maps_locked_owner() {
        let message = humanize_remote_owner_binding_error(
            "this installation is locked to a different owner email",
        );
        assert!(message.contains("当前设备已绑定其他邮箱"));
    }
}
