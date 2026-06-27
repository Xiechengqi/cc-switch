use crate::database::{ShareAppAccess, ShareAppSettings, ShareRecord};
use crate::error::AppError;
use crate::proxy::ProxyConfig;
use crate::services::share::{PrepareShareParams, ShareService};
use crate::store::AppState;
use crate::tunnel::config::{
    ShareTunnelStatus, TunnelConfig, TunnelInfo, TunnelRequest, TunnelType,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
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
    /// P17 动态绑定：列表内的 app 在创建时跟随当前激活的 provider；之后用户
    /// 在 cc-switch 里切换该 app 的 provider，本 share 的绑定会同步过去。
    /// 与 `bindings` 互斥（同一 app 不能同时出现）。
    #[serde(default)]
    pub dynamic_apps: Vec<String>,
    pub description: Option<String>,
    pub for_sale: String,
    #[serde(default = "default_sale_market_kind")]
    pub sale_market_kind: String,
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
    #[serde(alias = "display_name")]
    pub display_name: String,
    pub email: String,
    pub subdomain: String,
    #[serde(alias = "public_base_url")]
    pub public_base_url: String,
    #[serde(default = "default_market_kind")]
    #[serde(alias = "market_kind")]
    pub market_kind: String,
    pub status: String,
}

fn default_market_kind() -> String {
    "usage".to_string()
}

fn default_sale_market_kind() -> String {
    "token".to_string()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MarketsResponse {
    markets: Vec<PublicMarket>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum MarketsPayload {
    Wrapped(MarketsResponse),
    List(Vec<PublicMarket>),
}

impl MarketsPayload {
    fn into_markets(self) -> Vec<PublicMarket> {
        match self {
            MarketsPayload::Wrapped(response) => response.markets,
            MarketsPayload::List(markets) => markets,
        }
    }
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
    #[serde(default)]
    pub access_by_app: Option<std::collections::HashMap<String, ShareAppAccess>>,
    #[serde(default)]
    pub app_settings: Option<std::collections::HashMap<String, ShareAppSettings>>,
    #[serde(default)]
    pub sale_market_kind: Option<String>,
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

const CLIENT_TUNNEL_ID: &str = "__client_web__";
pub(crate) const WEB_CLIENT_TUNNEL_ID: &str = CLIENT_TUNNEL_ID;

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ClientTunnelSettingsView {
    pub owner_email: String,
    pub subdomain: String,
    pub enabled: bool,
    pub auto_start: bool,
    pub tunnel_url: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientTunnelState {
    pub config: ClientTunnelSettingsView,
    pub status: ShareTunnelStatus,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientTunnelUpdateParams {
    pub owner_email: String,
    pub subdomain: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub auto_start: bool,
}

fn default_true() -> bool {
    true
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
                dynamic_apps: params.dynamic_apps.iter().cloned().collect(),
                description: params.description.clone(),
                for_sale: params.for_sale.clone(),
                sale_market_kind: params.sale_market_kind.clone(),
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
    fetch_public_markets().await
}

async fn fetch_public_markets() -> Result<Vec<PublicMarket>, String> {
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
        .json::<MarketsPayload>()
        .await
        .map_err(|e| format!("解析 market 列表失败: {e}"))?;
    Ok(body.into_markets())
}

#[tauri::command]
pub async fn authorize_share_market(
    state: State<'_, AppState>,
    share_id: String,
    market_email: String,
) -> Result<ShareRecord, String> {
    let market_email = market_email.trim().to_ascii_lowercase();
    let markets = fetch_public_markets().await?;
    let public_market_emails = markets
        .iter()
        .map(|market| market.email.trim().to_ascii_lowercase())
        .collect::<HashSet<_>>();
    let share_market_emails = markets
        .iter()
        .filter(|market| market.market_kind == "share")
        .map(|market| market.email.trim().to_ascii_lowercase())
        .collect::<HashSet<_>>();
    if !share_market_emails.contains(&market_email) {
        return Err("只能委托给已注册的 share market".to_string());
    }
    let share = ShareService::get_detail(&state.db, &share_id)
        .map_err(|e: AppError| e.to_string())?
        .ok_or_else(|| format!("Share not found: {share_id}"))?;
    let owner_email = share.owner_email.clone();
    let access_by_app = replace_public_market_acl_with_share_market(
        share.effective_access_by_app(),
        &market_email,
        &public_market_emails,
    );
    ShareService::update_acl(
        &state.db,
        &share_id,
        &owner_email,
        Vec::new(),
        "selected",
        Some(access_by_app),
        Some("share"),
    )
    .map_err(|e: AppError| e.to_string())
}

fn replace_public_market_acl_with_share_market(
    mut access_by_app: HashMap<String, ShareAppAccess>,
    market_email: &str,
    public_market_emails: &HashSet<String>,
) -> HashMap<String, ShareAppAccess> {
    for access in access_by_app.values_mut() {
        access
            .shared_with_emails
            .retain(|email| !public_market_emails.contains(&email.trim().to_ascii_lowercase()));
        if !access
            .shared_with_emails
            .iter()
            .any(|email| email.eq_ignore_ascii_case(market_email))
        {
            access.shared_with_emails.push(market_email.to_string());
        }
    }
    if access_by_app.is_empty() {
        access_by_app.insert(
            "claude".to_string(),
            ShareAppAccess {
                shared_with_emails: vec![market_email.to_string()],
                market_access_mode: "selected".to_string(),
            },
        );
    }
    access_by_app
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
    let updated = ShareService::update_acl(
        &state.db,
        &params.share_id,
        &owner_email,
        params.shared_with_emails,
        &params.market_access_mode,
        params.access_by_app,
        params.sale_market_kind.as_deref(),
    )
    .map_err(|e: AppError| e.to_string())?;
    if let Some(app_settings) = params.app_settings {
        ShareService::update_app_settings(&state.db, &params.share_id, app_settings)
            .map_err(|e: AppError| e.to_string())
    } else {
        Ok(updated)
    }
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
    /// true 表示动态绑定当前 app 激活的 provider。
    #[serde(default)]
    pub dynamic: bool,
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
        params.dynamic,
    )
    .map_err(|e: AppError| e.to_string())
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
/// share_token 已移除（router 边界用 user_api_token 校验，client 不再持有该字段），
/// api_key 也是历史遗留死字段；导出主要用于换机迁移 share 元数据 + bindings 关系。
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
        .filter(should_restore_share_tunnel)
    {
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

fn should_restore_share_tunnel(share: &ShareRecord) -> bool {
    share.status == "active"
}

pub(crate) async fn start_share_tunnel_with_error_tracking(
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
pub async fn get_client_tunnel(state: State<'_, AppState>) -> Result<ClientTunnelState, String> {
    let config = load_or_default_client_tunnel_config().await?;
    let status = client_tunnel_status(state.inner()).await;
    Ok(ClientTunnelState { config, status })
}

#[tauri::command]
pub async fn claim_client_tunnel(
    state: State<'_, AppState>,
    params: ClientTunnelUpdateParams,
) -> Result<ClientTunnelState, String> {
    write_client_tunnel_config(state.inner(), params, true).await
}

#[tauri::command]
pub async fn update_client_tunnel(
    state: State<'_, AppState>,
    params: ClientTunnelUpdateParams,
) -> Result<ClientTunnelState, String> {
    write_client_tunnel_config(state.inner(), params, false).await
}

#[tauri::command]
pub async fn start_client_tunnel(state: State<'_, AppState>) -> Result<TunnelInfo, String> {
    start_client_tunnel_with_error_tracking(state.inner())
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn stop_client_tunnel(state: State<'_, AppState>) -> Result<(), String> {
    let mut mgr = state.tunnel_manager.write().await;
    if mgr.get_info(CLIENT_TUNNEL_ID).is_some() {
        mgr.stop_tunnel(CLIENT_TUNNEL_ID)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub async fn get_client_tunnel_status(
    state: State<'_, AppState>,
) -> Result<ShareTunnelStatus, String> {
    Ok(client_tunnel_status(state.inner()).await)
}

pub async fn restore_client_tunnel(state: &AppState) -> Result<(), AppError> {
    let settings = crate::settings::get_settings();
    let Some(config) = settings.client_tunnel else {
        return Ok(());
    };
    if !config.enabled || !config.auto_start {
        return Ok(());
    }
    let already_running = {
        let mgr = state.tunnel_manager.read().await;
        mgr.get_info(CLIENT_TUNNEL_ID).is_some()
    };
    if already_running {
        return Ok(());
    }
    if let Err(err) = start_client_tunnel_with_error_tracking(state).await {
        log::warn!("[Share] Failed to restore client tunnel: {err}");
    }
    Ok(())
}

pub(crate) async fn write_client_tunnel_config(
    state: &AppState,
    params: ClientTunnelUpdateParams,
    claim: bool,
) -> Result<ClientTunnelState, String> {
    let owner_email = normalize_owner_email(&params.owner_email)?;
    let subdomain = normalize_subdomain(&params.subdomain)?;
    let local_config = ClientTunnelSettingsView {
        owner_email: owner_email.clone(),
        subdomain: subdomain.clone(),
        enabled: params.enabled,
        auto_start: params.auto_start,
        tunnel_url: Some(current_tunnel_config().get_tunnel_addr(&subdomain)),
    };
    let router_config = current_tunnel_config();
    let http_client = reqwest::Client::new();
    let remote_claim = crate::tunnel::connection::ClientTunnelClaim {
        owner_email: owner_email.clone(),
        subdomain: subdomain.clone(),
        enabled: params.enabled,
    };
    let remote = if claim {
        crate::tunnel::connection::claim_client_tunnel(&http_client, &router_config, &remote_claim)
            .await
    } else {
        crate::tunnel::connection::update_client_tunnel(&http_client, &router_config, &remote_claim)
            .await
    }
    .map_err(|e| crate::email_auth::humanize_remote_owner_binding_error(&e.to_string()))?;

    let saved = ClientTunnelSettingsView {
        tunnel_url: Some(remote.tunnel_url),
        ..local_config
    };
    save_client_tunnel_settings(&saved)?;

    if saved.enabled && saved.auto_start {
        if let Err(err) = start_client_tunnel_with_error_tracking(state).await {
            log::warn!("[Share] start client tunnel after save failed: {err}");
        }
    } else {
        let mut mgr = state.tunnel_manager.write().await;
        if mgr.get_info(CLIENT_TUNNEL_ID).is_some() {
            let _ = mgr.stop_tunnel(CLIENT_TUNNEL_ID).await;
        }
    }

    Ok(ClientTunnelState {
        config: saved,
        status: client_tunnel_status(state).await,
    })
}

pub(crate) async fn start_client_tunnel_with_error_tracking(
    state: &AppState,
) -> Result<TunnelInfo, AppError> {
    match start_client_tunnel_inner(state).await {
        Ok(info) => {
            state
                .tunnel_manager
                .write()
                .await
                .clear_last_error(CLIENT_TUNNEL_ID);
            Ok(info)
        }
        Err(err) => {
            state
                .tunnel_manager
                .write()
                .await
                .set_last_error(CLIENT_TUNNEL_ID, err.to_string());
            Err(err)
        }
    }
}

async fn start_client_tunnel_inner(state: &AppState) -> Result<TunnelInfo, AppError> {
    let config = load_or_default_client_tunnel_config()
        .await
        .map_err(AppError::Message)?;
    if !config.enabled {
        return Err(AppError::Message("client tunnel is disabled".into()));
    }
    let router_config = current_tunnel_config();
    let http_client = reqwest::Client::new();
    crate::tunnel::connection::claim_client_tunnel(
        &http_client,
        &router_config,
        &crate::tunnel::connection::ClientTunnelClaim {
            owner_email: config.owner_email.clone(),
            subdomain: config.subdomain.clone(),
            enabled: true,
        },
    )
    .await
    .map_err(|e| {
        AppError::Message(crate::email_auth::humanize_remote_owner_binding_error(
            &e.to_string(),
        ))
    })?;

    let local_addr = current_proxy_local_addr(state).await?;
    ensure_proxy_reachable(&local_addr).await?;
    {
        let mut mgr = state.tunnel_manager.write().await;
        if mgr.get_info(CLIENT_TUNNEL_ID).is_some() {
            mgr.stop_tunnel(CLIENT_TUNNEL_ID)
                .await
                .map_err(|e| AppError::Message(e.to_string()))?;
        }
        let info = mgr
            .start_tunnel(
                CLIENT_TUNNEL_ID,
                TunnelRequest {
                    tunnel_type: TunnelType::ClientWebHttp,
                    subdomain: config.subdomain.clone(),
                    local_addr,
                    share_metadata: None,
                },
                state.db.clone(),
            )
            .await
            .map_err(|e| AppError::Message(e.to_string()))?;
        let mut saved = config;
        saved.tunnel_url = Some(info.tunnel_url.clone());
        save_client_tunnel_settings(&saved).map_err(AppError::Message)?;
        Ok(info)
    }
}

async fn client_tunnel_status(state: &AppState) -> ShareTunnelStatus {
    let mgr = state.tunnel_manager.read().await;
    ShareTunnelStatus {
        info: mgr.get_info(CLIENT_TUNNEL_ID),
        last_error: mgr.get_last_error(CLIENT_TUNNEL_ID),
        requires_owner_login: false,
    }
}

async fn load_or_default_client_tunnel_config() -> Result<ClientTunnelSettingsView, String> {
    let settings = crate::settings::get_settings();
    if let Some(config) = settings.client_tunnel {
        return Ok(ClientTunnelSettingsView {
            owner_email: config.owner_email,
            subdomain: config.subdomain,
            enabled: config.enabled,
            auto_start: config.auto_start,
            tunnel_url: config.tunnel_url,
        });
    }
    let owner_email = installation_owner_email().await?;
    let subdomain = format!("app-{}", derive_subdomain_from_email(&owner_email));
    Ok(ClientTunnelSettingsView {
        owner_email,
        subdomain: normalize_subdomain(&subdomain)?,
        enabled: true,
        auto_start: true,
        tunnel_url: None,
    })
}

fn save_client_tunnel_settings(config: &ClientTunnelSettingsView) -> Result<(), String> {
    let mut settings = crate::settings::get_settings();
    settings.client_tunnel = Some(crate::settings::ClientTunnelSettings {
        owner_email: config.owner_email.clone(),
        subdomain: config.subdomain.clone(),
        enabled: config.enabled,
        auto_start: config.auto_start,
        tunnel_url: config.tunnel_url.clone(),
    });
    crate::settings::update_settings(settings).map_err(|e| e.to_string())
}

async fn installation_owner_email() -> Result<String, String> {
    let config = current_tunnel_config();
    let owner = crate::email_auth::fetch_remote_owner_binding(&config)
        .await?
        .ok_or_else(|| "client tunnel requires a verified installation owner email".to_string())?;
    normalize_owner_email(&owner)
}

fn derive_subdomain_from_email(email: &str) -> String {
    let local = email.split('@').next().unwrap_or_default();
    let filtered: String = local
        .chars()
        .filter(|ch| ch.is_ascii_alphabetic())
        .map(|ch| ch.to_ascii_lowercase())
        .take(10)
        .collect();
    let prefix = if filtered.is_empty() {
        "s".to_string()
    } else {
        filtered
    };
    let suffix = chrono::Utc::now().timestamp_millis().max(0);
    format!("{prefix}-{}", base36_suffix(suffix as u64, 5))
}

fn base36_suffix(mut value: u64, len: usize) -> String {
    const ALPHABET: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut buf = Vec::new();
    loop {
        buf.push(ALPHABET[(value % 36) as usize] as char);
        value /= 36;
        if value == 0 {
            break;
        }
    }
    while buf.len() < len {
        buf.push('0');
    }
    let full = buf.iter().rev().collect::<String>();
    full.chars()
        .rev()
        .take(len)
        .collect::<String>()
        .chars()
        .rev()
        .collect()
}

pub(crate) fn normalize_subdomain(value: &str) -> Result<String, String> {
    let value = value.trim().to_ascii_lowercase();
    if value.len() < 3 || value.len() > 63 {
        return Err("subdomain 长度必须在 3-63 之间".to_string());
    }
    if value.starts_with('-') || value.ends_with('-') {
        return Err("subdomain 不能以 - 开头或结尾".to_string());
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
    {
        return Err("subdomain 只能包含小写字母、数字和 -".to_string());
    }
    Ok(value)
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

pub(crate) fn normalize_owner_email(email: &str) -> Result<String, String> {
    let email = email.trim().to_ascii_lowercase();
    if email.is_empty()
        || email.len() > 254
        || email.chars().any(char::is_whitespace)
        || email.matches('@').count() != 1
    {
        return Err("邮箱格式无效".to_string());
    }
    let (local, domain) = email
        .split_once('@')
        .ok_or_else(|| "邮箱格式无效".to_string())?;
    if local.is_empty()
        || domain.is_empty()
        || !domain.contains('.')
        || domain.starts_with('.')
        || domain.ends_with('.')
    {
        return Err("邮箱格式无效".to_string());
    }
    Ok(email)
}

#[tauri::command]
pub async fn configure_tunnel(
    state: State<'_, AppState>,
    config: TunnelConfig,
) -> Result<(), String> {
    let domain = crate::tunnel::config::normalize_tunnel_domain(&config.domain)?;
    let config = TunnelConfig { domain };

    // 持久化到 AppSettings，确保应用重启后依然可用
    let mut settings = crate::settings::get_settings();
    settings.set_share_router_domain(Some(config.domain.clone()));
    crate::settings::update_settings(settings).map_err(|e| e.to_string())?;

    let mut mgr = state.tunnel_manager.write().await;
    mgr.set_config(config);
    Ok(())
}

fn current_tunnel_config() -> TunnelConfig {
    TunnelConfig::from_settings_or_default()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_markets_parse_router_snake_case_fields() {
        let body: MarketsResponse = serde_json::from_value(serde_json::json!({
            "markets": [
                {
                    "id": "market-1",
                    "display_name": "Share Market",
                    "email": "share-market@example.com",
                    "subdomain": "share-market",
                    "public_base_url": "https://share-market.example.com",
                    "market_kind": "share",
                    "status": "active"
                }
            ]
        }))
        .expect("router snake_case markets response should parse");

        assert_eq!(body.markets[0].display_name, "Share Market");
        assert_eq!(
            body.markets[0].public_base_url,
            "https://share-market.example.com"
        );
        assert_eq!(body.markets[0].market_kind, "share");
    }

    #[test]
    fn owner_email_validation_rejects_ambiguous_addresses() {
        assert_eq!(
            normalize_owner_email(" Owner@Example.COM ").unwrap(),
            "owner@example.com"
        );
        assert!(normalize_owner_email("owner").is_err());
        assert!(normalize_owner_email("owner@").is_err());
        assert!(normalize_owner_email("@example.com").is_err());
        assert!(normalize_owner_email("owner@@example.com").is_err());
        assert!(normalize_owner_email("owner@example").is_err());
        assert!(normalize_owner_email("owner @example.com").is_err());
    }

    #[test]
    fn public_markets_parse_wrapped_or_direct_list_payloads() {
        let wrapped: MarketsPayload = serde_json::from_value(serde_json::json!({
            "markets": [{
                "id": "usage-market-1",
                "displayName": "https://market.example.com",
                "email": "market@example.com",
                "subdomain": "market",
                "publicBaseUrl": "https://market.example.com",
                "marketKind": "usage",
                "status": "active"
            }]
        }))
        .expect("wrapped router markets response should parse");
        assert_eq!(wrapped.into_markets()[0].market_kind, "usage");

        let direct: MarketsPayload = serde_json::from_value(serde_json::json!([
            {
                "id": "share-market-1",
                "displayName": "https://share-market.example.com",
                "email": "share-market@example.com",
                "subdomain": "share-market",
                "publicBaseUrl": "https://share-market.example.com",
                "marketKind": "share",
                "status": "active"
            }
        ]))
        .expect("direct router markets response should parse");
        assert_eq!(direct.into_markets()[0].market_kind, "share");
    }

    #[test]
    fn restore_share_tunnel_uses_only_previous_active_status() {
        let active = test_share("active", false);
        let paused_legacy_auto_start = test_share("paused", true);

        assert!(should_restore_share_tunnel(&active));
        assert!(!should_restore_share_tunnel(&paused_legacy_auto_start));
    }

    #[test]
    fn share_market_delegation_preserves_per_app_shareto() {
        let mut access_by_app = HashMap::new();
        access_by_app.insert(
            "claude".to_string(),
            ShareAppAccess {
                shared_with_emails: vec![
                    "friend@example.com".to_string(),
                    "usage-market@example.com".to_string(),
                ],
                market_access_mode: "selected".to_string(),
            },
        );
        access_by_app.insert(
            "codex".to_string(),
            ShareAppAccess {
                shared_with_emails: vec![
                    "coder@example.com".to_string(),
                    "old-share-market@example.com".to_string(),
                ],
                market_access_mode: "selected".to_string(),
            },
        );
        let public_market_emails = HashSet::from([
            "usage-market@example.com".to_string(),
            "old-share-market@example.com".to_string(),
            "new-share-market@example.com".to_string(),
        ]);

        let next = replace_public_market_acl_with_share_market(
            access_by_app,
            "new-share-market@example.com",
            &public_market_emails,
        );

        assert_eq!(
            next["claude"].shared_with_emails,
            vec![
                "friend@example.com".to_string(),
                "new-share-market@example.com".to_string()
            ]
        );
        assert_eq!(
            next["codex"].shared_with_emails,
            vec![
                "coder@example.com".to_string(),
                "new-share-market@example.com".to_string()
            ]
        );
    }

    fn test_share(status: &str, auto_start: bool) -> ShareRecord {
        ShareRecord {
            id: "share-1".to_string(),
            name: "Test Share".to_string(),
            owner_email: "owner@example.com".to_string(),
            shared_with_emails: vec![],
            market_access_mode: "selected".to_string(),
            access_by_app: HashMap::new(),
            app_settings: HashMap::new(),
            for_sale_official_price_percent_by_app: HashMap::new(),
            description: None,
            for_sale: "No".to_string(),
            sale_market_kind: "token".to_string(),
            bindings: HashMap::new(),
            dynamic_apps: HashSet::new(),
            api_key: String::new(),
            settings_config: None,
            token_limit: -1,
            parallel_limit: -1,
            tokens_used: 0,
            requests_count: 0,
            expires_at: String::new(),
            subdomain: Some("share-1".to_string()),
            tunnel_url: None,
            status: status.to_string(),
            auto_start,
            created_at: String::new(),
            last_used_at: None,
        }
    }
}
