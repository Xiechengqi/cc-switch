use crate::database::{Database, ShareRecord};
use crate::services::share::ShareService;
use http::HeaderMap;
use std::sync::Arc;

/// Result of resolving a request as belonging to a share scope.
pub enum ShareGuardResult {
    /// Not a share request (no `X-CC-Switch-Share-Id` header) — proceed with
    /// normal local proxy handling.
    NotShareRequest,
    /// Valid share — contains the share record with API key and config.
    Valid(Box<ShareRecord>),
    /// Invalid/inactive/expired/exhausted — return error to caller.
    Rejected(u16, String),
}

/// Resolve the incoming request to a share scope using the share id injected
/// by cc-switch-router on the tunnel transport. Authentication of the
/// caller (owner / sharedWithEmails / Free) happens at the router edge via
/// the user's `Authorization: Bearer <user_api_token>`; by the time the
/// request reaches us through the SSH tunnel the only thing left to verify
/// is that the share is currently routable (active / not expired / quota OK).
///
/// Direct external callers reaching this endpoint without going through the
/// router are not part of the supported deployment topology (clients run
/// behind no public IP), so we no longer accept caller-supplied API keys
/// here — the router is the sole authority.
pub fn check_share_request(db: &Arc<Database>, headers: &HeaderMap) -> ShareGuardResult {
    let share_id = match share_id_from_headers(headers) {
        Some(id) => id,
        None => return ShareGuardResult::NotShareRequest,
    };

    match ShareService::validate_share_for_invocation(db, share_id) {
        Ok(Some(validation)) => {
            if let Some(share) = validation.share {
                ShareGuardResult::Valid(Box::new(share))
            } else {
                let reason = validation
                    .rejection
                    .map(|reason| format!("{reason:?}"))
                    .unwrap_or_else(|| "Unknown".to_string());
                let message = validation
                    .message
                    .unwrap_or_else(|| "Share is not currently routable".to_string());
                ShareGuardResult::Rejected(403, format!("{message} [{reason}]"))
            }
        }
        Ok(None) => ShareGuardResult::Rejected(403, "Share not found".to_string()),
        Err(e) => ShareGuardResult::Rejected(500, format!("Share validation error: {e}")),
    }
}

pub fn share_user_email_from_headers(headers: &HeaderMap) -> Option<String> {
    headers
        .get("X-CC-Switch-User-Email")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 254
                && value.contains('@')
                && !value.chars().any(char::is_control)
        })
        .map(str::to_string)
}

fn share_id_from_headers(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("X-CC-Switch-Share-Id")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

/// Record one admitted share request as soon as the share scope is resolved.
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

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderValue;

    #[test]
    fn share_id_from_headers_reads_router_injected_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-cc-switch-share-id",
            HeaderValue::from_static("share-abc"),
        );

        assert_eq!(share_id_from_headers(&headers), Some("share-abc"));
    }

    #[test]
    fn share_id_from_headers_ignores_blank_value() {
        let mut headers = HeaderMap::new();
        headers.insert("x-cc-switch-share-id", HeaderValue::from_static("   "));

        assert_eq!(share_id_from_headers(&headers), None);
    }

    #[test]
    fn share_id_from_headers_returns_none_without_header() {
        let headers = HeaderMap::new();
        assert!(share_id_from_headers(&headers).is_none());
    }
}
