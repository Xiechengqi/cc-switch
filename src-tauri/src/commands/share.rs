use crate::database::ShareRecord;
use crate::email_auth;
use crate::error::AppError;
use crate::proxy::ProxyConfig;
use crate::services::share::{PrepareShareParams, ShareService};
use crate::store::AppState;
use crate::tunnel::config::{TunnelConfig, TunnelInfo, TunnelRequest, TunnelType};
use serde::{Deserialize, Serialize};
use tauri::State;
use tokio::net::TcpStream;
use tokio::time::{timeout, Duration};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateShareParams {
    pub app_type: String,
    pub description: Option<String>,
    pub for_sale: String,
    pub token_limit: i64,
    pub parallel_limit: i64,
    pub expires_in_secs: i64,
    pub subdomain: Option<String>,
    pub api_key: Option<String>,
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
pub struct UpdateShareApiKeyParams {
    pub share_id: String,
    pub api_key: String,
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
pub struct UpdateShareExpirationParams {
    pub share_id: String,
    pub expires_at: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateShareAclParams {
    pub share_id: String,
    pub shared_with_emails: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectInfo {
    pub tunnel_url: String,
    pub api_key: String,
    pub subdomain: String,
}

#[tauri::command]
pub async fn create_share(
    state: State<'_, AppState>,
    params: CreateShareParams,
) -> Result<ShareRecord, String> {
    let owner_email = require_authenticated_email(&state.db)?;
    let requested_subdomain = params.subdomain.clone();
    let mut last_claim_error = None;
    let mut share = None;

    for _ in 0..5 {
        let candidate = ShareService::prepare_create(PrepareShareParams {
            owner_email: owner_email.clone(),
            app_type: params.app_type.clone(),
            description: params.description.clone(),
            for_sale: params.for_sale.clone(),
            token_limit: params.token_limit,
            parallel_limit: params.parallel_limit,
            expires_in_secs: params.expires_in_secs,
            subdomain: requested_subdomain.clone(),
            api_key: params.api_key.clone(),
        })
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
    let owner_email = require_authenticated_email(&state.db)?;
    let share = ShareService::get_detail(&state.db, &share_id)
        .map_err(|e: AppError| e.to_string())?
        .ok_or_else(|| format!("Share not found: {share_id}"))?;
    if share.owner_email != owner_email {
        return Err("只有当前 share 的 owner 才能授权 market".to_string());
    }
    let mut emails = share.shared_with_emails;
    emails.push(market_email);
    ShareService::update_acl(&state.db, &share_id, &owner_email, emails)
        .map_err(|e: AppError| e.to_string())
}

#[tauri::command]
pub fn update_share_acl(
    state: State<'_, AppState>,
    params: UpdateShareAclParams,
) -> Result<ShareRecord, String> {
    let owner_email = require_authenticated_email(&state.db)?;
    let share = ShareService::get_detail(&state.db, &params.share_id)
        .map_err(|e: AppError| e.to_string())?
        .ok_or_else(|| format!("Share not found: {}", params.share_id))?;
    if share.owner_email != owner_email {
        return Err("只有当前 share 的 owner 才能修改分享名单".to_string());
    }
    ShareService::update_acl(
        &state.db,
        &params.share_id,
        &owner_email,
        params.shared_with_emails,
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
pub fn update_share_api_key(
    state: State<'_, AppState>,
    params: UpdateShareApiKeyParams,
) -> Result<ShareRecord, String> {
    ShareService::update_api_key(&state.db, &params.share_id, &params.api_key)
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
pub fn update_share_expiration(
    state: State<'_, AppState>,
    params: UpdateShareExpirationParams,
) -> Result<ShareRecord, String> {
    ShareService::update_expires_at(&state.db, &params.share_id, &params.expires_at)
        .map_err(|e: AppError| e.to_string())
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
        return start_share_tunnel_inner(state.inner(), &params.share_id)
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
    start_share_tunnel_inner(state.inner(), &share_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn disable_share(state: State<'_, AppState>, share_id: String) -> Result<(), String> {
    {
        let mut mgr = state.tunnel_manager.write().await;
        if mgr.get_info(&share_id).is_some() {
            mgr.stop_tunnel(&share_id)
                .await
                .map_err(|e| e.to_string())?;
        }
    }
    state
        .db
        .clear_share_tunnel(&share_id)
        .map_err(|e: AppError| e.to_string())?;
    ShareService::pause(&state.db, &share_id).map_err(|e: AppError| e.to_string())?;

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
    start_share_tunnel_inner(state.inner(), &share_id)
        .await
        .map_err(|e| e.to_string())
}

pub async fn restore_active_share_tunnel(state: &AppState) -> Result<(), AppError> {
    for share in ShareService::list(&state.db)?
        .into_iter()
        .filter(|share| share.status == "active")
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
        if let Err(err) = start_share_tunnel_inner(state, &share.id).await {
            log::warn!(
                "[Share] Failed to restore active share tunnel for share_id={}: {}",
                share.id,
                err
            );
        }
    }

    Ok(())
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
) -> Result<Option<TunnelInfo>, String> {
    let mgr = state.tunnel_manager.read().await;
    Ok(mgr.get_info(&share_id))
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
        api_key: share.share_token,
        subdomain,
    })
}

fn require_authenticated_email(
    db: &std::sync::Arc<crate::database::Database>,
) -> Result<String, String> {
    let status = email_auth::get_status()?;
    if !status.authenticated {
        return Err("创建 share 前请先完成邮箱验证码登录".to_string());
    }
    let email = status
        .email
        .ok_or_else(|| "邮箱登录状态异常，请重新登录".to_string())?;
    if let Some(existing_share) = ShareService::list(db)
        .map_err(|e: AppError| e.to_string())?
        .into_iter()
        .next()
    {
        if existing_share.owner_email != email {
            return Err(format!(
                "当前设备已绑定邮箱 {}，不能切换到 {}",
                existing_share.owner_email, email
            ));
        }
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
