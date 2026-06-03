use crate::database::ShareRecord;
use crate::error::AppError;
use crate::proxy::ProxyConfig;
use crate::services::share::{PrepareShareParams, ShareService};
use crate::store::AppState;
use crate::tunnel::config::{
    ShareTunnelStatus, TunnelConfig, TunnelInfo, TunnelRequest, TunnelType,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tauri::State;
use tokio::net::TcpStream;
use tokio::time::{timeout, Duration};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateShareParams {
    pub owner_email: String,
    /// P8 多 app share：创建时一次性指定 0..3 个 binding（键为 app_type）。
    /// 全空也允许，用户可后续在 UI 里逐个挂 provider。
    #[serde(default)]
    pub bindings: std::collections::HashMap<String, String>,
    pub description: Option<String>,
    pub for_sale: String,
    pub token_limit: i64,
    pub parallel_limit: i64,
    pub expires_in_secs: i64,
    pub subdomain: Option<String>,
    #[serde(default)]
    pub auto_start: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicMarket {
    pub id: String,
    pub display_name: String,
    pub email: String,
    pub subdomain: String,
    pub public_base_url: String,
    pub status: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MarketsResponse {
    markets: Vec<PublicMarket>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateShareTokenLimitParams {
    pub share_id: String,
    pub token_limit: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateShareParallelLimitParams {
    pub share_id: String,
    pub parallel_limit: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateShareSubdomainParams {
    pub share_id: String,
    pub subdomain: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateShareDescriptionParams {
    pub share_id: String,
    pub description: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateShareForSaleParams {
    pub share_id: String,
    pub for_sale: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateShareForSaleOfficialPricePercentParams {
    pub share_id: String,
    pub pricing: HashMap<String, u16>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateShareExpirationParams {
    pub share_id: String,
    pub expires_at: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateShareAutoStartParams {
    pub share_id: String,
    pub auto_start: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateShareOwnerEmailParams {
    pub share_id: String,
    pub owner_email: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferShareOwnerParams {
    pub share_id: String,
    pub target_email: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateShareAclParams {
    pub share_id: String,
    pub shared_with_emails: Vec<String>,
    #[serde(default = "default_market_access_mode")]
    pub market_access_mode: String,
}

fn default_market_access_mode() -> String {
    "selected".to_string()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectInfo {
    pub tunnel_url: String,
    pub subdomain: String,
}

#[tauri::command]
pub async fn create_share(
    state: State<'_, AppState>,
    params: CreateShareParams,
) -> Result<ShareRecord, String> {
    let owner_email = normalize_owner_email(&params.owner_email)?;
    let requested_subdomain = params.subdomain.clone();
    let mut last_claim_error = None;
    let mut share = None;

    for _ in 0..5 {
        let candidate = ShareService::prepare_create(
            &state.db,
            PrepareShareParams {
                owner_email: owner_email.clone(),
                bindings: params.bindings.clone(),
                description: params.description.clone(),
                for_sale: params.for_sale.clone(),
                token_limit: params.token_limit,
                parallel_limit: params.parallel_limit,
                expires_in_secs: params.expires_in_secs,
                subdomain: requested_subdomain.clone(),
                auto_start: params.auto_start,
            },
        )
        .map_err(|e: AppError| e.to_string())?;

        match crate::tunnel::sync::claim_share_subdomain(&candidate, &state.db).await {
            Ok(()) => {
                share = Some(candidate);
                break;
            }
            Err(err)
                if requested_subdomain.is_none() && err.contains("subdomain already claimed") =>
            {
                last_claim_error = Some(err);
                continue;
            }
            Err(err) => {
                return Err(crate::email_auth::humanize_remote_owner_binding_error(&err));
            }
        }
    }

    let share = share.ok_or_else(|| {
        crate::email_auth::humanize_remote_owner_binding_error(
            &last_claim_error
                .unwrap_or_else(|| "unable to allocate an available subdomain".to_string()),
        )
    })?;
    ShareService::create(&state.db, share).map_err(|e: AppError| e.to_string())
}

#[tauri::command]
pub async fn list_share_markets() -> Result<Vec<PublicMarket>, String> {
    let config = current_tunnel_config();
    let url = format!("{}/v1/markets", config.get_server_addr());
    let response = reqwest::Client::new()
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("获取 market 列表失败: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("获取 market 列表失败: HTTP {}", response.status()));
    }
    let body = response
        .json::<MarketsResponse>()
        .await
        .map_err(|e| format!("解析 market 列表失败: {e}"))?;
    Ok(body.markets)
}

#[tauri::command]
pub fn authorize_share_market(
    state: State<'_, AppState>,
    share_id: String,
    market_email: String,
) -> Result<ShareRecord, String> {
    let share = ShareService::get_detail(&state.db, &share_id)
        .map_err(|e: AppError| e.to_string())?
        .ok_or_else(|| format!("Share not found: {share_id}"))?;
    let owner_email = share.owner_email.clone();
    let mut emails = share.shared_with_emails;
    emails.push(market_email);
    ShareService::update_acl(
        &state.db,
        &share_id,
        &owner_email,
        emails,
        &share.market_access_mode,
    )
    .map_err(|e: AppError| e.to_string())
}

#[tauri::command]
pub fn update_share_acl(
    state: State<'_, AppState>,
    params: UpdateShareAclParams,
) -> Result<ShareRecord, String> {
    let share = ShareService::get_detail(&state.db, &params.share_id)
        .map_err(|e: AppError| e.to_string())?
        .ok_or_else(|| format!("Share not found: {}", params.share_id))?;
    let owner_email = share.owner_email.clone();
    ShareService::update_acl(
        &state.db,
        &params.share_id,
        &owner_email,
        params.shared_with_emails,
        &params.market_access_mode,
    )
    .map_err(|e: AppError| e.to_string())
}

#[tauri::command]
pub async fn delete_share(state: State<'_, AppState>, share_id: String) -> Result<(), String> {
    // Stop any running tunnel for this share before removing the DB row,
    // otherwise the public portr forward stays alive until app shutdown.
    {
        let mut mgr = state.tunnel_manager.write().await;
        if mgr.get_info(&share_id).is_some() {
            if let Err(e) = mgr.stop_tunnel(&share_id).await {
                log::warn!("[Share] 停止隧道失败（将继续删除）: {e}");
            }
        }
    }
    ShareService::delete(&state.db, &share_id).map_err(|e: AppError| e.to_string())?;
    crate::tunnel::sync::schedule_delete_share(share_id);
    Ok(())
}

#[tauri::command]
pub fn pause_share(state: State<'_, AppState>, share_id: String) -> Result<(), String> {
    ShareService::pause(&state.db, &share_id).map_err(|e: AppError| e.to_string())
}

#[tauri::command]
pub fn resume_share(state: State<'_, AppState>, share_id: String) -> Result<(), String> {
    ShareService::resume(&state.db, &share_id).map_err(|e: AppError| e.to_string())
}

#[tauri::command]
pub fn reset_share_usage(
    state: State<'_, AppState>,
    share_id: String,
) -> Result<ShareRecord, String> {
    ShareService::reset_usage(&state.db, &share_id).map_err(|e: AppError| e.to_string())
}

#[tauri::command]
pub fn update_share_token_limit(
    state: State<'_, AppState>,
    params: UpdateShareTokenLimitParams,
) -> Result<ShareRecord, String> {
    ShareService::update_token_limit(&state.db, &params.share_id, params.token_limit)
        .map_err(|e: AppError| e.to_string())
}

#[tauri::command]
pub fn update_share_parallel_limit(
    state: State<'_, AppState>,
    params: UpdateShareParallelLimitParams,
) -> Result<ShareRecord, String> {
    ShareService::update_parallel_limit(&state.db, &params.share_id, params.parallel_limit)
        .map_err(|e: AppError| e.to_string())
}

#[tauri::command]
pub fn update_share_description(
    state: State<'_, AppState>,
    params: UpdateShareDescriptionParams,
) -> Result<ShareRecord, String> {
    ShareService::update_description(&state.db, &params.share_id, params.description)
        .map_err(|e: AppError| e.to_string())
}

#[tauri::command]
pub fn update_share_for_sale(
    state: State<'_, AppState>,
    params: UpdateShareForSaleParams,
) -> Result<ShareRecord, String> {
    ShareService::update_for_sale(&state.db, &params.share_id, &params.for_sale)
        .map_err(|e: AppError| e.to_string())
}

#[tauri::command]
pub fn update_share_for_sale_official_price_percent(
    state: State<'_, AppState>,
    params: UpdateShareForSaleOfficialPricePercentParams,
) -> Result<ShareRecord, String> {
    ShareService::update_for_sale_official_price_percent_by_app(
        &state.db,
        &params.share_id,
        params.pricing,
    )
    .map_err(|e: AppError| e.to_string())
}

#[tauri::command]
pub fn update_share_expiration(
    state: State<'_, AppState>,
    params: UpdateShareExpirationParams,
) -> Result<ShareRecord, String> {
    ShareService::update_expires_at(&state.db, &params.share_id, &params.expires_at)
        .map_err(|e: AppError| e.to_string())
}

#[tauri::command]
pub fn update_share_auto_start(
    state: State<'_, AppState>,
    params: UpdateShareAutoStartParams,
) -> Result<ShareRecord, String> {
    ShareService::update_auto_start(&state.db, &params.share_id, params.auto_start)
        .map_err(|e: AppError| e.to_string())
}

#[tauri::command]
pub fn update_share_owner_email(
    state: State<'_, AppState>,
    params: UpdateShareOwnerEmailParams,
) -> Result<ShareRecord, String> {
    ShareService::update_owner_email(&state.db, &params.share_id, &params.owner_email)
        .map_err(|e: AppError| e.to_string())
}

#[tauri::command]
pub fn transfer_share_owner(
    state: State<'_, AppState>,
    params: TransferShareOwnerParams,
) -> Result<ShareRecord, String> {
    ShareService::transfer_owner_email(&state.db, &params.share_id, &params.target_email)
        .map_err(|e: AppError| e.to_string())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateShareProviderBindingParams {
    pub share_id: String,
    /// 目标 slot 的 app_type（claude / codex / gemini）。
    pub app_type: String,
    /// 新 provider id。`None`（或省略）表示清空该 slot（解绑）。
    #[serde(default)]
    pub provider_id: Option<String>,
}

/// P8 多 app share：改绑 / 新增 / 清空 share 在某个 app_type slot 上的 provider 绑定。
///
/// 要求 share 当前 status == paused，避免请求路径取到不一致的中间态（schema 的
/// UNIQUE(provider_id) 索引和 ShareService 内的乐观锁 CAS 是补充防御）。成功后
/// schedule_sync_share 会把新 bindings 推送到 router。
#[tauri::command]
pub async fn update_share_provider_binding(
    state: State<'_, AppState>,
    params: UpdateShareProviderBindingParams,
) -> Result<ShareRecord, String> {
    ShareService::update_provider_binding(
        &state.db,
        &params.share_id,
        &params.app_type,
        params.provider_id.as_deref(),
    )
    .map_err(|e: AppError| e.to_string())
}

/// 轮换 share_token。返回带新 token 的 ShareRecord。
#[tauri::command]
pub async fn rotate_share_token(
    state: State<'_, AppState>,
    share_id: String,
) -> Result<ShareRecord, String> {
    ShareService::rotate_token(&state.db, &share_id).map_err(|e: AppError| e.to_string())
}

/// 取 share 改绑历史（最近 N 条）。
#[tauri::command]
pub async fn list_share_binding_history(
    state: State<'_, AppState>,
    share_id: String,
    limit: Option<usize>,
) -> Result<Vec<crate::database::ShareBindingHistoryEntry>, String> {
    ShareService::list_binding_history(&state.db, &share_id, limit.unwrap_or(20))
        .map_err(|e: AppError| e.to_string())
}

/// A-4：导出当前所有 share 配置（JSON）。
/// 不包含 share_token / api_key 以外的敏感字段；token 仍然要包含因为换设备后
/// 用户希望接入方零改造。
#[tauri::command]
pub async fn export_all_shares(state: State<'_, AppState>) -> Result<Vec<ShareRecord>, String> {
    ShareService::list(&state.db).map_err(|e: AppError| e.to_string())
}

/// A-4：批量导入 share 配置。当本机已有同 id share 时跳过；
/// provider_id 在新机器上可能不存在，跳过那些并把 share id 收集回报。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportSharesResult {
    pub imported: usize,
    pub skipped_existing: Vec<String>,
    pub skipped_provider_missing: Vec<String>,
}

#[tauri::command]
pub async fn import_shares(
    state: State<'_, AppState>,
    shares: Vec<ShareRecord>,
) -> Result<ImportSharesResult, String> {
    let mut imported = 0;
    let mut skipped_existing = Vec::new();
    let mut skipped_provider_missing = Vec::new();
    for share in shares {
        if state
            .db
            .get_share_by_id(&share.id)
            .map_err(|e| e.to_string())?
            .is_some()
        {
            skipped_existing.push(share.id);
            continue;
        }
        // P8 多 app share：share 携带 0..3 个 binding；每个 binding 的 provider
        // 必须在本机存在且 app_type 匹配。任一缺失就 skip 这条 share。
        let mut all_providers_present = true;
        for (app_type, provider_id) in &share.bindings {
            let exists = state
                .db
                .get_provider_by_id(provider_id, app_type)
                .map_err(|e| e.to_string())?
                .is_some();
            if !exists {
                all_providers_present = false;
                break;
            }
        }
        if !all_providers_present {
            skipped_provider_missing.push(share.id);
            continue;
        }
        state
            .db
            .create_share(&share)
            .map_err(|e: AppError| e.to_string())?;
        imported += 1;
    }
    Ok(ImportSharesResult {
        imported,
        skipped_existing,
        skipped_provider_missing,
    })
}

#[tauri::command]
pub async fn update_share_subdomain(
    state: State<'_, AppState>,
    params: UpdateShareSubdomainParams,
) -> Result<ShareRecord, String> {
    let share = ShareService::get_detail(&state.db, &params.share_id)
        .map_err(|e: AppError| e.to_string())?
        .ok_or_else(|| format!("Share not found: {}", params.share_id))?;
    let requested_subdomain = params.subdomain;
    let mut next = share.clone();
    next.subdomain = Some(requested_subdomain.clone());

    crate::tunnel::sync::claim_share_subdomain(&next, &state.db)
        .await
        .map_err(|e| crate::email_auth::humanize_remote_owner_binding_error(&e))?;

    {
        let mut mgr = state.tunnel_manager.write().await;
        if mgr.get_info(&params.share_id).is_some() {
            mgr.stop_tunnel(&params.share_id)
                .await
                .map_err(|e| e.to_string())?;
        }
    }

    let updated = ShareService::update_subdomain(&state.db, &params.share_id, &requested_subdomain)
        .map_err(|e: AppError| e.to_string())?;

    if updated.status == "active" {
        return start_share_tunnel_with_error_tracking(state.inner(), &params.share_id)
            .await
            .map(|_| updated.clone())
            .map_err(|e| e.to_string());
    }

    Ok(updated)
}

#[tauri::command]
pub async fn enable_share(
    state: State<'_, AppState>,
    share_id: String,
) -> Result<TunnelInfo, String> {
    ShareService::resume(&state.db, &share_id).map_err(|e: AppError| e.to_string())?;
    start_share_tunnel_with_error_tracking(state.inner(), &share_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn disable_share(state: State<'_, AppState>, share_id: String) -> Result<(), String> {
    state
        .db
        .clear_share_tunnel(&share_id)
        .map_err(|e: AppError| e.to_string())?;
    ShareService::pause(&state.db, &share_id).map_err(|e: AppError| e.to_string())?;
    state
        .db
        .update_share_auto_start(&share_id, false)
        .map_err(|e: AppError| e.to_string())?;

    if let Ok(Some(share)) = state.db.get_share_by_id(&share_id) {
        let metadata = crate::tunnel::sync::share_metadata_from_record(&share);
        if let Err(err) = crate::tunnel::sync::sync_share_metadata_now(metadata).await {
            log::warn!(
                "[Share] immediate remote sync after disable failed for {}: {}",
                share_id,
                err
            );
        }
    }

    {
        let mut mgr = state.tunnel_manager.write().await;
        if mgr.get_info(&share_id).is_some() {
            match timeout(Duration::from_secs(5), mgr.stop_tunnel(&share_id)).await {
                Ok(Ok(())) => {}
                Ok(Err(err)) => {
                    log::warn!("[Share] stop tunnel after disable failed for {share_id}: {err}");
                }
                Err(_) => {
                    log::warn!("[Share] stop tunnel after disable timed out for {share_id}");
                }
            }
        }
    }

    Ok(())
}

#[tauri::command]
pub fn list_shares(state: State<'_, AppState>) -> Result<Vec<ShareRecord>, String> {
    ShareService::list(&state.db).map_err(|e: AppError| e.to_string())
}

#[tauri::command]
pub fn get_share_detail(
    state: State<'_, AppState>,
    share_id: String,
) -> Result<Option<ShareRecord>, String> {
    ShareService::get_detail(&state.db, &share_id).map_err(|e: AppError| e.to_string())
}

#[tauri::command]
pub async fn start_share_tunnel(
    state: State<'_, AppState>,
    share_id: String,
) -> Result<TunnelInfo, String> {
    start_share_tunnel_with_error_tracking(state.inner(), &share_id)
        .await
        .map_err(|e| e.to_string())
}

pub async fn restore_active_share_tunnel(state: &AppState) -> Result<(), AppError> {
    for share in ShareService::list(&state.db)?
        .into_iter()
        .filter(|share| share.status == "active" || share.auto_start)
    {
        if share.auto_start && share.status != "active" {
            state.db.update_share_status(&share.id, "active")?;
        }
        let already_running = {
            let mgr = state.tunnel_manager.read().await;
            mgr.get_info(&share.id).is_some()
        };
        if already_running {
            continue;
        }

        log::info!(
            "[Share] Restoring active share tunnel for share_id={}",
            share.id
        );
        if let Err(err) = start_share_tunnel_with_error_tracking(state, &share.id).await {
            log::warn!(
                "[Share] Failed to restore active share tunnel for share_id={}: {}",
                share.id,
                err
            );
        }
    }

    Ok(())
}

async fn start_share_tunnel_with_error_tracking(
    state: &AppState,
    share_id: &str,
) -> Result<TunnelInfo, AppError> {
    match start_share_tunnel_inner(state, share_id).await {
        Ok(info) => {
            state
                .tunnel_manager
                .write()
                .await
                .clear_last_error(share_id);
            Ok(info)
        }
        Err(err) => {
            state
                .tunnel_manager
                .write()
                .await
                .set_last_error(share_id, err.to_string());
            Err(err)
        }
    }
}

async fn start_share_tunnel_inner(
    state: &AppState,
    share_id: &str,
) -> Result<TunnelInfo, AppError> {
    let share = ShareService::get_detail(&state.db, share_id)?
        .ok_or_else(|| AppError::Message(format!("Share not found: {share_id}")))?;

    let subdomain = share
        .subdomain
        .clone()
        .unwrap_or_else(|| format!("share-{}", &share.id[..8]));
    crate::tunnel::sync::claim_share_subdomain(&share, &state.db)
        .await
        .map_err(|e| {
            AppError::Message(crate::email_auth::humanize_remote_owner_binding_error(&e))
        })?;

    let local_addr = current_proxy_local_addr(state).await?;
    ensure_proxy_reachable(&local_addr).await?;

    let mut share_metadata = crate::tunnel::sync::share_metadata_from_record(&share);
    share_metadata.subdomain = subdomain.clone();

    let req = TunnelRequest {
        tunnel_type: TunnelType::Http,
        subdomain: subdomain.clone(),
        local_addr,
        share_metadata: Some(share_metadata),
    };

    let mut mgr = state.tunnel_manager.write().await;
    let info = mgr
        .start_tunnel(share_id, req, state.db.clone())
        .await
        .map_err(|e| AppError::Message(e.to_string()))?;

    // Update share with tunnel info
    state
        .db
        .update_share_tunnel(share_id, &info.subdomain, &info.tunnel_url)?;

    if let Err(e) =
        crate::tunnel::sync::sync_recent_share_request_logs(&state.db, share_id, 50).await
    {
        log::warn!(
            "[Share] Failed to backfill recent request logs for share {}: {}",
            share_id,
            e
        );
    }

    Ok(info)
}

pub(crate) fn requires_owner_login_for_web(message: &str) -> bool {
    requires_owner_login_for_error(message)
}

fn requires_owner_login_for_error(message: &str) -> bool {
    message.contains("当前设备身份已失效")
        || message.contains("当前邮箱验证码登录凭证已过期")
        || message.contains("请重新发送并验证邮箱验证码")
        || message.contains("请重新登录")
        || message.contains("请先完成邮箱验证码登录")
        || message.contains("当前邮箱登录状态与 share owner 不一致")
        || message.contains("当前邮箱登录所属分享节点与所选分享节点不一致")
}

#[tauri::command]
pub async fn stop_share_tunnel(state: State<'_, AppState>, share_id: String) -> Result<(), String> {
    let mut mgr = state.tunnel_manager.write().await;
    mgr.stop_tunnel(&share_id)
        .await
        .map_err(|e| e.to_string())?;
    drop(mgr);
    state
        .db
        .clear_share_tunnel(&share_id)
        .map_err(|e: AppError| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn get_tunnel_status(
    state: State<'_, AppState>,
    share_id: String,
) -> Result<ShareTunnelStatus, String> {
    let mgr = state.tunnel_manager.read().await;
    let info = mgr.get_info(&share_id);
    let last_error = mgr.get_last_error(&share_id);
    Ok(ShareTunnelStatus {
        requires_owner_login: last_error
            .as_deref()
            .map(requires_owner_login_for_error)
            .unwrap_or(false),
        info,
        last_error,
    })
}

#[tauri::command]
pub fn get_share_connect_info(
    state: State<'_, AppState>,
    share_id: String,
) -> Result<ConnectInfo, String> {
    let share = ShareService::get_detail(&state.db, &share_id)
        .map_err(|e: AppError| e.to_string())?
        .ok_or_else(|| format!("Share not found: {share_id}"))?;
    let config = current_tunnel_config();
    let subdomain = share
        .subdomain
        .clone()
        .unwrap_or_else(|| format!("share-{}", &share.id[..8]));
    let tunnel_url = share
        .tunnel_url
        .clone()
        .unwrap_or_else(|| config.get_tunnel_addr(&subdomain));

    Ok(ConnectInfo {
        tunnel_url,
        subdomain,
    })
}

fn normalize_owner_email(email: &str) -> Result<String, String> {
    let email = email.trim().to_ascii_lowercase();
    if email.is_empty() || !email.contains('@') || email.len() > 254 {
        return Err("邮箱格式无效".to_string());
    }
    Ok(email)
}

#[tauri::command]
pub async fn configure_tunnel(
    state: State<'_, AppState>,
    config: TunnelConfig,
) -> Result<(), String> {
    // 持久化到 AppSettings，确保应用重启后依然可用
    let mut settings = crate::settings::get_settings();
    settings.set_share_router_domain(Some(config.domain.clone()));
    crate::settings::update_settings(settings).map_err(|e| e.to_string())?;

    let mut mgr = state.tunnel_manager.write().await;
    mgr.set_config(config);
    Ok(())
}

fn current_tunnel_config() -> TunnelConfig {
    let settings = crate::settings::get_settings();
    if let Some(domain) = settings.current_share_router_domain() {
        let domain = domain.to_string();
        TunnelConfig { domain }
    } else {
        TunnelConfig::default_public_service()
    }
}

async fn current_proxy_local_addr(state: &AppState) -> Result<String, AppError> {
    let config = state.db.get_proxy_config().await?;
    Ok(proxy_local_addr_from_config(&config))
}

async fn ensure_proxy_reachable(local_addr: &str) -> Result<(), AppError> {
    timeout(Duration::from_secs(2), TcpStream::connect(local_addr))
        .await
        .map_err(|_| {
            AppError::Message(format!(
                "本地代理服务不可达：{}。请先确认 cc-switch 代理已启动，并且正在监听当前配置端口。",
                local_addr
            ))
        })?
        .map(|_| ())
        .map_err(|err| {
            AppError::Message(format!(
                "本地代理服务不可达：{} ({})。请先确认 cc-switch 代理已启动，并且正在监听当前配置端口。",
                local_addr, err
            ))
        })
}

fn proxy_local_addr_from_config(config: &ProxyConfig) -> String {
    let connect_host = match config.listen_address.as_str() {
        "0.0.0.0" | "::" | "[::]" => "127.0.0.1",
        _ => config.listen_address.as_str(),
    };
    format!("{connect_host}:{}", config.listen_port)
}
