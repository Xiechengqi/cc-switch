use crate::database::{Database, ShareRecord};
use crate::services::share::ShareService;
use http::HeaderMap;
use std::sync::Arc;

/// Result of checking a share token from an incoming request.
pub enum ShareGuardResult {
    /// Not a share request (no X-Share-Token header) — proceed with normal proxy.
    NotShareRequest,
    /// Valid share — contains the share record with API key and config.
    Valid(ShareRecord),
    /// Invalid/expired/exhausted — return error to caller.
    Rejected(u16, String),
}

/// Check if the incoming request is a share request and validate it.
pub fn check_share_token(db: &Arc<Database>, headers: &HeaderMap) -> ShareGuardResult {
    let token = match headers
        .get("X-API-Key")
        .and_then(|v| v.to_str().ok())
        .or_else(|| headers.get("X-Share-Token").and_then(|v| v.to_str().ok()))
    {
        Some(t) => t,
        None => return ShareGuardResult::NotShareRequest,
    };

    match ShareService::validate_token(db, token) {
        Ok(Some(share)) => ShareGuardResult::Valid(share),
        Ok(None) => ShareGuardResult::Rejected(
            403,
            "Share token invalid, expired, or exhausted".to_string(),
        ),
        Err(e) => ShareGuardResult::Rejected(500, format!("Share validation error: {e}")),
    }
}

/// Record one admitted share request as soon as the token is accepted.
pub fn record_share_access(db: &Arc<Database>, share_id: &str) {
    if let Err(e) = ShareService::record_request(db, share_id) {
        log::error!("[ShareGuard] Failed to record request for share {share_id}: {e}");
    }
}

/// Record token usage once a request finishes and usage has been parsed.
pub fn record_share_request(db: &Arc<Database>, share_id: &str, total_tokens: i64) {
    if total_tokens > 0 {
        if let Err(e) = ShareService::record_tokens(db, share_id, total_tokens) {
            log::error!("[ShareGuard] Failed to record token usage for share {share_id}: {e}");
        }
    }
}
