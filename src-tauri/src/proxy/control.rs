//! Control-plane endpoint: the cc-switch-router server calls this over the
//! reverse tunnel to apply share-settings changes synchronously.
//!
//! The client stays authoritative: it applies the patch to its own local
//! config (via [`crate::tunnel::sync::apply_share_settings_patch`]) and reports
//! back the resulting [`ShareTunnelMetadata`] descriptor. The server only
//! persists what we return, after verifying it satisfies the patch.
//!
//! Auth is an HMAC-SHA256 over `METHOD\nPATH\n<body>\n<timestamp_ms>\n<nonce>`
//! using the per-installation `control_secret` issued at registration. Requests
//! arriving here are always from the tunnel (public `/_ctl/*` traffic is
//! rejected at the router edge), but we still authenticate every call so a
//! compromised co-tenant on the tunnel cannot drive our config.

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use hmac::{Hmac, Mac};
use once_cell::sync::Lazy;
use serde::Deserialize;
use serde_json::json;
use sha2::Sha256;
use std::collections::HashMap;
use std::sync::Mutex;

use super::server::ProxyState;
use crate::tunnel::sync::{apply_share_settings_patch, share_metadata_from_record, ShareSettingsPatch};

type HmacSha256 = Hmac<Sha256>;

const CTL_PATH: &str = "/_ctl/apply_share_settings";
/// Reject requests whose timestamp is outside this window (replay / clock skew).
const MAX_SKEW_MS: i64 = 5 * 60 * 1000;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApplyShareSettingsBody {
    share_id: String,
    patch: ShareSettingsPatch,
}

/// Remembers recently-seen nonces so a captured request cannot be replayed
/// inside the skew window. Entries are pruned by timestamp on each insert, so
/// the map stays bounded by the request rate over `MAX_SKEW_MS`.
static SEEN_NONCES: Lazy<Mutex<HashMap<String, i64>>> = Lazy::new(|| Mutex::new(HashMap::new()));

fn err(status: StatusCode, code: &str) -> Response {
    (status, Json(json!({ "ok": false, "error": code }))).into_response()
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

fn expected_signature(secret: &str, body: &[u8], timestamp_ms: &str, nonce: &str) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .expect("HMAC accepts keys of any size");
    mac.update(b"POST\n");
    mac.update(CTL_PATH.as_bytes());
    mac.update(b"\n");
    mac.update(body);
    mac.update(b"\n");
    mac.update(timestamp_ms.as_bytes());
    mac.update(b"\n");
    mac.update(nonce.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

/// Records a nonce as used, pruning stale entries. Returns false if the nonce
/// was already seen within the window (replay).
fn register_nonce(nonce: &str, now_ms: i64) -> bool {
    let mut seen = match SEEN_NONCES.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    seen.retain(|_, ts| (now_ms - *ts).abs() <= MAX_SKEW_MS);
    if seen.contains_key(nonce) {
        return false;
    }
    seen.insert(nonce.to_string(), now_ms);
    true
}

pub async fn apply_share_settings(
    State(state): State<ProxyState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(secret) = crate::tunnel::identity::load_control_secret() else {
        return err(StatusCode::UNAUTHORIZED, "control_secret_unavailable");
    };
    let (Some(timestamp_raw), Some(nonce), Some(signature_b64)) = (
        header(&headers, "x-ctl-timestamp-ms"),
        header(&headers, "x-ctl-nonce"),
        header(&headers, "x-ctl-signature"),
    ) else {
        return err(StatusCode::UNAUTHORIZED, "missing_control_headers");
    };
    let Ok(timestamp_ms) = timestamp_raw.parse::<i64>() else {
        return err(StatusCode::UNAUTHORIZED, "bad_timestamp");
    };
    let now_ms = chrono::Utc::now().timestamp_millis();
    if (now_ms - timestamp_ms).abs() > MAX_SKEW_MS {
        return err(StatusCode::UNAUTHORIZED, "stale_timestamp");
    }

    let Ok(provided_sig) = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        signature_b64,
    ) else {
        return err(StatusCode::UNAUTHORIZED, "bad_signature");
    };
    let expected = expected_signature(&secret, &body, timestamp_raw, nonce);
    // ct_eq via constant-time comparison: hmac's MacResult is not exposed here,
    // so compare the raw bytes with a length-checked constant-time fold.
    if provided_sig.len() != expected.len()
        || provided_sig
            .iter()
            .zip(expected.iter())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            != 0
    {
        return err(StatusCode::UNAUTHORIZED, "bad_signature");
    }
    if !register_nonce(nonce, now_ms) {
        return err(StatusCode::UNAUTHORIZED, "replay");
    }

    let parsed: ApplyShareSettingsBody = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "ok": false, "error": format!("bad_body: {e}") })),
            )
                .into_response();
        }
    };

    if let Err(e) = apply_share_settings_patch(&state.db, &parsed.share_id, parsed.patch) {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "ok": false, "error": format!("apply_failed: {e}") })),
        )
            .into_response();
    }

    let updated = match state.db.get_share_by_id(&parsed.share_id) {
        Ok(Some(share)) => share,
        Ok(None) => return err(StatusCode::NOT_FOUND, "share_not_found"),
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "ok": false, "error": format!("read_failed: {e}") })),
            )
                .into_response();
        }
    };

    let descriptor = share_metadata_from_record(&updated);
    (StatusCode::OK, Json(json!({ "ok": true, "share": descriptor }))).into_response()
}
