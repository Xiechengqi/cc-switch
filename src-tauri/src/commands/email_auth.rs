use crate::email_auth;
use crate::services::share::ShareService;
use crate::store::AppState;
use tauri::State;

#[tauri::command]
pub async fn email_auth_request_code(
    state: State<'_, AppState>,
    router_domain: String,
    email: String,
) -> Result<email_auth::EmailCodeRequestResponse, String> {
    ensure_email_allowed(&state, &email)?;
    let config = tunnel_config_from_domain(&router_domain)?;
    email_auth::request_code(&config, &email).await
}

#[tauri::command]
pub async fn email_auth_verify_code(
    state: State<'_, AppState>,
    router_domain: String,
    email: String,
    code: String,
) -> Result<email_auth::EmailAuthStatus, String> {
    ensure_email_allowed(&state, &email)?;
    let config = tunnel_config_from_domain(&router_domain)?;
    email_auth::verify_code(&config, &email, &code).await
}

#[tauri::command]
pub async fn email_auth_request_owner_change_code(
    state: State<'_, AppState>,
    router_domain: String,
    current_email: String,
    new_email: String,
) -> Result<email_auth::EmailCodeRequestResponse, String> {
    ensure_owner_change_allowed(&state, &current_email, &new_email)?;
    let config = tunnel_config_from_domain(&router_domain)?;
    email_auth::request_code(&config, &new_email).await
}

#[tauri::command]
pub async fn email_auth_change_owner_email(
    state: State<'_, AppState>,
    router_domain: String,
    current_email: String,
    new_email: String,
    code: String,
) -> Result<email_auth::EmailAuthStatus, String> {
    ensure_owner_change_allowed(&state, &current_email, &new_email)?;
    let config = tunnel_config_from_domain(&router_domain)?;
    let status = email_auth::change_owner_email(&config, &current_email, &new_email, &code).await?;
    ShareService::change_owner_email(&state.db, &current_email, &new_email)
        .map_err(|e| e.to_string())?;
    Ok(status)
}

#[tauri::command]
pub fn email_auth_get_status() -> Result<email_auth::EmailAuthStatus, String> {
    email_auth::get_status()
}

#[tauri::command]
pub async fn email_auth_session_me() -> Result<email_auth::EmailSessionMeResponse, String> {
    email_auth::session_me().await
}

#[tauri::command]
pub fn email_auth_logout(state: State<'_, AppState>) -> Result<(), String> {
    if ShareService::list(&state.db)
        .map_err(|e| e.to_string())?
        .into_iter()
        .next()
        .is_some()
    {
        return Err("当前设备已有 share，不能退出邮箱登录".to_string());
    }
    email_auth::clear_state()
}

fn ensure_email_allowed(state: &State<'_, AppState>, email: &str) -> Result<(), String> {
    let normalized = email.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return Err("邮箱不能为空".to_string());
    }

    if let Some(existing_share) = ShareService::list(&state.db)
        .map_err(|e| e.to_string())?
        .into_iter()
        .next()
    {
        if existing_share.owner_email != normalized {
            return Err(format!(
                "当前设备已绑定邮箱 {}，不能切换到 {}",
                existing_share.owner_email, normalized
            ));
        }
    }

    if let Some(current_email) = email_auth::current_email()? {
        if current_email != normalized
            && ShareService::list(&state.db)
                .map_err(|e| e.to_string())?
                .into_iter()
                .next()
                .is_some()
        {
            return Err(format!(
                "当前设备已绑定邮箱 {}，不能切换到 {}",
                current_email, normalized
            ));
        }
    }

    Ok(())
}

fn ensure_owner_change_allowed(
    state: &State<'_, AppState>,
    current_email: &str,
    new_email: &str,
) -> Result<(), String> {
    let current_email = current_email.trim().to_ascii_lowercase();
    let new_email = new_email.trim().to_ascii_lowercase();
    if current_email.is_empty() || new_email.is_empty() {
        return Err("邮箱不能为空".to_string());
    }
    if current_email == new_email {
        return Err("新 owner 邮箱必须不同于当前 owner 邮箱".to_string());
    }
    let status = email_auth::get_status()?;
    if !status.authenticated || status.email.as_deref() != Some(current_email.as_str()) {
        return Err("换绑前请先使用当前 share owner 邮箱登录".to_string());
    }
    if let Some(existing_share) = ShareService::list(&state.db)
        .map_err(|e| e.to_string())?
        .into_iter()
        .next()
    {
        if existing_share.owner_email != current_email {
            return Err(format!(
                "当前设备 share owner 是 {}，不能从 {} 发起换绑",
                existing_share.owner_email, current_email
            ));
        }
    } else {
        return Err("当前设备没有可换绑的 share owner".to_string());
    }
    Ok(())
}

fn tunnel_config_from_domain(domain: &str) -> Result<crate::tunnel::config::TunnelConfig, String> {
    let domain = domain.trim().trim_end_matches('/').to_ascii_lowercase();
    if domain.is_empty() {
        return Err("请先选择分享节点".to_string());
    }
    Ok(crate::tunnel::config::TunnelConfig { domain })
}
