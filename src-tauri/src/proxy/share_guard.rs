use crate::database::{Database, ShareRecord};
use crate::services::share::ShareService;
use http::HeaderMap;
use std::sync::Arc;

/// Result of checking a share token from an incoming request.
pub enum ShareGuardResult {
    /// Not a share request (no X-Share-Token header) — proceed with normal proxy.
    NotShareRequest,
    /// Valid share — contains the share record with API key and config.
    Valid(Box<ShareRecord>),
    /// Invalid/expired/exhausted — return error to caller.
    Rejected(u16, String),
}

/// Check if the incoming request is a share request and validate it.
pub fn check_share_token(db: &Arc<Database>, headers: &HeaderMap) -> ShareGuardResult {
    let token = match share_token_from_headers(headers) {
        Some(t) => t,
        None => return ShareGuardResult::NotShareRequest,
    };

    match ShareService::validate_token_with_reason(db, token) {
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
                    .unwrap_or_else(|| "Share token invalid, expired, or exhausted".to_string());
                ShareGuardResult::Rejected(403, format!("{message} [{reason}]"))
            }
        }
        Ok(None) => ShareGuardResult::Rejected(
            403,
            "Share token invalid, expired, or exhausted".to_string(),
        ),
        Err(e) => ShareGuardResult::Rejected(500, format!("Share validation error: {e}")),
    }
}

fn share_token_from_headers(headers: &HeaderMap) -> Option<&str> {
    let explicit_share_token = headers
        .get("X-API-Key")
        .and_then(|v| v.to_str().ok())
        .or_else(|| headers.get("X-Share-Token").and_then(|v| v.to_str().ok()));
    if explicit_share_token.is_some() {
        return explicit_share_token;
    }

    // Gemini direct proxy traffic uses x-goog-api-key as the normal client API
    // key placeholder. Treat it as a share token only on public share/router
    // hosts; direct localhost/IP proxy traffic must match Claude/Codex and not
    // require a share API key.
    if !is_share_router_host(headers) {
        return None;
    }

    headers.get("X-Goog-Api-Key").and_then(|v| v.to_str().ok())
}

fn is_share_router_host(headers: &HeaderMap) -> bool {
    headers
        .get(http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(crate::tunnel::config::is_share_tunnel_url)
        .unwrap_or(false)
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

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderValue;

    #[test]
    fn share_token_from_headers_accepts_gemini_api_key_header() {
        let mut headers = HeaderMap::new();
        headers.insert("host", HeaderValue::from_static("alpha.jptokenswitch.cc"));
        headers.insert("x-goog-api-key", HeaderValue::from_static("share-token"));

        assert_eq!(share_token_from_headers(&headers), Some("share-token"));
    }

    #[test]
    fn share_token_from_headers_ignores_gemini_api_key_on_direct_proxy_host() {
        let mut headers = HeaderMap::new();
        headers.insert("host", HeaderValue::from_static("192.168.1.14:3000"));
        headers.insert("x-goog-api-key", HeaderValue::from_static("dummy-key"));

        assert_eq!(share_token_from_headers(&headers), None);
    }

    #[test]
    fn share_token_from_headers_prefers_explicit_share_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("host", HeaderValue::from_static("192.168.1.14:3000"));
        headers.insert("x-goog-api-key", HeaderValue::from_static("gemini-token"));
        headers.insert("x-share-token", HeaderValue::from_static("share-token"));
        headers.insert("x-api-key", HeaderValue::from_static("api-token"));

        assert_eq!(share_token_from_headers(&headers), Some("api-token"));
    }
}
