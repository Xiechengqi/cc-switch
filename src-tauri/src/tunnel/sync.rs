use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Duration;

use crate::app_config::AppType;
use crate::database::{Database, ShareAppAccess, ShareAppSettings, ShareRecord};
use crate::provider::Provider;
use crate::settings;
use crate::tunnel::config::{
    ShareAppProvider, ShareAppProviders, ShareAppRuntimes, ShareRuntimeSnapshot, ShareSupport,
    ShareTunnelMetadata, ShareTunnelRequestLog, ShareUpstreamModel, ShareUpstreamProvider,
    ShareUpstreamQuota, ShareUpstreamQuotaTier, TunnelConfig,
};
use crate::tunnel::identity;
use futures::StreamExt;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::sleep;

const BATCH_DELAY_MS: u64 = 1500;
const SHARE_ROUTER_CONNECT_TIMEOUT_SECS: u64 = 10;
const SHARE_ROUTER_REQUEST_TIMEOUT_SECS: u64 = 20;

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareSettingsPatch {
    #[serde(default)]
    pub owner_email: Option<String>,
    #[serde(default)]
    pub description: Option<Option<String>>,
    #[serde(default)]
    pub for_sale: Option<String>,
    #[serde(default)]
    pub sale_market_kind: Option<String>,
    #[serde(default)]
    pub market_access_mode: Option<String>,
    #[serde(default)]
    pub shared_with_emails: Option<Vec<String>>,
    #[serde(default)]
    pub access_by_app: Option<HashMap<String, ShareAppAccess>>,
    #[serde(default)]
    pub app_settings: Option<HashMap<String, ShareAppSettings>>,
    #[serde(default)]
    pub for_sale_official_price_percent_by_app: Option<HashMap<String, u16>>,
    #[serde(default)]
    pub token_limit: Option<i64>,
    #[serde(default)]
    pub parallel_limit: Option<i64>,
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub auto_start: Option<bool>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ShareEditView {
    id: String,
    share_id: String,
    revision: i64,
    patch: ShareSettingsPatch,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PendingShareEditsResponse {
    edits: Vec<ShareEditView>,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ShareEditAckPayload<'a> {
    edit_id: &'a str,
    revision: i64,
    status: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_message: Option<&'a str>,
}

#[derive(Clone)]
enum ShareSyncOp {
    Upsert(Box<ShareTunnelMetadata>),
    Delete { share_id: String },
    DeleteAll,
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

fn share_router_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(SHARE_ROUTER_CONNECT_TIMEOUT_SECS))
        .timeout(Duration::from_secs(SHARE_ROUTER_REQUEST_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("create cc-switch-router HTTP client failed: {e}"))
}

fn describe_share_router_send_error(operation: &str, url: &str, err: reqwest::Error) -> String {
    if err.is_timeout() {
        return format!(
            "{operation} timed out after {SHARE_ROUTER_REQUEST_TIMEOUT_SECS}s: {url}. 请检查分享节点是否可访问，或切换到其他分享节点后重试"
        );
    }
    if err.is_connect() {
        return format!(
            "{operation} connection failed: {url}. 请检查网络、DNS、代理或防火墙，或切换到其他分享节点后重试: {err}"
        );
    }
    format!("{operation} request failed: {url}: {err}")
}

async fn send_share_router_request(
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
                .map_err(|retry_err| describe_share_router_send_error(operation, url, retry_err))
        }
        Err(err) => Err(describe_share_router_send_error(operation, url, err)),
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

fn build_signed_claim_request_payload<T: serde::Serialize, U: serde::Serialize>(
    identity: &identity::TunnelIdentity,
    claim: &T,
    share: &U,
) -> Result<serde_json::Value, String> {
    let timestamp_ms = chrono::Utc::now().timestamp_millis();
    let nonce = uuid::Uuid::new_v4().to_string();
    let signature = identity::sign_action_payload(
        identity,
        &identity.installation_id,
        "share_claim_subdomain",
        claim,
        timestamp_ms,
        &nonce,
    )
    .map_err(|e| e.to_string())?;

    Ok(serde_json::json!({
        "installationId": &identity.installation_id,
        "timestampMs": timestamp_ms,
        "nonce": nonce,
        "signature": signature,
        "claim": claim,
        "share": share,
    }))
}

pub fn schedule_sync_share(share: ShareRecord, _db: &Arc<Database>) {
    tauri::async_runtime::spawn(async move {
        let metadata = share_metadata_from_record(&share);
        if let Err(err) = enqueue_op(ShareSyncOp::Upsert(Box::new(metadata))).await {
            log::debug!("[TunnelSync] enqueue upsert failed: {err}");
        }
    });
}

pub fn schedule_pull_pending_share_edits(db: Arc<Database>) {
    tauri::async_runtime::spawn(async move {
        if let Err(err) = pull_and_apply_pending_share_edits(&db).await {
            log::debug!("[TunnelSync] pending share edit pull skipped/failed: {err}");
        }
    });
}

pub fn spawn_share_edit_event_listener(db: Arc<Database>) {
    tauri::async_runtime::spawn(async move {
        if let Err(err) = share_edit_event_loop(db).await {
            log::warn!("[TunnelSync] share edit event listener stopped: {err}");
        }
    });
}

pub async fn pull_and_apply_pending_share_edits(db: &Arc<Database>) -> Result<(), String> {
    let shares = db.list_shares().map_err(|e| e.to_string())?;
    if shares.is_empty() {
        return Ok(());
    }
    let share_ids = shares
        .iter()
        .map(|share| share.id.clone())
        .collect::<Vec<_>>();
    let config = load_config();
    let client = share_router_client()?;
    let identity = identity::ensure_identity(&client, &config)
        .await
        .map_err(|e| e.to_string())?;
    let request_payload =
        build_signed_request_payload(&identity, "share_pending_edits", "shareIds", &share_ids)?;
    let url = format!("{}/v1/shares/pending-edits", config.get_server_addr());
    let resp = send_share_router_request(
        client.post(&url).json(&request_payload),
        "pull pending share edits",
        &url,
    )
    .await?;
    if !resp.status().is_success() {
        return Err(read_error_message(resp).await);
    }
    let response = resp
        .json::<PendingShareEditsResponse>()
        .await
        .map_err(|e| format!("decode pending share edits failed: {e}"))?;
    for edit in response.edits {
        let result = apply_share_settings_patch(db, &edit.share_id, edit.patch.clone())
            .map(|_| "applied".to_string())
            .map_err(|err| err.to_string());
        if result.as_deref() == Ok("applied") {
            if let Some(share) = db
                .get_share_by_id(&edit.share_id)
                .map_err(|e| e.to_string())?
            {
                schedule_sync_share(share, db);
            }
        }
        let (status, error_message) = match result {
            Ok(status) => (status, None),
            Err(err) => ("rejected".to_string(), Some(err)),
        };
        ack_share_edit(
            &client,
            &config,
            &identity,
            &edit,
            &status,
            error_message.as_deref(),
        )
        .await?;
    }
    Ok(())
}

pub(crate) fn apply_share_settings_patch(
    db: &Arc<Database>,
    share_id: &str,
    patch: ShareSettingsPatch,
) -> Result<(), crate::error::AppError> {
    let current = db
        .get_share_by_id(share_id)?
        .ok_or_else(|| crate::error::AppError::Message(format!("Share not found: {share_id}")))?;

    // Owner and ACL are applied together in a single write. Updating the owner
    // on its own re-normalizes the existing shareto list against the *new*
    // owner (dropping the new owner from it) but never adds the *old* owner —
    // so an owner transfer that touched owner without a matching shareto write
    // would silently strip the previous owner's access. Computing the final
    // owner + shareto here, and demoting the old owner into shareto whenever the
    // owner changes, keeps the transfer atomic and loss-free regardless of which
    // patch fields the router sent.
    if patch.owner_email.is_some()
        || patch.shared_with_emails.is_some()
        || patch.market_access_mode.is_some()
        || patch.sale_market_kind.is_some()
    {
        let old_owner = current.owner_email.trim().to_ascii_lowercase();
        let next_owner = match &patch.owner_email {
            Some(value) => value.trim().to_ascii_lowercase(),
            None => old_owner.clone(),
        };
        let mut next_shared = patch
            .shared_with_emails
            .clone()
            .unwrap_or_else(|| current.shared_with_emails.clone());
        if next_owner != old_owner
            && !old_owner.is_empty()
            && !next_shared
                .iter()
                .any(|email| email.trim().eq_ignore_ascii_case(&old_owner))
        {
            next_shared.push(current.owner_email.clone());
        }
        let next_mode = patch
            .market_access_mode
            .as_deref()
            .unwrap_or(&current.market_access_mode);
        // update_acl normalizes the owner + list (lowercases, dedupes, strips the
        // owner from shareto) and persists owner + shareto + mode in one update.
        crate::services::share::ShareService::update_acl(
            db,
            share_id,
            &next_owner,
            next_shared,
            next_mode,
            patch.access_by_app.clone(),
            patch.sale_market_kind.as_deref(),
        )?;
    }
    if let Some(description) = patch.description {
        crate::services::share::ShareService::update_description(db, share_id, description)?;
    }
    if let Some(for_sale) = patch.for_sale {
        crate::services::share::ShareService::update_for_sale(db, share_id, &for_sale)?;
    }
    if let Some(pricing) = patch.for_sale_official_price_percent_by_app {
        crate::services::share::ShareService::update_for_sale_official_price_percent_by_app(
            db, share_id, pricing,
        )?;
    }
    if let Some(app_settings) = patch.app_settings {
        crate::services::share::ShareService::update_app_settings(db, share_id, app_settings)?;
    }
    if let Some(token_limit) = patch.token_limit {
        crate::services::share::ShareService::update_token_limit(db, share_id, token_limit)?;
    }
    if let Some(parallel_limit) = patch.parallel_limit {
        crate::services::share::ShareService::update_parallel_limit(db, share_id, parallel_limit)?;
    }
    if let Some(expires_at) = patch.expires_at {
        crate::services::share::ShareService::update_expires_at(db, share_id, &expires_at)?;
    }
    if let Some(auto_start) = patch.auto_start {
        crate::services::share::ShareService::update_auto_start(db, share_id, auto_start)?;
    }
    Ok(())
}

async fn ack_share_edit(
    client: &reqwest::Client,
    config: &TunnelConfig,
    identity: &identity::TunnelIdentity,
    edit: &ShareEditView,
    status: &str,
    error_message: Option<&str>,
) -> Result<(), String> {
    let ack = ShareEditAckPayload {
        edit_id: &edit.id,
        revision: edit.revision,
        status,
        error_message,
    };
    let payload = build_signed_request_payload(identity, "share_edit_ack", "ack", &ack)?;
    let url = format!("{}/v1/shares/edit-ack", config.get_server_addr());
    let resp =
        send_share_router_request(client.post(&url).json(&payload), "ack share edit", &url).await?;
    if resp.status().is_success() {
        Ok(())
    } else {
        Err(read_error_message(resp).await)
    }
}

async fn share_edit_event_loop(db: Arc<Database>) -> Result<(), String> {
    loop {
        let config = load_config();
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(SHARE_ROUTER_CONNECT_TIMEOUT_SECS))
            .build()
            .map_err(|e| format!("create share edit event HTTP client failed: {e}"))?;
        let shares = db.list_shares().map_err(|e| e.to_string())?;
        if shares.is_empty() {
            sleep(Duration::from_secs(30)).await;
            continue;
        }
        let identity = identity::ensure_identity(&client, &config)
            .await
            .map_err(|e| e.to_string())?;
        let timestamp_ms = chrono::Utc::now().timestamp_millis();
        let nonce = uuid::Uuid::new_v4().to_string();
        let event_payload = serde_json::json!({ "installationId": &identity.installation_id });
        let signature = identity::sign_action_payload(
            &identity,
            &identity.installation_id,
            "share_edit_events",
            &event_payload,
            timestamp_ms,
            &nonce,
        )
        .map_err(|e| e.to_string())?;
        let url = format!(
            "{}/v1/shares/edit-events?installationId={}&timestampMs={}&nonce={}&signature={}",
            config.get_server_addr(),
            urlencoding::encode(&identity.installation_id),
            timestamp_ms,
            urlencoding::encode(&nonce),
            urlencoding::encode(&signature),
        );
        match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => {
                let mut stream = resp.bytes_stream();
                let mut buffer = String::new();
                while let Some(chunk) = stream.next().await {
                    let chunk = chunk.map_err(|e| e.to_string())?;
                    buffer.push_str(&String::from_utf8_lossy(&chunk));
                    while let Some(index) = buffer.find('\n') {
                        let line = buffer[..index].trim().to_string();
                        buffer = buffer[index + 1..].to_string();
                        if line.starts_with("event: share_edit_available")
                            || line.starts_with("event: resync")
                        {
                            if let Err(err) = pull_and_apply_pending_share_edits(&db).await {
                                log::warn!("[TunnelSync] share edit event pull failed: {err}");
                            }
                        }
                    }
                }
            }
            Ok(resp) => {
                let message = read_error_message(resp).await;
                log::warn!("[TunnelSync] share edit event stream rejected: {message}");
            }
            Err(err) => {
                log::debug!("[TunnelSync] share edit event stream failed: {err}");
            }
        }
        sleep(Duration::from_secs(10)).await;
    }
}

pub async fn sync_share_metadata_now(share: ShareTunnelMetadata) -> Result<(), String> {
    sync_share_metadata_now_inner(share, true).await
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ShareRuntimeRefreshPayload {
    share_id: String,
    subdomain: String,
}

pub fn schedule_share_runtime_refresh_after_provider_switch(db: Arc<Database>, app: AppType) {
    if !matches!(app, AppType::Claude | AppType::Codex | AppType::Gemini) {
        return;
    }

    tauri::async_runtime::spawn(async move {
        if let Err(err) = refresh_share_runtime_after_provider_switch(&db, &app, true).await {
            log::debug!(
                "[TunnelSync] share runtime refresh after {} provider switch skipped/failed: {err}",
                app.as_str()
            );
        }
    });
}

async fn refresh_share_runtime_after_provider_switch(
    db: &Database,
    app: &AppType,
    allow_identity_reset_retry: bool,
) -> Result<(), String> {
    // 多 share 模式：app 的 current provider 切换会影响所有该 app_type 下的
    // active share；逐个推送 runtime 给 router 同步。
    //
    // share 与 provider 1:1，一次 current provider 切换其实只对"未绑定该 provider"
    // 的 share 没意义，但 share 自己对外的运行时（quota / model health 等）就在
    // share-runtime 快照里——刷新成本低，简单刷全部即可。
    let app_str = app.as_str();
    let shares: Vec<_> = db
        .list_shares()
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|share| {
            // P8 多 app share：筛选所有"有 app_str 这个 slot binding"的 share。
            share.status == "active"
                && share.subdomain.is_some()
                && share.bindings.contains_key(app_str)
        })
        .collect();

    if shares.is_empty() {
        return Ok(());
    }

    let config = load_config();
    let client = share_router_client()?;
    let identity = identity::ensure_identity(&client, &config)
        .await
        .map_err(|e| e.to_string())?;

    let mut last_err: Option<String> = None;
    for share in shares {
        let Some(subdomain) = share.subdomain.clone() else {
            continue;
        };
        let payload = ShareRuntimeRefreshPayload {
            share_id: share.id.clone(),
            subdomain,
        };
        let url = format!("{}/v1/shares/runtime-refresh", config.get_server_addr());
        let request_payload =
            build_signed_request_payload(&identity, "share_runtime_refresh", "refresh", &payload)?;
        let resp = match send_share_router_request(
            client.post(&url).json(&request_payload),
            "refresh share runtime",
            &url,
        )
        .await
        {
            Ok(resp) => resp,
            Err(err) => {
                log::warn!(
                    "[TunnelSync] refresh share runtime request failed for share {}: {err}",
                    share.id
                );
                last_err = Some(err);
                continue;
            }
        };

        if resp.status().is_success() {
            log::debug!(
                "[TunnelSync] refreshed share runtime after {} provider switch for share {}",
                app_str,
                share.id
            );
            continue;
        }

        let message = read_error_message(resp).await;
        if allow_identity_reset_retry && identity::should_reset_identity_for_api_error(&message) {
            log::warn!(
                "[TunnelSync] share runtime refresh rejected for installation {}, refreshing identity and retrying once: {}",
                identity.installation_id,
                message
            );
            identity::refresh_installation_registration(&client, &config)
                .await
                .map_err(|e| e.to_string())?;
            // 重试整批，避免身份刷新后 partial 状态。
            return Box::pin(refresh_share_runtime_after_provider_switch(db, app, false)).await;
        }

        log::warn!(
            "[TunnelSync] share runtime refresh request for installation {} share {} failed: {}",
            identity.installation_id,
            share.id,
            message
        );
        last_err = Some(message);
    }

    match last_err {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

/// P11：单个 share 的 SUPPORT —— 直接来自 share.bindings。一条 share 在某个 app
/// slot 有 binding，该 app 就是 supported；否则 false。这与请求路径上
/// `resolve_share_outcome` 的判断口径一致：没有 binding 的 slot 会被 401 拒绝。
fn share_support_from_bindings(share: &ShareRecord) -> ShareSupport {
    ShareSupport {
        claude: share.bindings.contains_key("claude"),
        codex: share.bindings.contains_key("codex"),
        gemini: share.bindings.contains_key("gemini"),
    }
}

pub(crate) async fn build_share_runtime_snapshot(
    share: &ShareRecord,
    db: &Database,
) -> ShareRuntimeSnapshot {
    // P11：snapshot 必须 per-share。SUPPORT 和 app_runtimes 都按 share.bindings
    // 决定，避免同 client 多 share 在 router dashboard 上显示同一份内容。
    let support = share_support_from_bindings(share);
    let app_runtimes = build_all_upstream_provider_snapshots(db, share).await;
    let app_providers = build_all_app_provider_snapshots(db, share).await;
    // model_health 同样 per-share：store 按 (share_id, app_type) 索引；额外把
    // 自己当前 binding 的 app 集合传过去，挡住 store 里残留的、已经不再绑定的
    // 旧条目（解绑/改绑路径上有 active purge，但 defense in depth）。
    let bound_apps: std::collections::HashSet<String> = share
        .bindings
        .iter()
        .filter(|(_, pid)| !pid.trim().is_empty())
        .map(|(app, _)| app.clone())
        .collect();
    let model_health = crate::tunnel::model_health::current_share_model_health_summary_for_share(
        &share.id,
        &bound_apps,
    )
    .await;
    ShareRuntimeSnapshot {
        share_id: share.id.clone(),
        queried_at: chrono::Utc::now().timestamp(),
        token_limit: share.token_limit,
        tokens_used: share.tokens_used,
        requests_count: share.requests_count,
        share_status: share.status.clone(),
        support,
        app_runtimes,
        app_providers,
        model_health,
    }
}

async fn build_all_app_provider_snapshots(db: &Database, share: &ShareRecord) -> ShareAppProviders {
    ShareAppProviders {
        claude: build_app_provider_snapshots(db, AppType::Claude, share).await,
        codex: build_app_provider_snapshots(db, AppType::Codex, share).await,
        gemini: build_app_provider_snapshots(db, AppType::Gemini, share).await,
    }
}

async fn build_app_provider_snapshots(
    db: &Database,
    app: AppType,
    share: &ShareRecord,
) -> Vec<ShareAppProvider> {
    let bound_provider_id = share.bindings.get(app.as_str()).cloned();
    let enabled = bound_provider_id.is_some();
    let providers = match db.get_all_providers(app.as_str()) {
        Ok(providers) => providers,
        Err(err) => {
            log::debug!(
                "[TunnelSync] failed to load providers for {}: {err}",
                app.as_str()
            );
            return Vec::new();
        }
    };

    let mut snapshots = Vec::with_capacity(providers.len());
    for (provider_id, provider) in providers {
        snapshots.push(
            build_app_provider_snapshot(
                &app,
                provider,
                // is_current 现在按 "是否就是本 share 在该 slot 绑的那个" 判定。
                bound_provider_id.as_deref() == Some(provider_id.as_str()),
                enabled,
            )
            .await,
        );
    }
    snapshots
}

async fn build_app_provider_snapshot(
    app: &AppType,
    provider: Provider,
    is_current: bool,
    enabled: bool,
) -> ShareAppProvider {
    let provider_type = provider
        .meta
        .as_ref()
        .and_then(|meta| meta.provider_type.clone());
    let mut kind = provider.category.clone();
    let mut for_sale_official_price_percent = provider_sale_percent(&provider);
    let mut account_email = None;
    let mut api_url = custom_provider_api_url(app, &provider);
    let mut quota = None;
    let mut models = custom_provider_models(app, &provider);

    if let Some(upstream) = build_official_oauth_snapshot(app, &provider).await {
        kind = Some(upstream.kind);
        if for_sale_official_price_percent.is_none() {
            for_sale_official_price_percent = upstream.for_sale_official_price_percent;
        }
        account_email = upstream.account_email;
        api_url = upstream.api_url;
        quota = upstream.quota;
        if !upstream.models.is_empty() {
            models = upstream.models;
        }
    }

    if let Some(auth_provider) = managed_oauth_provider_for_app(app, &provider) {
        let (managed_account_email, managed_quota) =
            managed_oauth_account_summary(auth_provider, &provider).await;
        if kind.is_none() {
            kind = Some("official_oauth".to_string());
        }
        if account_email.is_none() {
            account_email = managed_account_email;
        }
        if quota.is_none() {
            quota = with_quota_dispatch_limit(managed_quota, &provider);
        }
    }

    let codex_image_generation_enabled = matches!(app, AppType::Codex)
        && provider.is_codex_official_with_managed_auth()
        && provider.codex_image_generation_enabled();

    ShareAppProvider {
        id: provider.id,
        name: provider.name,
        app: app.as_str().to_string(),
        kind,
        provider_type,
        is_current,
        enabled,
        codex_image_generation_enabled,
        for_sale_official_price_percent,
        account_email,
        api_url,
        quota,
        models,
    }
}

async fn build_all_upstream_provider_snapshots(
    db: &Database,
    share: &ShareRecord,
) -> ShareAppRuntimes {
    let (kiro, cursor, antigravity, copilot) = tokio::join!(
        build_oauth_provider_snapshot(db, "kiro_oauth"),
        build_oauth_provider_snapshot(db, "cursor_oauth"),
        build_oauth_provider_snapshot(db, "antigravity_oauth"),
        build_oauth_provider_snapshot(db, "github_copilot"),
    );
    // P11：每个 app 的 runtime 必须按 share.bindings[app] 取，避免所有 share 都拿到
    // "全局 current provider"的同一份数据。slot 没绑定时该 app 的 runtime 留空。
    let mut runtimes = ShareAppRuntimes {
        claude: build_upstream_provider_snapshot_for_app(
            db,
            share.bindings.get("claude").map(String::as_str),
            AppType::Claude,
        )
        .await,
        codex: build_upstream_provider_snapshot_for_app(
            db,
            share.bindings.get("codex").map(String::as_str),
            AppType::Codex,
        )
        .await,
        gemini: build_upstream_provider_snapshot_for_app(
            db,
            share.bindings.get("gemini").map(String::as_str),
            AppType::Gemini,
        )
        .await,
        kiro,
        cursor,
        antigravity,
        copilot,
    };
    apply_share_for_sale_pricing_override(share, &mut runtimes);
    runtimes
}

/// Build a `ShareUpstreamProvider` snapshot for a standalone OAuth provider
/// (kiro / cursor / antigravity / copilot) by reading the cached quota.
/// Returns `None` if no account is configured or no quota has been fetched yet.
async fn build_oauth_provider_snapshot(
    db: &Database,
    auth_provider: &str,
) -> Option<ShareUpstreamProvider> {
    let service = crate::services::oauth_quota::global_oauth_quota_service()?;
    let cached = service.get_first_for_provider(auth_provider).await?;
    // Only surface providers with a successful quota fetch.
    if !cached.quota.success {
        return None;
    }
    let account_email = oauth_account_label(auth_provider, &cached.account_id)
        .await
        .or(Some(cached.account_id));
    let mut quota = subscription_quota_to_upstream(cached.quota);
    quota.dispatch_limit_percent = quota_dispatch_limit_for_auth_provider(db, auth_provider);
    Some(ShareUpstreamProvider {
        kind: "official_oauth".to_string(),
        app: auth_provider.to_string(),
        provider_name: cached.provider_name,
        for_sale_official_price_percent: None,
        account_email,
        api_url: None,
        quota: Some(quota),
        models: Vec::new(),
    })
}

fn apply_share_for_sale_pricing_override(share: &ShareRecord, runtimes: &mut ShareAppRuntimes) {
    if share.for_sale != "Yes" {
        return;
    }
    if let Some(percent) = share.for_sale_official_price_percent_by_app.get("claude") {
        if let Some(runtime) = runtimes.claude.as_mut() {
            if runtime.for_sale_official_price_percent.is_none() {
                runtime.for_sale_official_price_percent = Some(*percent);
            }
        }
    }
    if let Some(percent) = share.for_sale_official_price_percent_by_app.get("codex") {
        if let Some(runtime) = runtimes.codex.as_mut() {
            if runtime.for_sale_official_price_percent.is_none() {
                runtime.for_sale_official_price_percent = Some(*percent);
            }
        }
    }
    if let Some(percent) = share.for_sale_official_price_percent_by_app.get("gemini") {
        if let Some(runtime) = runtimes.gemini.as_mut() {
            if runtime.for_sale_official_price_percent.is_none() {
                runtime.for_sale_official_price_percent = Some(*percent);
            }
        }
    }
}

async fn build_upstream_provider_snapshot_for_app(
    db: &Database,
    bound_provider_id: Option<&str>,
    app: AppType,
) -> Option<ShareUpstreamProvider> {
    // P11：bound_provider_id 来自 share.bindings[app_str]。slot 未绑定 → None；
    // 绑定指向已被删除的 provider → unknown，避免 router dashboard 留白。
    let provider_id = match bound_provider_id {
        Some(id) if !id.is_empty() => id.to_string(),
        _ => return None,
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

    if let Some(mut snapshot) = build_official_oauth_snapshot(&app, &provider).await {
        if snapshot.models.is_empty() {
            snapshot.models = custom_provider_models(&app, &provider);
        }
        return Some(snapshot);
    }

    let mut snapshot = ShareUpstreamProvider {
        kind: "custom_provider".to_string(),
        app: app.as_str().to_string(),
        provider_name: Some(provider.name.clone()),
        for_sale_official_price_percent: provider_sale_percent(&provider),
        account_email: None,
        api_url: custom_provider_api_url(&app, &provider),
        quota: None,
        models: custom_provider_models(&app, &provider),
    };

    if let Some(auth_provider) = managed_oauth_provider_for_app(&app, &provider) {
        let (account_email, quota) = managed_oauth_account_summary(auth_provider, &provider).await;
        snapshot.kind = "official_oauth".to_string();
        snapshot.account_email = account_email;
        snapshot.quota = with_quota_dispatch_limit(quota, &provider);
    }

    Some(snapshot)
}

fn unknown_upstream_provider(app: &str) -> ShareUpstreamProvider {
    ShareUpstreamProvider {
        kind: "unknown".to_string(),
        app: app.to_string(),
        provider_name: None,
        for_sale_official_price_percent: None,
        account_email: None,
        api_url: None,
        quota: None,
        models: Vec::new(),
    }
}

fn provider_sale_percent(provider: &Provider) -> Option<u16> {
    provider
        .meta
        .as_ref()
        .and_then(|meta| meta.for_sale_official_price_percent)
}

fn provider_quota_dispatch_limit_percent(provider: &Provider) -> Option<f64> {
    let value = provider
        .meta
        .as_ref()
        .and_then(|meta| meta.quota_dispatch_limit_percent)?;
    if value == 0 {
        None
    } else {
        Some(f64::from(value.min(100)))
    }
}

fn with_quota_dispatch_limit(
    quota: Option<ShareUpstreamQuota>,
    provider: &Provider,
) -> Option<ShareUpstreamQuota> {
    let mut quota = quota?;
    if !provider.is_google_gemini_oauth_provider()
        && !provider.is_google_gemini_official_with_managed_auth()
        && !provider.is_antigravity_family_provider()
    {
        quota.dispatch_limit_percent = provider_quota_dispatch_limit_percent(provider);
    }
    Some(quota)
}

fn quota_dispatch_limit_for_auth_provider(db: &Database, auth_provider: &str) -> Option<f64> {
    if matches!(auth_provider, "google_gemini_oauth" | "antigravity_oauth") {
        return None;
    }
    for app in [AppType::Claude, AppType::Codex, AppType::Gemini] {
        let providers = match db.get_all_providers(app.as_str()) {
            Ok(providers) => providers,
            Err(_) => continue,
        };
        for provider in providers.values() {
            let Some(meta) = provider.meta.as_ref() else {
                continue;
            };
            let provider_matches = meta.provider_type.as_deref() == Some(auth_provider)
                || meta
                    .auth_binding
                    .as_ref()
                    .and_then(|binding| binding.auth_provider.as_deref())
                    == Some(auth_provider);
            if provider_matches {
                if let Some(limit) = provider_quota_dispatch_limit_percent(provider) {
                    return Some(limit);
                }
            }
        }
    }
    None
}

fn managed_oauth_provider_for_app(app: &AppType, provider: &Provider) -> Option<&'static str> {
    match app {
        AppType::Claude if provider.is_kiro_oauth_provider() => Some("kiro_oauth"),
        AppType::Claude | AppType::Codex if provider.is_cursor_oauth_provider() => {
            Some("cursor_oauth")
        }
        AppType::Claude | AppType::Gemini if provider.is_antigravity_family_provider() => {
            Some("antigravity_oauth")
        }
        _ => None,
    }
}

async fn managed_oauth_account_summary(
    auth_provider: &str,
    provider: &Provider,
) -> (Option<String>, Option<ShareUpstreamQuota>) {
    let account_id = match provider
        .meta
        .as_ref()
        .and_then(|meta| meta.managed_account_id_for(auth_provider))
        .filter(|id| !id.trim().is_empty())
    {
        Some(id) => Some(id),
        None => default_oauth_account_id(auth_provider).await,
    };

    let Some(account_id) = account_id else {
        return (None, None);
    };

    let account_email = oauth_account_label(auth_provider, &account_id)
        .await
        .or(Some(account_id.clone()));
    let quota = cached_upstream_quota(auth_provider, &account_id).await;
    (account_email, quota)
}

async fn default_oauth_account_id(auth_provider: &str) -> Option<String> {
    let data_dir = crate::config::get_app_config_dir();
    match auth_provider {
        "kiro_oauth" => {
            crate::proxy::providers::kiro_oauth_auth::KiroOAuthManager::new(data_dir)
                .default_account_id()
                .await
        }
        "cursor_oauth" => {
            crate::proxy::providers::cursor_oauth_auth::CursorOAuthManager::new(data_dir)
                .default_account_id()
                .await
        }
        "antigravity_oauth" => {
            crate::proxy::providers::antigravity_oauth_auth::AntigravityOAuthManager::new(data_dir)
                .default_account_id()
                .await
        }
        _ => None,
    }
}

async fn oauth_account_label(auth_provider: &str, account_id: &str) -> Option<String> {
    let data_dir = crate::config::get_app_config_dir();
    match auth_provider {
        "kiro_oauth" => {
            let manager = crate::proxy::providers::kiro_oauth_auth::KiroOAuthManager::new(data_dir);
            manager.get_account(account_id).await.and_then(|account| {
                public_account_label(account.email.as_deref(), None, &account.account_id)
            })
        }
        "cursor_oauth" => {
            let manager =
                crate::proxy::providers::cursor_oauth_auth::CursorOAuthManager::new(data_dir);
            manager.get_account(account_id).await.and_then(|account| {
                public_account_label(account.email.as_deref(), None, &account.account_id)
            })
        }
        "antigravity_oauth" => {
            let manager =
                crate::proxy::providers::antigravity_oauth_auth::AntigravityOAuthManager::new(
                    data_dir,
                );
            account_label(&manager.list_accounts().await, account_id)
        }
        _ => None,
    }
}

fn public_account_label(
    email: Option<&str>,
    login: Option<&str>,
    account_id: &str,
) -> Option<String> {
    email
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| login.map(str::trim).filter(|value| !value.is_empty()))
        .map(str::to_string)
        .or_else(|| Some(account_id.to_string()))
}

fn custom_provider_api_url(app: &AppType, provider: &Provider) -> Option<String> {
    let settings = &provider.settings_config;
    let raw = match app {
        AppType::Claude => settings
            .pointer("/env/ANTHROPIC_BASE_URL")
            .and_then(|v| v.as_str())
            .or_else(|| settings.get("base_url").and_then(|v| v.as_str()))
            .or_else(|| settings.get("baseURL").and_then(|v| v.as_str()))
            .or_else(|| settings.get("apiEndpoint").and_then(|v| v.as_str())),
        AppType::Codex => settings
            .get("base_url")
            .and_then(|v| v.as_str())
            .or_else(|| settings.get("baseURL").and_then(|v| v.as_str()))
            .or_else(|| {
                settings
                    .pointer("/config/base_url")
                    .and_then(|v| v.as_str())
            })
            .or_else(|| {
                settings
                    .get("config")
                    .and_then(|v| v.as_str())
                    .and_then(extract_codex_toml_base_url)
            }),
        AppType::Gemini => settings
            .pointer("/env/GOOGLE_GEMINI_BASE_URL")
            .and_then(|v| v.as_str())
            .or_else(|| settings.get("base_url").and_then(|v| v.as_str()))
            .or_else(|| settings.get("baseURL").and_then(|v| v.as_str())),
        _ => None,
    }?;

    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn extract_codex_toml_base_url(config: &str) -> Option<&str> {
    for marker in ["base_url = \"", "base_url = '"] {
        let Some(start) = config.find(marker) else {
            continue;
        };
        let quote = marker.chars().last()?;
        let rest = &config[start + marker.len()..];
        let Some(end) = rest.find(quote) else {
            continue;
        };
        let value = rest[..end].trim();
        if !value.is_empty() {
            return Some(value);
        }
    }
    None
}

fn custom_provider_models(app: &AppType, provider: &Provider) -> Vec<ShareUpstreamModel> {
    match app {
        AppType::Claude => claude_custom_models(provider),
        AppType::Codex => codex_custom_models(provider),
        AppType::Gemini => gemini_custom_models(provider),
        _ => Vec::new(),
    }
}

fn claude_custom_models(provider: &Provider) -> Vec<ShareUpstreamModel> {
    let env = provider.settings_config.get("env");
    [
        ("default", "ANTHROPIC_MODEL"),
        ("haiku", "ANTHROPIC_DEFAULT_HAIKU_MODEL"),
        ("sonnet", "ANTHROPIC_DEFAULT_SONNET_MODEL"),
        ("opus", "ANTHROPIC_DEFAULT_OPUS_MODEL"),
    ]
    .into_iter()
    .filter_map(|(slot, key)| {
        env.and_then(|value| value.get(key))
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|actual_model| ShareUpstreamModel {
                slot: slot.to_string(),
                actual_model: actual_model.to_string(),
            })
    })
    .collect()
}

fn codex_custom_models(provider: &Provider) -> Vec<ShareUpstreamModel> {
    let settings = &provider.settings_config;
    single_model(
        settings
            .get("model")
            .and_then(|value| value.as_str())
            .or_else(|| {
                settings
                    .pointer("/config/model")
                    .and_then(|value| value.as_str())
            })
            .or_else(|| {
                settings
                    .get("config")
                    .and_then(|value| value.as_str())
                    .and_then(extract_codex_toml_model)
            }),
    )
}

fn gemini_custom_models(provider: &Provider) -> Vec<ShareUpstreamModel> {
    let settings = &provider.settings_config;
    single_model(
        settings
            .pointer("/env/GEMINI_MODEL")
            .and_then(|value| value.as_str())
            .or_else(|| {
                settings
                    .pointer("/env/GOOGLE_GEMINI_MODEL")
                    .and_then(|value| value.as_str())
            })
            .or_else(|| settings.get("model").and_then(|value| value.as_str()))
            .or_else(|| {
                settings
                    .pointer("/config/model")
                    .and_then(|value| value.as_str())
            }),
    )
}

fn single_model(model: Option<&str>) -> Vec<ShareUpstreamModel> {
    model
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|actual_model| {
            vec![ShareUpstreamModel {
                slot: "model".to_string(),
                actual_model: actual_model.to_string(),
            }]
        })
        .unwrap_or_default()
}

fn extract_codex_toml_model(config: &str) -> Option<&str> {
    for line in config.lines() {
        let trimmed = line.trim();
        for marker in ["model = \"", "model = '"] {
            let Some(rest) = trimmed.strip_prefix(marker) else {
                continue;
            };
            let quote = marker.chars().last()?;
            let Some(end) = rest.find(quote) else {
                continue;
            };
            let value = rest[..end].trim();
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    None
}

async fn build_official_oauth_snapshot(
    app: &AppType,
    provider: &Provider,
) -> Option<ShareUpstreamProvider> {
    match app {
        AppType::Codex if provider.is_openai_session_provider() => {
            build_openai_session_snapshot(provider).await
        }
        AppType::Codex
            if provider.is_codex_official_with_managed_auth()
                || provider.is_codex_oauth_provider() =>
        {
            build_codex_oauth_snapshot(provider).await
        }
        AppType::Claude if provider.is_claude_oauth_provider() => {
            build_claude_oauth_snapshot(provider).await
        }
        AppType::Claude if provider.is_github_copilot() => {
            build_github_copilot_snapshot(provider).await
        }
        AppType::Gemini
            if provider.is_google_gemini_oauth_provider()
                || provider.is_google_gemini_official_with_managed_auth() =>
        {
            build_gemini_oauth_snapshot(provider).await
        }
        _ => None,
    }
}

async fn build_openai_session_snapshot(provider: &Provider) -> Option<ShareUpstreamProvider> {
    use crate::proxy::providers::openai_session_auth::OpenAISessionManager;

    let manager = OpenAISessionManager::new(crate::config::get_app_config_dir());
    let bound_account_id = provider.meta.as_ref().and_then(|meta| {
        meta.managed_account_id_for("openai_official_session")
            .or_else(|| meta.managed_account_id_for("openai_session"))
    });
    let account_id = match bound_account_id {
        Some(id) if !id.trim().is_empty() => Some(id),
        _ => manager.default_account_id().await,
    };
    let accounts = manager.list_accounts().await;
    let account_email = account_id
        .as_deref()
        .and_then(|id| account_login(&accounts, id));
    let quota = match account_id.as_deref() {
        Some(id) => cached_upstream_quota("openai_official_session", id).await,
        None => None,
    };
    let quota = with_quota_dispatch_limit(quota, provider);

    Some(ShareUpstreamProvider {
        kind: "official_oauth".to_string(),
        app: "codex".to_string(),
        provider_name: Some(provider.name.clone()),
        for_sale_official_price_percent: provider_sale_percent(provider),
        account_email,
        api_url: None,
        quota,
        models: Vec::new(),
    })
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
    let quota = with_quota_dispatch_limit(quota, provider);

    Some(ShareUpstreamProvider {
        kind: "official_oauth".to_string(),
        app: "codex".to_string(),
        provider_name: Some(provider.name.clone()),
        for_sale_official_price_percent: provider_sale_percent(provider),
        account_email,
        api_url: None,
        quota,
        models: Vec::new(),
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
    let quota = with_quota_dispatch_limit(quota, provider);

    Some(ShareUpstreamProvider {
        kind: "official_oauth".to_string(),
        app: "claude".to_string(),
        provider_name: Some(provider.name.clone()),
        for_sale_official_price_percent: provider_sale_percent(provider),
        account_email,
        api_url: None,
        quota,
        models: Vec::new(),
    })
}

async fn build_github_copilot_snapshot(provider: &Provider) -> Option<ShareUpstreamProvider> {
    use crate::proxy::providers::copilot_auth::CopilotAuthManager;

    let manager = CopilotAuthManager::new(crate::config::get_app_config_dir());
    let bound_account_id = provider
        .meta
        .as_ref()
        .and_then(|meta| meta.managed_account_id_for("github_copilot"));
    let account_id = match bound_account_id {
        Some(id) if !id.trim().is_empty() => Some(id),
        _ => manager.default_account_id().await,
    };
    let accounts = manager.list_accounts().await;
    let account_email = account_id
        .as_deref()
        .and_then(|id| account_login(&accounts, id));
    let quota = match account_id.as_deref() {
        Some(id) => cached_upstream_quota("github_copilot", id).await,
        None => None,
    };
    let quota = with_quota_dispatch_limit(quota, provider);

    Some(ShareUpstreamProvider {
        kind: "official_oauth".to_string(),
        app: "claude".to_string(),
        provider_name: Some(provider.name.clone()),
        for_sale_official_price_percent: provider_sale_percent(provider),
        account_email,
        api_url: None,
        quota,
        models: Vec::new(),
    })
}

async fn build_gemini_oauth_snapshot(provider: &Provider) -> Option<ShareUpstreamProvider> {
    use crate::proxy::providers::gemini_oauth_auth::GeminiOAuthManager;

    let manager = GeminiOAuthManager::new(crate::config::get_app_config_dir());
    let bound_account_id = provider
        .meta
        .as_ref()
        .and_then(|meta| meta.managed_account_id_for("google_gemini_oauth"));
    let account_id = match bound_account_id {
        Some(id) if !id.trim().is_empty() => Some(id),
        _ => manager.default_account_id().await,
    };
    let accounts = manager.list_accounts().await;
    let account_email = account_id
        .as_deref()
        .and_then(|id| account_login(&accounts, id));
    let quota = match account_id.as_deref() {
        Some(id) => cached_upstream_quota("google_gemini_oauth", id).await,
        None => None,
    };

    Some(ShareUpstreamProvider {
        kind: "official_oauth".to_string(),
        app: "gemini".to_string(),
        provider_name: Some(provider.name.clone()),
        for_sale_official_price_percent: provider_sale_percent(provider),
        account_email,
        api_url: None,
        quota,
        models: Vec::new(),
    })
}

fn account_login(
    accounts: &[crate::proxy::providers::copilot_auth::GitHubAccount],
    account_id: &str,
) -> Option<String> {
    account_label(accounts, account_id)
}

fn account_label(
    accounts: &[crate::proxy::providers::copilot_auth::GitHubAccount],
    account_id: &str,
) -> Option<String> {
    let account = accounts.iter().find(|account| account.id == account_id)?;
    public_account_label(
        account.email.as_deref(),
        Some(account.login.as_str()),
        &account.id,
    )
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
    let block = crate::services::oauth_quota::quota_block_status(&quota);
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
        plan: quota.credential_message,
        queried_at: quota.queried_at,
        availability: Some(
            block
                .as_ref()
                .map(|item| item.availability.clone())
                .unwrap_or_else(|| "available".to_string()),
        ),
        blocked_until: block.as_ref().and_then(|item| item.blocked_until.clone()),
        blocked_reason: block.as_ref().map(|item| item.blocked_reason.clone()),
        blocked_scope: block.as_ref().map(|item| item.blocked_scope.clone()),
        dispatch_limit_percent: None,
        tiers: quota
            .tiers
            .into_iter()
            .map(|tier| ShareUpstreamQuotaTier {
                label: quota_tier_label(&tier.name),
                utilization: tier.utilization,
                resets_at: tier.resets_at,
                used: tier.used,
                limit: tier.limit,
                unit: tier.unit,
            })
            .collect(),
    }
}

fn quota_tier_label(name: &str) -> String {
    match name {
        "five_hour" => "5h".to_string(),
        "seven_day" => "1w".to_string(),
        "premium" => "premium".to_string(),
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
    let client = share_router_client()?;
    let metadata = share_metadata_from_record(share);
    let identity = identity::ensure_identity(&client, &config)
        .await
        .map_err(|e| e.to_string())?;
    let url = format!("{}/v1/shares/claim-subdomain", config.get_server_addr());
    let claim = metadata.claim_payload();
    let request_payload = build_signed_claim_request_payload(&identity, &claim, &metadata)?;
    let resp = send_share_router_request(
        client.post(&url).json(&request_payload),
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
                "[TunnelSync] share subdomain claim rejected for installation {}, refreshing identity and retrying once: {}",
                identity.installation_id,
                message
            );
            identity::refresh_installation_registration(&client, &config)
                .await
                .map_err(|e| e.to_string())?;
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

pub fn reconcile_share_router_state(db: Arc<Database>) {
    tauri::async_runtime::spawn(async move {
        let shares = match db.list_shares() {
            Ok(shares) => shares,
            Err(err) => {
                log::warn!("[TunnelSync] share router reconcile skipped: {err}");
                return;
            }
        };

        if shares.is_empty() {
            // 没 share：让 router 把所有遗留快照清掉。
            if let Err(err) = enqueue_op(ShareSyncOp::DeleteAll).await {
                log::warn!("[TunnelSync] enqueue share router reconcile delete-all failed: {err}");
            }
            return;
        }

        // 多 share 模式：逐个 Upsert，把 router 端状态对齐成本机当前列表。
        // router 侧靠 share_id 区分，不会因为顺序混乱出错。
        for share in shares {
            let op = ShareSyncOp::Upsert(Box::new(share_metadata_from_record(&share)));
            if let Err(err) = enqueue_op(op).await {
                log::warn!(
                    "[TunnelSync] enqueue share router reconcile upsert for share {} failed: {err}",
                    share.id
                );
            }
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
    let client = share_router_client()?;
    let identity = identity::ensure_identity(&client, &config)
        .await
        .map_err(|e| e.to_string())?;
    let url = format!(
        "{}/v1/share-request-logs/batch-sync",
        config.get_server_addr()
    );
    let request_payload =
        build_signed_request_payload(&identity, "share_request_logs_batch_sync", "logs", &logs)?;
    let resp = send_share_router_request(
        client.post(&url).json(&request_payload),
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
            "[TunnelSync] share request log sync rejected for installation {}, refreshing identity and retrying once: {}",
            identity.installation_id,
            message
        );
        identity::refresh_installation_registration(&client, &config)
            .await
            .map_err(|e| e.to_string())?;
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
    let client = share_router_client()?;
    let identity = identity::ensure_identity(&client, &config)
        .await
        .map_err(|e| e.to_string())?;
    let url = format!("{}/v1/shares/sync", config.get_server_addr());
    let request_payload = build_signed_request_payload(&identity, "share_sync", "share", &share)?;
    let resp = send_share_router_request(
        client.post(&url).json(&request_payload),
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
            "[TunnelSync] direct share sync rejected for installation {}, refreshing identity and retrying once: {}",
            identity.installation_id,
            message
        );
        identity::refresh_installation_registration(&client, &config)
            .await
            .map_err(|e| e.to_string())?;
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
        ShareSyncOp::DeleteAll => "__delete_all__".to_string(),
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
    let client = share_router_client()?;

    let (ops, request_logs) = {
        let state = global_state();
        let mut guard = state.lock().await;
        if guard.pending.is_empty() && guard.pending_request_logs.is_empty() {
            guard.flush_scheduled = false;
            return Ok(());
        }
        let mut ops = guard.pending.drain().map(|(_, op)| op).collect::<Vec<_>>();
        if ops.iter().any(|op| matches!(op, ShareSyncOp::Upsert(_))) {
            ops.retain(|op| !matches!(op, ShareSyncOp::DeleteAll));
        }
        let request_logs = guard
            .pending_request_logs
            .drain()
            .map(|(_, log)| log)
            .collect::<Vec<_>>();
        guard.flush_scheduled = false;
        (ops, request_logs)
    };

    if !ops.is_empty() {
        let identity = identity::ensure_identity(&client, &config)
            .await
            .map_err(|e| e.to_string())?;
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
                ShareSyncOp::DeleteAll => serde_json::json!({
                    "kind": "delete_all",
                }),
            })
            .collect::<Vec<_>>();

        let url = format!("{}/v1/shares/batch-sync", config.get_server_addr());
        let request_payload =
            build_signed_request_payload(&identity, "share_batch_sync", "ops", &payload_ops)?;
        let resp = send_share_router_request(
            client.post(&url).json(&request_payload),
            "batch sync shares",
            &url,
        )
        .await?;
        if !resp.status().is_success() {
            let message = read_error_message(resp).await;
            if allow_identity_reset_retry && identity::should_reset_identity_for_api_error(&message)
            {
                log::warn!(
                    "[TunnelSync] batch share sync rejected for installation {}, refreshing identity and retrying once: {}",
                    identity.installation_id,
                    message
                );
                identity::refresh_installation_registration(&client, &config)
                    .await
                    .map_err(|e| e.to_string())?;
                let state = global_state();
                let mut guard = state.lock().await;
                for op in ops {
                    let key = match &op {
                        ShareSyncOp::Upsert(share) => share.share_id.clone(),
                        ShareSyncOp::Delete { share_id } => share_id.clone(),
                        ShareSyncOp::DeleteAll => "__delete_all__".to_string(),
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
        let identity = identity::ensure_identity(&client, &config)
            .await
            .map_err(|e| e.to_string())?;
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
        let resp = send_share_router_request(
            client.post(&url).json(&request_payload),
            "batch sync share request logs",
            &url,
        )
        .await?;
        if !resp.status().is_success() {
            let message = read_error_message(resp).await;
            if allow_identity_reset_retry && identity::should_reset_identity_for_api_error(&message)
            {
                log::warn!(
                    "[TunnelSync] batch share request log sync rejected for installation {}, refreshing identity and retrying once: {}",
                    identity.installation_id,
                    message
                );
                identity::refresh_installation_registration(&client, &config)
                    .await
                    .map_err(|e| e.to_string())?;
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
    if let Some(domain) = settings.current_share_router_domain() {
        let domain = domain.to_string();
        TunnelConfig { domain }
    } else {
        TunnelConfig::default_public_service()
    }
}

pub(crate) fn share_metadata_from_record(share: &ShareRecord) -> ShareTunnelMetadata {
    // P8 多 app share：router 端 ShareTunnelMetadata.app_type / provider_id 仍是必填，
    // 用"主 app slot"填充（claude > codex > gemini 优先）。其它 slot 的信息走 router
    // 已有的 app_runtimes 通道。完全没有 binding 时 app_type 用空串占位 — 这种 share
    // 不会通过 active 状态推送到 router。
    let primary_app = share.primary_app().unwrap_or_default();
    let primary_pid = share.primary_provider_id();
    // P12：未绑定 provider 的 app 不应该被外部分享。从 bindings 直接派生 support
    // （之前是 Default::default 全 false，要等 runtime snapshot 才修正——但 sync
    // 这一步 router 已经把 enabled_<app> 写进 shares 表了；同步晚到的窗口里 market
    // 可能正好选到一个未绑定 app）。同时把 pricing map 也按 bindings 过滤，避免历史
    // 上设过价但现已解绑的 app 仍带价"漏出去"。
    let support = share_support_from_bindings(share);
    let for_sale_pricing: HashMap<String, u16> = share
        .for_sale_official_price_percent_by_app
        .iter()
        .filter(|(app, _)| share.bindings.contains_key(*app))
        .map(|(app, percent)| (app.clone(), *percent))
        .collect();
    ShareTunnelMetadata {
        share_id: share.id.clone(),
        share_name: share.name.clone(),
        owner_email: share.owner_email.clone(),
        shared_with_emails: share.shared_with_emails.clone(),
        market_access_mode: share.market_access_mode.clone(),
        access_by_app: share.effective_access_by_app(),
        app_settings: share.effective_app_settings(),
        for_sale_official_price_percent_by_app: for_sale_pricing,
        description: share.description.clone(),
        for_sale: share.for_sale.clone(),
        sale_market_kind: share.sale_market_kind.clone(),
        subdomain: share.subdomain.clone().unwrap_or_default(),
        app_type: primary_app,
        provider_id: primary_pid,
        // P9：把全量 slot bindings 也送上去，router dashboard 能渲染 per-slot chip。
        bindings: share.bindings.clone(),
        token_limit: share.token_limit,
        parallel_limit: share.parallel_limit,
        tokens_used: share.tokens_used,
        requests_count: share.requests_count,
        share_status: share.status.clone(),
        auto_start: share.auto_start,
        created_at: share.created_at.clone(),
        expires_at: share.expires_at.clone(),
        support,
        upstream_provider: None,
        app_runtimes: Default::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn provider(settings_config: serde_json::Value) -> Provider {
        Provider {
            id: "provider-id".to_string(),
            name: "Provider".to_string(),
            settings_config,
            website_url: Some("https://website.example".to_string()),
            category: None,
            created_at: None,
            sort_index: None,
            notes: None,
            meta: None,
            icon: None,
            icon_color: None,
            in_failover_queue: false,
        }
    }

    #[test]
    fn extracts_claude_custom_api_url_from_env() {
        let provider = provider(json!({
            "env": {
                "ANTHROPIC_BASE_URL": "https://claude-api.example/v1/"
            }
        }));

        assert_eq!(
            custom_provider_api_url(&AppType::Claude, &provider).as_deref(),
            Some("https://claude-api.example/v1")
        );
    }

    #[test]
    fn extracts_gemini_custom_api_url_from_env() {
        let provider = provider(json!({
            "env": {
                "GOOGLE_GEMINI_BASE_URL": "https://gemini-api.example/v1beta/"
            }
        }));

        assert_eq!(
            custom_provider_api_url(&AppType::Gemini, &provider).as_deref(),
            Some("https://gemini-api.example/v1beta")
        );
    }

    #[test]
    fn extracts_codex_custom_api_url_from_toml() {
        let provider = provider(json!({
            "config": "model_provider = \"custom\"\n[model_providers.custom]\nbase_url = 'https://codex-api.example/v1/'\n"
        }));

        assert_eq!(
            custom_provider_api_url(&AppType::Codex, &provider).as_deref(),
            Some("https://codex-api.example/v1")
        );
    }

    #[test]
    fn subscription_quota_to_upstream_preserves_plan_and_premium_tier() {
        let quota = crate::services::subscription::SubscriptionQuota {
            tool: "github_copilot".to_string(),
            credential_status: crate::services::subscription::CredentialStatus::Valid,
            credential_message: Some("individual".to_string()),
            success: true,
            tiers: vec![crate::services::subscription::QuotaTier {
                name: "premium".to_string(),
                utilization: 12.0,
                resets_at: Some("2026-05-31T00:00:00Z".to_string()),
                used: None,
                limit: None,
                unit: None,
                used_value_usd: None,
                max_value_usd: None,
            }],
            extra_usage: None,
            error: None,
            queried_at: Some(1_774_000_000),
            failure: None,
        };

        let upstream = subscription_quota_to_upstream(quota);

        assert_eq!(upstream.status, "ok");
        assert_eq!(upstream.plan.as_deref(), Some("individual"));
        assert_eq!(upstream.tiers[0].label, "premium");
        assert_eq!(upstream.tiers[0].utilization, 12.0);
        assert_eq!(upstream.tiers[0].used, None);
        assert_eq!(upstream.tiers[0].limit, None);
    }

    /// P11 回归：同一台 client 上的两个 share 各绑不同 provider 时，runtime
    /// snapshot 必须按 share 自己的 bindings 取数据，而不是全局 current
    /// provider。否则 router dashboard 会把同一个 SUPPORT 数据复制到所有 share。
    #[tokio::test]
    async fn build_share_runtime_snapshot_uses_per_share_bindings() {
        use crate::database::Database;
        use crate::provider::{Provider, ProviderMeta};
        use std::collections::HashMap;
        use std::sync::Arc;

        fn make_provider(id: &str, app_type: &str) -> Provider {
            let mut provider = Provider::with_id(
                id.to_string(),
                format!("Provider {id}"),
                serde_json::json!({ "env": {} }),
                Some(String::new()),
            );
            provider.category = Some("custom".to_string());
            provider.meta = Some(ProviderMeta {
                provider_type: Some(app_type.to_string()),
                ..Default::default()
            });
            provider
        }
        fn make_share(id: &str, claude_pid: &str) -> ShareRecord {
            let mut bindings = HashMap::new();
            bindings.insert("claude".to_string(), claude_pid.to_string());
            ShareRecord {
                id: id.to_string(),
                name: id.to_string(),
                owner_email: "u@example.com".to_string(),
                shared_with_emails: Vec::new(),
                market_access_mode: "selected".to_string(),
                access_by_app: HashMap::new(),
                app_settings: HashMap::new(),
                for_sale_official_price_percent_by_app: HashMap::new(),
                description: None,
                for_sale: "No".to_string(),
                sale_market_kind: "token".to_string(),
                bindings,
                dynamic_apps: std::collections::HashSet::new(),
                api_key: String::new(),
                settings_config: None,
                token_limit: -1,
                parallel_limit: 3,
                tokens_used: 0,
                requests_count: 0,
                expires_at: "2100-01-01T00:00:00Z".to_string(),
                subdomain: Some(format!("sub-{id}")),
                tunnel_url: None,
                status: "active".to_string(),
                auto_start: false,
                created_at: "2025-01-01T00:00:00Z".to_string(),
                last_used_at: None,
            }
        }

        let db = Arc::new(Database::memory().expect("memory db"));
        db.save_provider("claude", &make_provider("p-alpha", "claude"))
            .expect("save alpha");
        db.save_provider("claude", &make_provider("p-beta", "claude"))
            .expect("save beta");

        let share_a = make_share("share-a", "p-alpha");
        let share_b = make_share("share-b", "p-beta");

        let snap_a = build_share_runtime_snapshot(&share_a, &db).await;
        let snap_b = build_share_runtime_snapshot(&share_b, &db).await;

        // SUPPORT 必须按 bindings 派生：两条 share 都只绑了 claude。
        assert!(snap_a.support.claude && !snap_a.support.codex && !snap_a.support.gemini);
        assert!(snap_b.support.claude && !snap_b.support.codex && !snap_b.support.gemini);

        // app_runtimes.claude 必须各自命中自己绑定的 provider，不能同源。
        let a_claude = snap_a.app_runtimes.claude.expect("share-a claude runtime");
        let b_claude = snap_b.app_runtimes.claude.expect("share-b claude runtime");
        assert_eq!(a_claude.provider_name.as_deref(), Some("Provider p-alpha"));
        assert_eq!(b_claude.provider_name.as_deref(), Some("Provider p-beta"));

        // app_providers.claude 同时列出两条 provider，但 is_current 只命中 share 自己绑的那条。
        let a_current_ids: Vec<String> = snap_a
            .app_providers
            .claude
            .iter()
            .filter(|p| p.is_current)
            .map(|p| p.id.clone())
            .collect();
        let b_current_ids: Vec<String> = snap_b
            .app_providers
            .claude
            .iter()
            .filter(|p| p.is_current)
            .map(|p| p.id.clone())
            .collect();
        assert_eq!(a_current_ids, vec!["p-alpha"]);
        assert_eq!(b_current_ids, vec!["p-beta"]);

        // Slot 没绑定时，对应 app 的 runtime 必须为空（codex/gemini 没绑定）。
        assert!(snap_a.app_runtimes.codex.is_none());
        assert!(snap_a.app_runtimes.gemini.is_none());
    }

    /// P12 回归：share_metadata_from_record（sync 时写到 router 的 wire payload）
    /// 必须按 bindings 派生 support，且 pricing map 不能把未绑定的 app 漏出去。
    /// 否则 router 在 share 同步刚写入 / 用户解绑后立刻 enable for_sale 这种窗口里，
    /// 会把未绑定的 app 当成"可用"，市场调度命中后才被 client 401 拒。
    #[test]
    fn share_metadata_from_record_respects_unbound_slots() {
        use std::collections::HashMap;

        let mut bindings = HashMap::new();
        bindings.insert("claude".to_string(), "p-claude".to_string());
        // codex / gemini 未绑定

        // 历史定价残留：codex 之前被定过价 80%，现在已解绑。
        let mut pricing = HashMap::new();
        pricing.insert("claude".to_string(), 60u16);
        pricing.insert("codex".to_string(), 80u16);

        let share = ShareRecord {
            id: "s1".to_string(),
            name: "s1".to_string(),
            owner_email: "u@example.com".to_string(),
            shared_with_emails: Vec::new(),
            market_access_mode: "all".to_string(),
            access_by_app: HashMap::new(),
            app_settings: HashMap::new(),
            for_sale_official_price_percent_by_app: pricing,
            description: None,
            for_sale: "Yes".to_string(),
            sale_market_kind: "token".to_string(),
            bindings,
            dynamic_apps: std::collections::HashSet::new(),
            api_key: String::new(),
            settings_config: None,
            token_limit: -1,
            parallel_limit: 3,
            tokens_used: 0,
            requests_count: 0,
            expires_at: "2100-01-01T00:00:00Z".to_string(),
            subdomain: Some("alpha".to_string()),
            tunnel_url: None,
            status: "active".to_string(),
            auto_start: false,
            created_at: "2025-01-01T00:00:00Z".to_string(),
            last_used_at: None,
        };

        let metadata = share_metadata_from_record(&share);

        // SUPPORT 必须按 bindings 派生：未绑定的 codex/gemini 显式 false。
        assert!(metadata.support.claude);
        assert!(!metadata.support.codex);
        assert!(!metadata.support.gemini);

        // Pricing 只能包含已绑定的 app。codex 已解绑，老定价不能漏出去。
        assert_eq!(
            metadata
                .for_sale_official_price_percent_by_app
                .get("claude"),
            Some(&60)
        );
        assert!(!metadata
            .for_sale_official_price_percent_by_app
            .contains_key("codex"));
        assert!(!metadata
            .for_sale_official_price_percent_by_app
            .contains_key("gemini"));
    }
}
