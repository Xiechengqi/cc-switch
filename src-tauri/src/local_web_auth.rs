use std::sync::{Mutex, OnceLock};

use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use axum::http::{header, HeaderMap};
use base64::Engine;
use chrono::{DateTime, Duration, Utc};
use rand::{distributions::Alphanumeric, Rng};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{database::Database, error::AppError};

const PASSWORD_HASH_KEY: &str = "web_admin_password_hash_v1";
const SESSIONS_KEY: &str = "web_admin_sessions_v1";
const ACCESS_TTL_SECS: i64 = 60 * 60;
const REFRESH_TTL_SECS: i64 = 30 * 24 * 60 * 60;
const LOGIN_FAILURE_WINDOW_SECS: i64 = 10 * 60;
const LOGIN_FAILURE_LOCK_SECS: i64 = 10 * 60;
const LOGIN_FAILURE_LIMIT: usize = 8;

static SETUP_TOKEN: OnceLock<String> = OnceLock::new();
static LOGIN_THROTTLE: OnceLock<Mutex<LoginThrottle>> = OnceLock::new();

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthMethods {
    pub router_available: bool,
    pub password_configured: bool,
    pub setup_token_required: bool,
    pub methods: Vec<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PasswordLoginResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: String,
    pub refresh_expires_at: String,
}

#[derive(Debug, Clone)]
pub struct LocalWebPrincipal {
    pub user_email: String,
    pub role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredSession {
    id: String,
    access_token_hash: String,
    refresh_token_hash: String,
    access_expires_at: String,
    refresh_expires_at: String,
    created_at: String,
    last_used_at: String,
    revoked_at: Option<String>,
}

#[derive(Debug, Default)]
struct LoginThrottle {
    failures: Vec<DateTime<Utc>>,
    locked_until: Option<DateTime<Utc>>,
}

pub fn auth_methods(db: &Database) -> Result<AuthMethods, AppError> {
    let router_available = crate::settings::get_settings()
        .client_tunnel
        .as_ref()
        .map(|config| config.enabled && !config.subdomain.trim().is_empty())
        .unwrap_or(false);
    let password_configured = is_password_configured(db)?;
    let mut methods = Vec::new();
    if router_available {
        methods.push("email");
        methods.push("apiToken");
    }
    if password_configured {
        methods.push("password");
    } else {
        methods.push("passwordSetup");
    }
    Ok(AuthMethods {
        router_available,
        password_configured,
        setup_token_required: !password_configured,
        methods,
    })
}

pub fn ensure_startup_setup_token(db: &Database) -> Result<Option<String>, AppError> {
    if is_password_configured(db)? {
        return Ok(None);
    }
    let token = SETUP_TOKEN.get_or_init(|| generate_secret(32)).to_string();
    Ok(Some(token))
}

pub fn is_password_configured(db: &Database) -> Result<bool, AppError> {
    Ok(db
        .get_setting(PASSWORD_HASH_KEY)?
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false))
}

pub fn setup_password(
    db: &Database,
    password: &str,
    setup_token: Option<&str>,
) -> Result<PasswordLoginResponse, AppError> {
    if is_password_configured(db)? {
        return Err(AppError::Message(
            "web password is already configured".into(),
        ));
    }
    let expected = SETUP_TOKEN.get().map(String::as_str);
    if expected.is_some() && setup_token.map(str::trim) != expected {
        return Err(AppError::Message("invalid setup token".into()));
    }
    set_password_hash(db, password)?;
    create_session(db)
}

pub fn login(db: &Database, password: &str) -> Result<PasswordLoginResponse, AppError> {
    let Some(hash) = db.get_setting(PASSWORD_HASH_KEY)? else {
        return Err(AppError::Message("web password is not configured".into()));
    };
    check_password_login_allowed()?;
    if let Err(err) = verify_password(password, &hash) {
        record_password_login_failure();
        return Err(err);
    }
    clear_password_login_failures();
    create_session(db)
}

pub fn refresh(db: &Database, refresh_token: &str) -> Result<PasswordLoginResponse, AppError> {
    let now = Utc::now();
    let refresh_hash = hash_token(refresh_token.trim());
    let mut sessions = read_sessions(db)?;
    let Some(session) = sessions
        .iter_mut()
        .find(|session| session.revoked_at.is_none() && session.refresh_token_hash == refresh_hash)
    else {
        return Err(AppError::Message("refresh session not found".into()));
    };
    if parse_time(&session.refresh_expires_at)? < now {
        return Err(AppError::Message("refresh session expired".into()));
    }
    let access_token = generate_secret(48);
    let refresh_token = generate_secret(64);
    let access_expires_at = now + Duration::seconds(ACCESS_TTL_SECS);
    let refresh_expires_at = now + Duration::seconds(REFRESH_TTL_SECS);
    session.access_token_hash = hash_token(&access_token);
    session.refresh_token_hash = hash_token(&refresh_token);
    session.access_expires_at = access_expires_at.to_rfc3339();
    session.refresh_expires_at = refresh_expires_at.to_rfc3339();
    session.last_used_at = now.to_rfc3339();
    write_sessions(db, &sessions)?;
    Ok(PasswordLoginResponse {
        access_token,
        refresh_token,
        expires_at: access_expires_at.to_rfc3339(),
        refresh_expires_at: refresh_expires_at.to_rfc3339(),
    })
}

pub fn logout(db: &Database, access_token: &str) -> Result<(), AppError> {
    let access_hash = hash_token(access_token.trim());
    let now = Utc::now().to_rfc3339();
    let mut sessions = read_sessions(db)?;
    for session in &mut sessions {
        if session.access_token_hash == access_hash {
            session.revoked_at = Some(now.clone());
        }
    }
    write_sessions(db, &sessions)
}

pub fn change_password(db: &Database, current: &str, next: &str) -> Result<(), AppError> {
    let Some(hash) = db.get_setting(PASSWORD_HASH_KEY)? else {
        return Err(AppError::Message("web password is not configured".into()));
    };
    verify_password(current, &hash)?;
    set_password_hash(db, next)?;
    revoke_all_sessions(db)
}

pub fn authenticate_headers(
    db: &Database,
    headers: &HeaderMap,
) -> Result<Option<LocalWebPrincipal>, AppError> {
    let Some(token) = bearer_token(headers) else {
        return Ok(None);
    };
    authenticate_access_token(db, token)
}

fn authenticate_access_token(
    db: &Database,
    access_token: &str,
) -> Result<Option<LocalWebPrincipal>, AppError> {
    let access_hash = hash_token(access_token.trim());
    let now = Utc::now();
    let mut sessions = read_sessions(db)?;
    let mut matched = false;
    for session in &mut sessions {
        if session.revoked_at.is_some() || session.access_token_hash != access_hash {
            continue;
        }
        if parse_time(&session.access_expires_at)? < now {
            continue;
        }
        session.last_used_at = now.to_rfc3339();
        matched = true;
        break;
    }
    if matched {
        write_sessions(db, &sessions)?;
        return Ok(Some(LocalWebPrincipal {
            user_email: "local-admin@cc-switch.local".to_string(),
            role: "admin".to_string(),
        }));
    }
    Ok(None)
}

fn set_password_hash(db: &Database, password: &str) -> Result<(), AppError> {
    validate_password(password)?;
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| AppError::Message(format!("hash web password failed: {e}")))?
        .to_string();
    db.set_setting(PASSWORD_HASH_KEY, &hash)
}

fn verify_password(password: &str, encoded_hash: &str) -> Result<(), AppError> {
    let parsed = PasswordHash::new(encoded_hash)
        .map_err(|e| AppError::Message(format!("parse web password hash failed: {e}")))?;
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .map_err(|_| AppError::Message("invalid password".into()))
}

fn validate_password(password: &str) -> Result<(), AppError> {
    if password.chars().count() < 8 {
        return Err(AppError::Message(
            "web password must be at least 8 characters".into(),
        ));
    }
    Ok(())
}

fn check_password_login_allowed() -> Result<(), AppError> {
    let now = Utc::now();
    let throttle = LOGIN_THROTTLE.get_or_init(|| Mutex::new(LoginThrottle::default()));
    let mut guard = throttle
        .lock()
        .map_err(|_| AppError::Message("web password throttle lock poisoned".into()))?;
    if let Some(locked_until) = guard.locked_until {
        if locked_until > now {
            return Err(AppError::Message("too many password attempts".into()));
        }
        guard.locked_until = None;
    }
    guard
        .failures
        .retain(|time| *time + Duration::seconds(LOGIN_FAILURE_WINDOW_SECS) >= now);
    Ok(())
}

fn record_password_login_failure() {
    let now = Utc::now();
    let throttle = LOGIN_THROTTLE.get_or_init(|| Mutex::new(LoginThrottle::default()));
    let Ok(mut guard) = throttle.lock() else {
        return;
    };
    guard
        .failures
        .retain(|time| *time + Duration::seconds(LOGIN_FAILURE_WINDOW_SECS) >= now);
    guard.failures.push(now);
    if guard.failures.len() >= LOGIN_FAILURE_LIMIT {
        guard.locked_until = Some(now + Duration::seconds(LOGIN_FAILURE_LOCK_SECS));
        guard.failures.clear();
    }
}

fn clear_password_login_failures() {
    let throttle = LOGIN_THROTTLE.get_or_init(|| Mutex::new(LoginThrottle::default()));
    if let Ok(mut guard) = throttle.lock() {
        guard.failures.clear();
        guard.locked_until = None;
    }
}

fn create_session(db: &Database) -> Result<PasswordLoginResponse, AppError> {
    let now = Utc::now();
    let access_token = generate_secret(48);
    let refresh_token = generate_secret(64);
    let access_expires_at = now + Duration::seconds(ACCESS_TTL_SECS);
    let refresh_expires_at = now + Duration::seconds(REFRESH_TTL_SECS);
    let mut sessions = read_sessions(db)?;
    sessions.push(StoredSession {
        id: Uuid::new_v4().to_string(),
        access_token_hash: hash_token(&access_token),
        refresh_token_hash: hash_token(&refresh_token),
        access_expires_at: access_expires_at.to_rfc3339(),
        refresh_expires_at: refresh_expires_at.to_rfc3339(),
        created_at: now.to_rfc3339(),
        last_used_at: now.to_rfc3339(),
        revoked_at: None,
    });
    prune_sessions(&mut sessions)?;
    write_sessions(db, &sessions)?;
    Ok(PasswordLoginResponse {
        access_token,
        refresh_token,
        expires_at: access_expires_at.to_rfc3339(),
        refresh_expires_at: refresh_expires_at.to_rfc3339(),
    })
}

fn revoke_all_sessions(db: &Database) -> Result<(), AppError> {
    let now = Utc::now().to_rfc3339();
    let mut sessions = read_sessions(db)?;
    for session in &mut sessions {
        session.revoked_at = Some(now.clone());
    }
    write_sessions(db, &sessions)
}

fn read_sessions(db: &Database) -> Result<Vec<StoredSession>, AppError> {
    let Some(raw) = db.get_setting(SESSIONS_KEY)? else {
        return Ok(Vec::new());
    };
    serde_json::from_str(&raw)
        .map_err(|e| AppError::Message(format!("parse web sessions failed: {e}")))
}

fn write_sessions(db: &Database, sessions: &[StoredSession]) -> Result<(), AppError> {
    let raw = serde_json::to_string(sessions)
        .map_err(|e| AppError::Message(format!("serialize web sessions failed: {e}")))?;
    db.set_setting(SESSIONS_KEY, &raw)
}

fn prune_sessions(sessions: &mut Vec<StoredSession>) -> Result<(), AppError> {
    let now = Utc::now();
    sessions.retain(|session| {
        session.revoked_at.is_none()
            && parse_time(&session.refresh_expires_at)
                .map(|expires| expires >= now)
                .unwrap_or(false)
    });
    Ok(())
}

fn parse_time(value: &str) -> Result<DateTime<Utc>, AppError> {
    DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| AppError::Message(format!("parse web auth timestamp failed: {e}")))
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn hash_token(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(hasher.finalize())
}

fn generate_secret(len: usize) -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(len)
        .map(char::from)
        .collect()
}
