use crate::config::get_app_config_dir;
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub router_domain: Option<String>,
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
    #[serde(default)]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub router_domain: Option<String>,
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
struct RouterVerifyEmailCodeResponse {
    user: EmailAuthUser,
    access_token: String,
    refresh_token: String,
    expires_at: String,
    refresh_expires_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RouterRefreshSessionResponse {
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
#[serde(rename_all = "camelCase")]
struct BindOwnerEmailResponse {
    ok: bool,
    owner_email: String,
    already_bound: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChangeOwnerEmailResponse {
    ok: bool,
    old_email: String,
    new_email: String,
    updated_shares: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChangeOwnerEmailSignaturePayload<'a> {
    old_email: &'a str,
    new_email: &'a str,
}

#[derive(Debug)]
struct BindOwnerEmailError {
    status: reqwest::StatusCode,
    message: String,
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
            router_domain: None,
        });
    };
    Ok(EmailAuthStatus {
        authenticated: !state.email.trim().is_empty(),
        email: Some(state.email),
        expires_at: state.expires_at,
        router_domain: state.router_domain,
    })
}

pub fn current_email() -> Result<Option<String>, String> {
    Ok(read_state()?.map(|state| state.email))
}

pub async fn request_code(
    config: &crate::tunnel::config::TunnelConfig,
    email: &str,
) -> Result<EmailCodeRequestResponse, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(AUTH_REQUEST_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("create email auth client failed: {e}"))?;
    let identity = crate::tunnel::identity::ensure_identity(&client, config)
        .await
        .map_err(|e| e.to_string())?;
    let timestamp_ms = chrono::Utc::now().timestamp_millis();
    let nonce = uuid::Uuid::new_v4().to_string();
    let payload = serde_json::json!({ "email": email, "purpose": LOGIN_PURPOSE });
    let signature = crate::tunnel::identity::sign_action_payload(
        &identity,
        &identity.installation_id,
        "auth_request_code",
        &payload,
        timestamp_ms,
        &nonce,
    )
    .map_err(|e| e.to_string())?;
    let url = format!(
        "{}/v1/auth/email/request-code",
        config.get_server_addr().trim_end_matches('/')
    );
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

pub async fn verify_code(
    config: &crate::tunnel::config::TunnelConfig,
    email: &str,
    code: &str,
) -> Result<EmailAuthStatus, String> {
    let body = verify_code_with_router(config, email, code).await?;
    let email = body.user.email.trim().to_ascii_lowercase();
    let state = state_from_router_session(config, email.clone(), body)?;
    write_state(&state)?;
    ensure_remote_owner_binding(config, &email).await?;
    Ok(EmailAuthStatus {
        authenticated: true,
        email: Some(email),
        expires_at: state.expires_at,
        router_domain: state.router_domain,
    })
}

pub async fn change_owner_email(
    config: &crate::tunnel::config::TunnelConfig,
    old_email: &str,
    new_email: &str,
    code: &str,
) -> Result<EmailAuthStatus, String> {
    let old_email = old_email.trim().to_ascii_lowercase();
    let new_email = new_email.trim().to_ascii_lowercase();
    if old_email.is_empty() || new_email.is_empty() {
        return Err("邮箱不能为空".to_string());
    }
    if old_email == new_email {
        return Err("新 owner 邮箱必须不同于当前 owner 邮箱".to_string());
    }
    let Some(state) = read_state()? else {
        return Err("换绑前请先完成当前 share owner 邮箱登录".to_string());
    };
    if state.email.trim().to_ascii_lowercase() != old_email {
        return Err("当前邮箱登录状态与 share owner 不一致，请重新登录".to_string());
    }
    if let Some(router_domain) = state.router_domain.as_deref() {
        if router_domain != config.domain {
            return Err("当前邮箱登录所属分享节点与所选分享节点不一致，请重新登录".to_string());
        }
    }
    let remote_owner = fetch_remote_owner_binding(config).await?;
    if remote_owner.as_deref() != Some(old_email.as_str()) {
        return Err("当前分享节点绑定的 owner 与本地状态不一致，请刷新后重试".to_string());
    }

    let body = verify_code_with_router(config, &new_email, code)
        .await
        .map_err(|err| humanize_email_code_error(&err))?;
    let verified_email = body.user.email.trim().to_ascii_lowercase();
    if verified_email != new_email {
        return Err("验证码验证邮箱与新 owner 邮箱不一致".to_string());
    }
    let access_token = body.access_token.clone();
    change_remote_owner_email(config, &old_email, &new_email, &access_token).await?;

    let next_state = state_from_router_session(config, new_email.clone(), body)?;
    write_state(&next_state)?;
    Ok(EmailAuthStatus {
        authenticated: true,
        email: Some(new_email),
        expires_at: next_state.expires_at,
        router_domain: next_state.router_domain,
    })
}

fn humanize_email_code_error(message: &str) -> String {
    let normalized = message.trim().to_ascii_lowercase();
    if normalized.contains("verification code expired or not found") {
        return "验证码已过期或不存在，请重新发送验证码".to_string();
    }
    if normalized.contains("invalid verification code") {
        return "验证码不正确，请检查后重试".to_string();
    }
    message.to_string()
}

async fn verify_code_with_router(
    config: &crate::tunnel::config::TunnelConfig,
    email: &str,
    code: &str,
) -> Result<RouterVerifyEmailCodeResponse, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(AUTH_REQUEST_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("create email auth client failed: {e}"))?;
    let identity = crate::tunnel::identity::ensure_identity(&client, config)
        .await
        .map_err(|e| e.to_string())?;
    let url = format!(
        "{}/v1/auth/email/verify-code",
        config.get_server_addr().trim_end_matches('/')
    );
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
    handle_json_response(response).await
}

fn state_from_router_session(
    config: &crate::tunnel::config::TunnelConfig,
    email: String,
    body: RouterVerifyEmailCodeResponse,
) -> Result<EmailAuthState, String> {
    Ok(EmailAuthState {
        email,
        router_domain: Some(config.domain.clone()),
        verification_token: None,
        verification_token_expires_at: None,
        access_token: Some(body.access_token),
        refresh_token: Some(body.refresh_token),
        expires_at: Some(parse_rfc3339_timestamp(&body.expires_at)?),
        refresh_expires_at: Some(parse_rfc3339_timestamp(&body.refresh_expires_at)?),
        verified_at: chrono::Utc::now().timestamp(),
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
            crate::tunnel::identity::refresh_installation_registration(&client, config)
                .await
                .map_err(|e| e.to_string())?;
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
    let Some(mut state) = read_state()? else {
        return Err("创建 share 前请先完成邮箱验证码登录".to_string());
    };
    let email = state.email.trim().to_ascii_lowercase();
    let expected_email = expected_email.trim().to_ascii_lowercase();
    if email.is_empty() || email != expected_email {
        return Err("当前邮箱登录状态与 share owner 不一致，请重新登录".to_string());
    }
    if let Some(router_domain) = state.router_domain.as_deref() {
        if router_domain != config.domain {
            return Err("当前邮箱登录所属分享节点与所选分享节点不一致，请重新登录".to_string());
        }
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(AUTH_REQUEST_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("create bind owner client failed: {e}"))?;
    let mut identity = crate::tunnel::identity::ensure_identity(&client, config)
        .await
        .map_err(|e| e.to_string())?;
    let verification_token = state
        .verification_token
        .as_ref()
        .filter(|value| !value.trim().is_empty())
        .cloned();

    let body = if let Some(verification_token) = verification_token.as_deref() {
        match send_bind_owner_request(
            &client,
            config,
            &identity,
            &email,
            Some(verification_token),
            None,
        )
        .await
        {
            Ok(body) => body,
            Err(err) => {
                if crate::tunnel::identity::should_reset_identity_for_api_error(&err.message) {
                    identity =
                        crate::tunnel::identity::refresh_installation_registration(&client, config)
                            .await
                            .map_err(|e| e.to_string())?;
                    match send_bind_owner_request(
                        &client,
                        config,
                        &identity,
                        &email,
                        Some(verification_token),
                        None,
                    )
                    .await
                    {
                        Ok(body) => body,
                        Err(retry_err) if retry_err.status == reqwest::StatusCode::UNAUTHORIZED => {
                            let access_token =
                                match valid_access_token(config, &mut state, &client).await {
                                    Ok(token) => token,
                                    Err(_) if remote_owner_matches(config, &email).await? => {
                                        return Ok(());
                                    }
                                    Err(err) => return Err(err),
                                };
                            send_bind_owner_request(
                                &client,
                                config,
                                &identity,
                                &email,
                                None,
                                Some(&access_token),
                            )
                            .await
                            .map_err(|err| humanize_remote_owner_binding_error(&err.message))?
                        }
                        Err(retry_err) => {
                            return Err(humanize_remote_owner_binding_error(&retry_err.message));
                        }
                    }
                } else if err.status == reqwest::StatusCode::UNAUTHORIZED {
                    let access_token = match valid_access_token(config, &mut state, &client).await {
                        Ok(token) => token,
                        Err(_) if remote_owner_matches(config, &email).await? => {
                            return Ok(());
                        }
                        Err(err) => return Err(err),
                    };
                    send_bind_owner_request(
                        &client,
                        config,
                        &identity,
                        &email,
                        None,
                        Some(&access_token),
                    )
                    .await
                    .map_err(|err| humanize_remote_owner_binding_error(&err.message))?
                } else {
                    return Err(humanize_remote_owner_binding_error(&err.message));
                }
            }
        }
    } else {
        match send_bind_owner_request(&client, config, &identity, &email, None, None).await {
            Ok(body) => body,
            Err(err) if err.status == reqwest::StatusCode::UNAUTHORIZED => {
                let access_token = match valid_access_token(config, &mut state, &client).await {
                    Ok(token) => token,
                    Err(_) if remote_owner_matches(config, &email).await? => {
                        return Ok(());
                    }
                    Err(err) => return Err(err),
                };
                if crate::tunnel::identity::should_reset_identity_for_api_error(&err.message) {
                    identity =
                        crate::tunnel::identity::refresh_installation_registration(&client, config)
                            .await
                            .map_err(|e| e.to_string())?;
                }
                send_bind_owner_request(
                    &client,
                    config,
                    &identity,
                    &email,
                    None,
                    Some(&access_token),
                )
                .await
                .map_err(|err| humanize_remote_owner_binding_error(&err.message))?
            }
            Err(err) => return Err(humanize_remote_owner_binding_error(&err.message)),
        }
    };
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

async fn remote_owner_matches(
    config: &crate::tunnel::config::TunnelConfig,
    email: &str,
) -> Result<bool, String> {
    Ok(fetch_remote_owner_binding(config).await?.as_deref() == Some(email))
}

async fn send_bind_owner_request(
    client: &reqwest::Client,
    config: &crate::tunnel::config::TunnelConfig,
    identity: &crate::tunnel::identity::TunnelIdentity,
    email: &str,
    verification_token: Option<&str>,
    bearer_token: Option<&str>,
) -> Result<BindOwnerEmailResponse, BindOwnerEmailError> {
    let timestamp_ms = chrono::Utc::now().timestamp_millis();
    let nonce = uuid::Uuid::new_v4().to_string();
    let mut payload = serde_json::json!({ "email": email });
    if let Some(token) = verification_token {
        if let Some(object) = payload.as_object_mut() {
            object.insert(
                "verificationToken".to_string(),
                serde_json::Value::String(token.to_string()),
            );
        }
    }
    let signature = crate::tunnel::identity::sign_action_payload(
        identity,
        &identity.installation_id,
        "bind_installation_owner_email",
        &payload,
        timestamp_ms,
        &nonce,
    )
    .map_err(|e| BindOwnerEmailError {
        status: reqwest::StatusCode::INTERNAL_SERVER_ERROR,
        message: e.to_string(),
    })?;
    let url = format!(
        "{}/v1/installations/bind-owner-email",
        config.get_server_addr()
    );
    let mut request = client.post(&url).json(&serde_json::json!({
            "installationId": identity.installation_id,
            "email": email,
            "verificationToken": verification_token,
            "timestampMs": timestamp_ms,
            "nonce": nonce,
            "signature": signature,
    }));
    if let Some(token) = bearer_token {
        request = request.bearer_auth(token);
    }
    let response = request.send().await.map_err(|e| BindOwnerEmailError {
        status: reqwest::StatusCode::INTERNAL_SERVER_ERROR,
        message: format!("bind installation owner email failed: {e}"),
    })?;
    if response.status().is_success() {
        return response.json().await.map_err(|e| BindOwnerEmailError {
            status: reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            message: format!("parse bind owner response failed: {e}"),
        });
    }

    let status = response.status();
    let text = response
        .text()
        .await
        .unwrap_or_else(|_| format!("HTTP {status}"));
    let message = serde_json::from_str::<ErrorResponse>(&text)
        .map(|err| err.message)
        .unwrap_or(text);
    Err(BindOwnerEmailError { status, message })
}

async fn change_remote_owner_email(
    config: &crate::tunnel::config::TunnelConfig,
    old_email: &str,
    new_email: &str,
    access_token: &str,
) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(AUTH_REQUEST_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("create change owner client failed: {e}"))?;
    let identity = crate::tunnel::identity::ensure_identity(&client, config)
        .await
        .map_err(|e| e.to_string())?;
    let timestamp_ms = chrono::Utc::now().timestamp_millis();
    let nonce = uuid::Uuid::new_v4().to_string();
    let payload = ChangeOwnerEmailSignaturePayload {
        old_email,
        new_email,
    };
    let signature = crate::tunnel::identity::sign_action_payload(
        &identity,
        &identity.installation_id,
        "change_installation_owner_email",
        &payload,
        timestamp_ms,
        &nonce,
    )
    .map_err(|e| e.to_string())?;
    let url = format!(
        "{}/v1/installations/change-owner-email",
        config.get_server_addr()
    );
    let response = client
        .post(&url)
        .bearer_auth(access_token)
        .json(&serde_json::json!({
            "installationId": identity.installation_id,
            "oldEmail": old_email,
            "newEmail": new_email,
            "timestampMs": timestamp_ms,
            "nonce": nonce,
            "signature": signature,
        }))
        .send()
        .await
        .map_err(|e| format!("change owner email failed: {e}"))?;
    let body: ChangeOwnerEmailResponse = handle_json_response(response).await?;
    if !body.ok
        || body.old_email.trim().to_ascii_lowercase() != old_email
        || body.new_email.trim().to_ascii_lowercase() != new_email
    {
        return Err("换绑 share owner 邮箱失败，请重新验证新邮箱后重试".to_string());
    }
    let _ = body.updated_shares;
    Ok(())
}

async fn valid_access_token(
    config: &crate::tunnel::config::TunnelConfig,
    state: &mut EmailAuthState,
    client: &reqwest::Client,
) -> Result<String, String> {
    let now = chrono::Utc::now().timestamp();
    if let (Some(token), Some(expires_at)) = (state.access_token.as_deref(), state.expires_at) {
        if !token.trim().is_empty() && expires_at > now + 60 {
            return Ok(token.to_string());
        }
    }

    let refresh_token = state
        .refresh_token
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "当前邮箱登录凭证已过期，请重新登录".to_string())?;
    if let Some(refresh_expires_at) = state.refresh_expires_at {
        if refresh_expires_at <= now {
            return Err("当前邮箱登录凭证已过期，请重新登录".to_string());
        }
    }

    let identity = crate::tunnel::identity::ensure_identity(client, config)
        .await
        .map_err(|e| e.to_string())?;
    let url = format!(
        "{}/v1/auth/session/refresh",
        config.get_server_addr().trim_end_matches('/')
    );
    let response = client
        .post(&url)
        .json(&serde_json::json!({
            "refreshToken": refresh_token,
            "installationId": identity.installation_id,
        }))
        .send()
        .await
        .map_err(|e| format!("refresh email auth session failed: {e}"))?;
    let body: RouterRefreshSessionResponse = handle_json_response(response).await?;
    state.email = body.user.email.trim().to_ascii_lowercase();
    state.router_domain = Some(config.domain.clone());
    state.access_token = Some(body.access_token.clone());
    state.refresh_token = Some(body.refresh_token);
    state.expires_at = Some(parse_rfc3339_timestamp(&body.expires_at)?);
    state.refresh_expires_at = Some(parse_rfc3339_timestamp(&body.refresh_expires_at)?);
    write_state(state)?;
    Ok(body.access_token)
}

fn parse_rfc3339_timestamp(value: &str) -> Result<i64, String> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.timestamp())
        .map_err(|e| format!("parse auth timestamp failed: {e}"))
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
    use super::{humanize_remote_owner_binding_error, EmailAuthState};

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

    #[test]
    fn deserialize_legacy_email_auth_state_without_verified_at() {
        let raw = r#"{
  "email": "owner@example.com",
  "accessToken": null,
  "refreshToken": null,
  "expiresAt": null,
  "refreshExpiresAt": null
}"#;
        let state: EmailAuthState = serde_json::from_str(raw).expect("deserialize legacy state");
        assert_eq!(state.email, "owner@example.com");
        assert_eq!(state.verified_at, 0);
    }
}
