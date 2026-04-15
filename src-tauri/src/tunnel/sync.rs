use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Duration;

use crate::database::{Database, ShareRecord};
use crate::settings;
use crate::tunnel::config::{
    ShareSupport, ShareTunnelMetadata, ShareTunnelRequestLog, TunnelConfig,
};
use crate::tunnel::identity;
use std::sync::Arc;
use tokio::sync::Mutex;

const BATCH_DELAY_MS: u64 = 1500;

#[derive(Clone)]
enum ShareSyncOp {
    Upsert(ShareTunnelMetadata),
    Delete { share_id: String },
}

#[derive(Default)]
struct SyncState {
    pending: HashMap<String, ShareSyncOp>,
    pending_request_logs: HashMap<String, ShareTunnelRequestLog>,
    flush_scheduled: bool,
}

fn global_state() -> &'static Mutex<SyncState> {
    static STATE: OnceLock<Mutex<SyncState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(SyncState::default()))
}

pub fn schedule_sync_share(share: ShareRecord, db: &Arc<Database>) {
    let db = Arc::clone(db);
    tauri::async_runtime::spawn(async move {
        let support = query_share_support(&db).await;
        let mut metadata = share_metadata_from_record(&share);
        metadata.support = support;
        if let Err(err) = enqueue_op(ShareSyncOp::Upsert(metadata)).await {
            log::debug!("[TunnelSync] enqueue upsert failed: {err}");
        }
    });
}

pub(crate) async fn query_share_support(db: &Database) -> ShareSupport {
    ShareSupport {
        claude: db
            .get_proxy_config_for_app("claude")
            .await
            .map(|c| c.enabled)
            .unwrap_or(false),
        codex: db
            .get_proxy_config_for_app("codex")
            .await
            .map(|c| c.enabled)
            .unwrap_or(false),
        gemini: db
            .get_proxy_config_for_app("gemini")
            .await
            .map(|c| c.enabled)
            .unwrap_or(false),
    }
}

pub async fn claim_share_subdomain(
    share: &ShareRecord,
    db: &Arc<Database>,
) -> Result<(), String> {
    claim_share_subdomain_inner(share, db, true).await
}

async fn claim_share_subdomain_inner(
    share: &ShareRecord,
    db: &Arc<Database>,
    allow_identity_reset_retry: bool,
) -> Result<(), String> {
    let config = load_config();
    let client = reqwest::Client::new();
    let identity = identity::ensure_identity(&client, &config)
        .await
        .map_err(|e| e.to_string())?;
    let support = query_share_support(db).await;
    let mut metadata = share_metadata_from_record(share);
    metadata.support = support;
    let url = format!("{}/v1/shares/claim-subdomain", config.get_server_addr());
    let resp = client
        .post(url)
        .json(&serde_json::json!({
            "installationId": identity.installation_id,
            "share": metadata,
        }))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    match handle_claim_response(resp, &identity.installation_id).await {
        Ok(()) => Ok(()),
        Err(message)
            if allow_identity_reset_retry && message.contains("installation not found") =>
        {
            log::warn!(
                "[TunnelSync] portr-rs no longer recognizes installation {}, re-registering identity before subdomain claim",
                identity.installation_id
            );
            identity::reset_identity().map_err(|e| e.to_string())?;
            Box::pin(claim_share_subdomain_inner(share, db, false)).await
        }
        Err(message) => Err(message),
    }?;
    Ok(())
}

async fn handle_claim_response(
    resp: reqwest::Response,
    installation_id: &str,
) -> Result<(), String> {
    if resp.status().is_success() {
        return Ok(());
    }

    let status = resp.status();
    let text = resp
        .text()
        .await
        .unwrap_or_else(|_| format!("HTTP {status}"));
    let message = serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|value| {
            value
                .get("message")
                .and_then(|msg| msg.as_str())
                .map(str::to_string)
        })
        .unwrap_or(text);

    Err(format!(
        "claim subdomain request for installation {installation_id} failed: {message}"
    ))
}

pub fn schedule_delete_share(share_id: String) {
    tauri::async_runtime::spawn(async move {
        if let Err(err) = enqueue_op(ShareSyncOp::Delete { share_id }).await {
            log::debug!("[TunnelSync] enqueue delete failed: {err}");
        }
    });
}

pub fn schedule_sync_share_request_log(log: ShareTunnelRequestLog) {
    tauri::async_runtime::spawn(async move {
        if let Err(err) = enqueue_request_log(log).await {
            log::debug!("[TunnelSync] enqueue share request log failed: {err}");
        }
    });
}

pub async fn sync_recent_share_request_logs(
    db: &crate::database::Database,
    share_id: &str,
    limit: usize,
) -> Result<(), String> {
    let logs = db
        .get_recent_share_request_logs(share_id, limit)
        .map_err(|e| e.to_string())?;
    if logs.is_empty() {
        return Ok(());
    }

    let config = load_config();
    let client = reqwest::Client::new();
    let identity = identity::ensure_identity(&client, &config)
        .await
        .map_err(|e| e.to_string())?;
    let url = format!(
        "{}/v1/share-request-logs/batch-sync",
        config.get_server_addr()
    );
    client
        .post(url)
        .json(&serde_json::json!({
            "installationId": identity.installation_id,
            "logs": logs,
        }))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?;
    Ok(())
}

async fn enqueue_op(op: ShareSyncOp) -> Result<(), String> {
    let state = global_state();
    let mut guard = state.lock().await;
    let key = match &op {
        ShareSyncOp::Upsert(share) => share.share_id.clone(),
        ShareSyncOp::Delete { share_id } => share_id.clone(),
    };
    guard.pending.insert(key, op);
    if !guard.flush_scheduled {
        guard.flush_scheduled = true;
        tauri::async_runtime::spawn(async {
            tokio::time::sleep(Duration::from_millis(BATCH_DELAY_MS)).await;
            if let Err(err) = flush_pending().await {
                log::warn!("[TunnelSync] batch flush failed: {err}");
            }
        });
    }
    Ok(())
}

async fn enqueue_request_log(log: ShareTunnelRequestLog) -> Result<(), String> {
    let state = global_state();
    let mut guard = state.lock().await;
    guard
        .pending_request_logs
        .insert(log.request_id.clone(), log);
    if !guard.flush_scheduled {
        guard.flush_scheduled = true;
        tauri::async_runtime::spawn(async {
            tokio::time::sleep(Duration::from_millis(BATCH_DELAY_MS)).await;
            if let Err(err) = flush_pending().await {
                log::warn!("[TunnelSync] batch flush failed: {err}");
            }
        });
    }
    Ok(())
}

async fn flush_pending() -> Result<(), String> {
    let config = load_config();
    let client = reqwest::Client::new();
    let identity = identity::ensure_identity(&client, &config)
        .await
        .map_err(|e| e.to_string())?;

    let (ops, request_logs) = {
        let state = global_state();
        let mut guard = state.lock().await;
        if guard.pending.is_empty() && guard.pending_request_logs.is_empty() {
            guard.flush_scheduled = false;
            return Ok(());
        }
        let ops = guard.pending.drain().map(|(_, op)| op).collect::<Vec<_>>();
        let request_logs = guard
            .pending_request_logs
            .drain()
            .map(|(_, log)| log)
            .collect::<Vec<_>>();
        guard.flush_scheduled = false;
        (ops, request_logs)
    };

    if !ops.is_empty() {
        let payload_ops = ops
            .into_iter()
            .map(|op| match op {
                ShareSyncOp::Upsert(share) => serde_json::json!({
                    "kind": "upsert",
                    "share": share,
                }),
                ShareSyncOp::Delete { share_id } => serde_json::json!({
                    "kind": "delete",
                    "shareId": share_id,
                }),
            })
            .collect::<Vec<_>>();

        let url = format!("{}/v1/shares/batch-sync", config.get_server_addr());
        client
            .post(url)
            .json(&serde_json::json!({
                "installationId": identity.installation_id,
                "ops": payload_ops,
            }))
            .send()
            .await
            .map_err(|e| e.to_string())?
            .error_for_status()
            .map_err(|e| e.to_string())?;
    }

    if !request_logs.is_empty() {
        let url = format!(
            "{}/v1/share-request-logs/batch-sync",
            config.get_server_addr()
        );
        client
            .post(url)
            .json(&serde_json::json!({
                "installationId": identity.installation_id,
                "logs": request_logs,
            }))
            .send()
            .await
            .map_err(|e| e.to_string())?
            .error_for_status()
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn load_config() -> TunnelConfig {
    let settings = settings::get_settings();
    if let Some(domain) = settings.portr_domain {
        TunnelConfig { domain }
    } else {
        TunnelConfig::default_public_service()
    }
}

fn share_metadata_from_record(share: &ShareRecord) -> ShareTunnelMetadata {
    ShareTunnelMetadata {
        share_id: share.id.clone(),
        share_name: share.name.clone(),
        subdomain: share.subdomain.clone().unwrap_or_default(),
        share_token: share.share_token.clone(),
        app_type: share.app_type.clone(),
        provider_id: share.provider_id.clone(),
        token_limit: share.token_limit,
        tokens_used: share.tokens_used,
        requests_count: share.requests_count,
        share_status: share.status.clone(),
        created_at: share.created_at.clone(),
        expires_at: share.expires_at.clone(),
        support: Default::default(),
    }
}
