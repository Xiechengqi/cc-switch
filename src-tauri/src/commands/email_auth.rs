use crate::email_auth;
use crate::services::share::ShareService;
use crate::store::AppState;
use tauri::State;

#[tauri::command]
pub async fn email_auth_request_code(
    state: State<'_, AppState>,
    email: String,
) -> Result<email_auth::EmailCodeRequestResponse, String> {
    ensure_email_allowed(&state, &email)?;
    email_auth::request_code(&email).await
}

#[tauri::command]
pub async fn email_auth_verify_code(
    state: State<'_, AppState>,
    email: String,
    code: String,
) -> Result<email_auth::EmailAuthStatus, String> {
    ensure_email_allowed(&state, &email)?;
    email_auth::verify_code(&email, &code).await
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
