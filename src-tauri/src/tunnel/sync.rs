use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Duration;

use crate::app_config::AppType;
use crate::database::{Database, ShareRecord};
use crate::provider::Provider;
use crate::settings;
use crate::tunnel::config::{
    ShareAppRuntimes, ShareRuntimeSnapshot, ShareSupport, ShareTunnelMetadata,
    ShareTunnelRequestLog, ShareUpstreamProvider, ShareUpstreamQuota, ShareUpstreamQuotaTier,
    TunnelConfig,
};
use crate::tunnel::identity;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::sleep;

const BATCH_DELAY_MS: u64 = 1500;
const PORTR_CONNECT_TIMEOUT_SECS: u64 = 10;
const PORTR_REQUEST_TIMEOUT_SECS: u64 = 20;

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

fn portr_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(PORTR_CONNECT_TIMEOUT_SECS))
        .timeout(Duration::from_secs(PORTR_REQUEST_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("create portr-rs HTTP client failed: {e}"))
}

fn describe_portr_send_error(operation: &str, url: &str, err: reqwest::Error) -> String {
    if err.is_timeout() {
        return format!(
            "{operation} timed out after {PORTR_REQUEST_TIMEOUT_SECS}s: {url}. 请检查分享节点是否可访问，或切换到其他分享节点后重试"
        );
    }
    if err.is_connect() {
        return format!(
            "{operation} connection failed: {url}. 请检查网络、DNS、代理或防火墙，或切换到其他分享节点后重试: {err}"
        );
    }
    format!("{operation} request failed: {url}: {err}")
}

async fn send_portr_request(
    request: reqwest::RequestBuilder,
    operation: &str,
    url: &str,
) -> Result<reqwest::Response, String> {
    let retry_request = request.try_clone();
    match request.send().await {
        Ok(resp) => Ok(resp),
        Err(err) if (err.is_timeout() || err.is_connect()) && retry_request.is_some() => {
            log::warn!("[TunnelSync] {operation} failed once for {url}, retrying: {err}");
            sleep(Duration::from_millis(500)).await;
            retry_request
                .expect("checked is_some")
                .send()
                .await
                .map_err(|retry_err| describe_portr_send_error(operation, url, retry_err))
        }
        Err(err) => Err(describe_portr_send_error(operation, url, err)),
    }
}

fn build_signed_request_payload<T: serde::Serialize>(
    identity: &identity::TunnelIdentity,
    action: &str,
    payload_key: &str,
    payload: &T,
) -> Result<serde_json::Value, String> {
    let timestamp_ms = chrono::Utc::now().timestamp_millis();
    let nonce = uuid::Uuid::new_v4().to_string();
    let signature = identity::sign_action_payload(
        identity,
        &identity.installation_id,
        action,
        payload,
        timestamp_ms,
        &nonce,
    )
    .map_err(|e| e.to_string())?;

    Ok(serde_json::json!({
        "installationId": &identity.installation_id,
        "timestampMs": timestamp_ms,
        "nonce": nonce,
        "signature": signature,
        payload_key: payload,
    }))
}

async fn require_auth_bearer_token() -> Result<String, String> {
    crate::email_auth::ensure_access_token()
        .await?
        .ok_or_else(|| "owner email login is required".to_string())
}

pub fn schedule_sync_share(share: ShareRecord, _db: &Arc<Database>) {
    tauri::async_runtime::spawn(async move {
        let metadata = share_metadata_from_record(&share);
        if let Err(err) = enqueue_op(ShareSyncOp::Upsert(metadata)).await {
            log::debug!("[TunnelSync] enqueue upsert failed: {err}");
        }
    });
}

pub async fn sync_share_metadata_now(share: ShareTunnelMetadata) -> Result<(), String> {
    sync_share_metadata_now_inner(share, true).await
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

pub(crate) async fn build_share_runtime_snapshot(
    share: &ShareRecord,
    db: &Database,
) -> ShareRuntimeSnapshot {
    let support = query_share_support(db).await;
    let app_runtimes = build_all_upstream_provider_snapshots(db, &support).await;
    ShareRuntimeSnapshot {
        share_id: share.id.clone(),
        queried_at: chrono::Utc::now().timestamp(),
        support,
        app_runtimes,
    }
}

async fn build_all_upstream_provider_snapshots(
    db: &Database,
    support: &ShareSupport,
) -> ShareAppRuntimes {
    ShareAppRuntimes {
        claude: build_upstream_provider_snapshot_for_app(db, support.claude, AppType::Claude).await,
        codex: build_upstream_provider_snapshot_for_app(db, support.codex, AppType::Codex).await,
        gemini: build_upstream_provider_snapshot_for_app(db, support.gemini, AppType::Gemini).await,
    }
}

async fn build_upstream_provider_snapshot_for_app(
    db: &Database,
    enabled: bool,
    app: AppType,
) -> Option<ShareUpstreamProvider> {
    if !enabled {
        return None;
    }

    let provider_id = match crate::settings::get_effective_current_provider(db, &app) {
        Ok(Some(id)) => id,
        Ok(None) => {
            return Some(unknown_upstream_provider(app.as_str()));
        }
        Err(err) => {
            log::debug!(
                "[TunnelSync] failed to resolve current provider for {}: {err}",
                app.as_str()
            );
            return Some(unknown_upstream_provider(app.as_str()));
        }
    };

    let provider = match db.get_provider_by_id(&provider_id, app.as_str()) {
        Ok(Some(provider)) => provider,
        Ok(None) => return Some(unknown_upstream_provider(app.as_str())),
        Err(err) => {
            log::debug!(
                "[TunnelSync] failed to load provider {provider_id} for {}: {err}",
                app.as_str()
            );
            return Some(unknown_upstream_provider(app.as_str()));
        }
    };

    if let Some(snapshot) = build_official_oauth_snapshot(&app, &provider).await {
        return Some(snapshot);
    }

    Some(ShareUpstreamProvider {
        kind: "custom_provider".to_string(),
        app: app.as_str().to_string(),
        provider_name: Some(provider.name),
        account_email: None,
        quota: None,
    })
}

fn unknown_upstream_provider(app: &str) -> ShareUpstreamProvider {
    ShareUpstreamProvider {
        kind: "unknown".to_string(),
        app: app.to_string(),
        provider_name: None,
        account_email: None,
        quota: None,
    }
}

async fn build_official_oauth_snapshot(
    app: &AppType,
    provider: &Provider,
) -> Option<ShareUpstreamProvider> {
    match app {
        AppType::Codex
            if provider.is_codex_official_with_managed_auth()
                || provider.is_codex_oauth_provider() =>
        {
            build_codex_oauth_snapshot(provider).await
        }
        AppType::Claude if provider.is_claude_oauth_provider() => {
            build_claude_oauth_snapshot(provider).await
        }
        _ => None,
    }
}

async fn build_codex_oauth_snapshot(provider: &Provider) -> Option<ShareUpstreamProvider> {
    use crate::proxy::providers::codex_oauth_auth::CodexOAuthManager;

    let manager = CodexOAuthManager::new(crate::config::get_app_config_dir());
    let bound_account_id = provider
        .meta
        .as_ref()
        .and_then(|meta| meta.managed_account_id_for("codex_oauth"));
    let account_id = match bound_account_id {
        Some(id) if !id.trim().is_empty() => Some(id),
        _ => manager.default_account_id().await,
    };
    let accounts = manager.list_accounts().await;
    let account_email = account_id
        .as_deref()
        .and_then(|id| account_login(&accounts, id));
    let quota = match account_id.as_deref() {
        Some(id) => cached_upstream_quota("codex_oauth", id).await,
        None => None,
    };

    Some(ShareUpstreamProvider {
        kind: "official_oauth".to_string(),
        app: "codex".to_string(),
        provider_name: Some(provider.name.clone()),
        account_email,
        quota,
    })
}

async fn build_claude_oauth_snapshot(provider: &Provider) -> Option<ShareUpstreamProvider> {
    use crate::proxy::providers::claude_oauth_auth::ClaudeOAuthManager;

    let manager = ClaudeOAuthManager::new(crate::config::get_app_config_dir());
    let bound_account_id = provider
        .meta
        .as_ref()
        .and_then(|meta| meta.managed_account_id_for("claude_oauth"));
    let account_id = match bound_account_id {
        Some(id) if !id.trim().is_empty() => Some(id),
        _ => manager.default_account_id().await,
    };
    let accounts = manager.list_accounts().await;
    let account_email = account_id
        .as_deref()
        .and_then(|id| account_login(&accounts, id));
    let quota = match account_id.as_deref() {
        Some(id) => cached_upstream_quota("claude_oauth", id).await,
        None => None,
    };

    Some(ShareUpstreamProvider {
        kind: "official_oauth".to_string(),
        app: "claude".to_string(),
        provider_name: Some(provider.name.clone()),
        account_email,
        quota,
    })
}

fn account_login(
    accounts: &[crate::proxy::providers::copilot_auth::GitHubAccount],
    account_id: &str,
) -> Option<String> {
    accounts
        .iter()
        .find(|account| account.id == account_id)
        .map(|account| account.login.clone())
}

async fn cached_upstream_quota(
    auth_provider: &str,
    account_id: &str,
) -> Option<ShareUpstreamQuota> {
    let service = crate::services::oauth_quota::global_oauth_quota_service()?;
    let cached = service.get(auth_provider, account_id).await?;
    Some(subscription_quota_to_upstream(cached.quota))
}

fn subscription_quota_to_upstream(
    quota: crate::services::subscription::SubscriptionQuota,
) -> ShareUpstreamQuota {
    let status = if quota.success {
        "ok"
    } else if matches!(
        quota.credential_status,
        crate::services::subscription::CredentialStatus::NotFound
    ) {
        "unknown"
    } else {
        "failed"
    };
    ShareUpstreamQuota {
        status: status.to_string(),
        queried_at: quota.queried_at,
        tiers: quota
            .tiers
            .into_iter()
            .map(|tier| ShareUpstreamQuotaTier {
                label: quota_tier_label(&tier.name),
                utilization: tier.utilization,
                resets_at: tier.resets_at,
            })
            .collect(),
    }
}

fn quota_tier_label(name: &str) -> String {
    match name {
        "five_hour" => "5h".to_string(),
        "seven_day" => "1w".to_string(),
        other => other.replace('_', " "),
    }
}

pub async fn claim_share_subdomain(share: &ShareRecord, db: &Arc<Database>) -> Result<(), String> {
    claim_share_subdomain_inner(share, db, true).await
}

async fn claim_share_subdomain_inner(
    share: &ShareRecord,
    db: &Arc<Database>,
    allow_identity_reset_retry: bool,
) -> Result<(), String> {
    let config = load_config();
    let client = portr_client()?;
    let identity = identity::ensure_identity(&client, &config)
        .await
        .map_err(|e| e.to_string())?;
    let metadata = share_metadata_from_record(share);
    let url = format!("{}/v1/shares/claim-subdomain", config.get_server_addr());
    let request_payload =
        build_signed_request_payload(&identity, "share_claim_subdomain", "share", &metadata)?;
    let bearer_token = require_auth_bearer_token().await?;
    let resp = send_portr_request(
        client
            .post(&url)
            .bearer_auth(bearer_token)
            .json(&request_payload),
        "claim subdomain",
        &url,
    )
    .await?;
    match handle_claim_response(resp, &identity.installation_id).await {
        Ok(()) => Ok(()),
        Err(message)
            if allow_identity_reset_retry
                && identity::should_reset_identity_for_api_error(&message) =>
        {
            log::warn!(
                "[TunnelSync] share subdomain claim rejected for installation {}, resetting identity and retrying once: {}",
                identity.installation_id,
                message
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

async fn read_error_message(resp: reqwest::Response) -> String {
    let status = resp.status();
    let text = resp
        .text()
        .await
        .unwrap_or_else(|_| format!("HTTP {status}"));
    serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|value| {
            value
                .get("message")
                .and_then(|msg| msg.as_str())
                .map(str::to_string)
        })
        .unwrap_or(text)
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
    sync_recent_share_request_logs_inner(db, share_id, limit, true).await
}

async fn sync_recent_share_request_logs_inner(
    db: &crate::database::Database,
    share_id: &str,
    limit: usize,
    allow_identity_reset_retry: bool,
) -> Result<(), String> {
    let logs = db
        .get_recent_share_request_logs(share_id, limit)
        .map_err(|e| e.to_string())?;
    if logs.is_empty() {
        return Ok(());
    }

    let config = load_config();
    let client = portr_client()?;
    let identity = identity::ensure_identity(&client, &config)
        .await
        .map_err(|e| e.to_string())?;
    let url = format!(
        "{}/v1/share-request-logs/batch-sync",
        config.get_server_addr()
    );
    let request_payload =
        build_signed_request_payload(&identity, "share_request_logs_batch_sync", "logs", &logs)?;
    let bearer_token = require_auth_bearer_token().await?;
    let resp = send_portr_request(
        client
            .post(&url)
            .bearer_auth(bearer_token)
            .json(&request_payload),
        "sync share request logs",
        &url,
    )
    .await?;

    if resp.status().is_success() {
        return Ok(());
    }

    let message = read_error_message(resp).await;
    if allow_identity_reset_retry && identity::should_reset_identity_for_api_error(&message) {
        log::warn!(
            "[TunnelSync] share request log sync rejected for installation {}, resetting identity and retrying once: {}",
            identity.installation_id,
            message
        );
        identity::reset_identity().map_err(|e| e.to_string())?;
        return Box::pin(sync_recent_share_request_logs_inner(
            db, share_id, limit, false,
        ))
        .await;
    }

    Err(format!(
        "share request log sync request for installation {} failed: {}",
        identity.installation_id, message
    ))
}

async fn sync_share_metadata_now_inner(
    share: ShareTunnelMetadata,
    allow_identity_reset_retry: bool,
) -> Result<(), String> {
    let config = load_config();
    let client = portr_client()?;
    let identity = identity::ensure_identity(&client, &config)
        .await
        .map_err(|e| e.to_string())?;
    let url = format!("{}/v1/shares/sync", config.get_server_addr());
    let request_payload = build_signed_request_payload(&identity, "share_sync", "share", &share)?;
    let bearer_token = require_auth_bearer_token().await?;
    let resp = send_portr_request(
        client
            .post(&url)
            .bearer_auth(bearer_token)
            .json(&request_payload),
        "sync share metadata",
        &url,
    )
    .await?;

    if resp.status().is_success() {
        return Ok(());
    }

    let message = read_error_message(resp).await;

    if allow_identity_reset_retry && identity::should_reset_identity_for_api_error(&message) {
        log::warn!(
            "[TunnelSync] direct share sync rejected for installation {}, resetting identity and retrying once: {}",
            identity.installation_id,
            message
        );
        identity::reset_identity().map_err(|e| e.to_string())?;
        return Box::pin(sync_share_metadata_now_inner(share, false)).await;
    }

    Err(format!(
        "direct share sync request for installation {} failed: {}",
        identity.installation_id, message
    ))
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
    flush_pending_inner(true).await
}

async fn flush_pending_inner(allow_identity_reset_retry: bool) -> Result<(), String> {
    let config = load_config();
    let client = portr_client()?;
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
        let bearer_token = require_auth_bearer_token().await?;
        let payload_ops = ops
            .iter()
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
        let request_payload =
            build_signed_request_payload(&identity, "share_batch_sync", "ops", &payload_ops)?;
        let resp = send_portr_request(
            client
                .post(&url)
                .bearer_auth(&bearer_token)
                .json(&request_payload),
            "batch sync shares",
            &url,
        )
        .await?;
        if !resp.status().is_success() {
            let message = read_error_message(resp).await;
            if allow_identity_reset_retry && identity::should_reset_identity_for_api_error(&message)
            {
                log::warn!(
                    "[TunnelSync] batch share sync rejected for installation {}, resetting identity and retrying once: {}",
                    identity.installation_id,
                    message
                );
                identity::reset_identity().map_err(|e| e.to_string())?;
                let state = global_state();
                let mut guard = state.lock().await;
                for op in ops {
                    let key = match &op {
                        ShareSyncOp::Upsert(share) => share.share_id.clone(),
                        ShareSyncOp::Delete { share_id } => share_id.clone(),
                    };
                    guard.pending.insert(key, op);
                }
                for log in request_logs {
                    guard
                        .pending_request_logs
                        .insert(log.request_id.clone(), log);
                }
                return Box::pin(flush_pending_inner(false)).await;
            }

            return Err(format!(
                "batch share sync request for installation {} failed: {}",
                identity.installation_id, message
            ));
        }
    }

    if !request_logs.is_empty() {
        let bearer_token = require_auth_bearer_token().await?;
        let url = format!(
            "{}/v1/share-request-logs/batch-sync",
            config.get_server_addr()
        );
        let request_payload = build_signed_request_payload(
            &identity,
            "share_request_logs_batch_sync",
            "logs",
            &request_logs,
        )?;
        let resp = send_portr_request(
            client
                .post(&url)
                .bearer_auth(&bearer_token)
                .json(&request_payload),
            "batch sync share request logs",
            &url,
        )
        .await?;
        if !resp.status().is_success() {
            let message = read_error_message(resp).await;
            if allow_identity_reset_retry && identity::should_reset_identity_for_api_error(&message)
            {
                log::warn!(
                    "[TunnelSync] batch share request log sync rejected for installation {}, resetting identity and retrying once: {}",
                    identity.installation_id,
                    message
                );
                identity::reset_identity().map_err(|e| e.to_string())?;
                let state = global_state();
                let mut guard = state.lock().await;
                for log in request_logs {
                    guard
                        .pending_request_logs
                        .insert(log.request_id.clone(), log);
                }
                return Box::pin(flush_pending_inner(false)).await;
            }

            return Err(format!(
                "batch share request log sync request for installation {} failed: {}",
                identity.installation_id, message
            ));
        }
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

pub(crate) fn share_metadata_from_record(share: &ShareRecord) -> ShareTunnelMetadata {
    ShareTunnelMetadata {
        share_id: share.id.clone(),
        share_name: share.name.clone(),
        owner_email: share.owner_email.clone(),
        shared_with_emails: share.shared_with_emails.clone(),
        description: share.description.clone(),
        for_sale: share.for_sale.clone(),
        subdomain: share.subdomain.clone().unwrap_or_default(),
        share_token: share.share_token.clone(),
        app_type: share.app_type.clone(),
        provider_id: share.provider_id.clone(),
        token_limit: share.token_limit,
        parallel_limit: share.parallel_limit,
        tokens_used: share.tokens_used,
        requests_count: share.requests_count,
        share_status: share.status.clone(),
        created_at: share.created_at.clone(),
        expires_at: share.expires_at.clone(),
        support: Default::default(),
        upstream_provider: None,
        app_runtimes: Default::default(),
    }
}
