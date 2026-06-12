use crate::database::{
    derive_access_by_app, legacy_acl_from_access_by_app, Database, ShareAppAccess, ShareRecord,
};
use crate::error::AppError;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub struct ShareService;

pub struct ShareTokenValidation {
    pub share: Option<ShareRecord>,
    pub rejection: Option<ShareTokenRejectReason>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShareTokenRejectReason {
    NotFound,
    Inactive,
    Expired,
    Exhausted,
}

impl ShareTokenValidation {
    fn valid(share: ShareRecord) -> Self {
        Self {
            share: Some(share),
            rejection: None,
            message: None,
        }
    }

    fn rejected(reason: ShareTokenRejectReason, message: impl Into<String>) -> Self {
        Self {
            share: None,
            rejection: Some(reason),
            message: Some(message.into()),
        }
    }
}

impl ShareService {
    pub const UNLIMITED_TOKEN_LIMIT: i64 = -1;
    pub const UNLIMITED_PARALLEL_LIMIT: i64 = -1;
    pub const MIN_PARALLEL_LIMIT: i64 = 3;
    pub const MAX_DESCRIPTION_CHARS: usize = 200;
    pub const FOR_SALE_NO: &'static str = "No";
    pub const FOR_SALE_YES: &'static str = "Yes";
    pub const FOR_SALE_FREE: &'static str = "Free";

    pub fn is_unlimited_token_limit(token_limit: i64) -> bool {
        token_limit == Self::UNLIMITED_TOKEN_LIMIT
    }

    pub fn is_unlimited_parallel_limit(parallel_limit: i64) -> bool {
        parallel_limit == Self::UNLIMITED_PARALLEL_LIMIT
    }

    pub fn prepare_create(
        db: &Arc<Database>,
        params: PrepareShareParams,
    ) -> Result<ShareRecord, AppError> {
        let id = uuid::Uuid::new_v4().to_string();
        let subdomain = params
            .subdomain
            .map(|value| normalize_subdomain(&value))
            .transpose()?
            .unwrap_or_else(|| format!("share-{}", &id[..8]));
        let now = chrono::Utc::now();
        let expires_at = now + chrono::Duration::seconds(params.expires_in_secs);
        let description = normalize_description(params.description)?;
        let for_sale = normalize_for_sale(&params.for_sale)?;
        let requested_sale_market_kind = normalize_sale_market_kind(&params.sale_market_kind)?;
        let sale_market_kind =
            if for_sale == ShareService::FOR_SALE_YES && requested_sale_market_kind == "share" {
                "token".to_string()
            } else {
                requested_sale_market_kind
            };
        let parallel_limit = normalize_parallel_limit(params.parallel_limit)?;
        let owner_email = normalize_email(&params.owner_email)?;
        let token_limit = params.token_limit;

        // P8 校验每个 binding 的 (app_type 合法 + provider 存在且 app_type 一致)。
        // 0 binding 也允许，创建后再在 UI 逐个挂 provider。
        let mut bindings = HashMap::new();
        for (raw_app, raw_pid) in &params.bindings {
            let app_type = normalize_share_app_type(raw_app)?;
            let provider_id = normalize_provider_id(raw_pid)?;
            let provider = db
                .get_provider_by_id(&provider_id, &app_type)?
                .ok_or_else(|| {
                    AppError::Message(format!(
                        "Provider {provider_id} 在 {app_type} 应用下不存在，无法绑定 share"
                    ))
                })?;
            debug_assert_eq!(provider.id, provider_id);
            bindings.insert(app_type, provider_id);
        }

        // P17 动态绑定：把每个 dynamic app 的当前激活 provider 解析后塞进 bindings。
        // 与显式 bindings 互斥，不允许同一个 app 同时出现在两边——前端要么传固定
        // provider_id 进 bindings，要么把 app 列进 dynamic_apps 让后端自己解析。
        let mut dynamic_apps: HashSet<String> = HashSet::new();
        for raw_app in &params.dynamic_apps {
            let app_type = normalize_share_app_type(raw_app)?;
            if bindings.contains_key(&app_type) {
                return Err(AppError::Message(format!(
                    "{app_type} 同时出现在固定 bindings 和 dynamic_apps 里，请只选一种"
                )));
            }
            let app = std::str::FromStr::from_str(app_type.as_str()).map_err(|e: AppError| e)?;
            let current = crate::settings::get_effective_current_provider(db, &app)?
                .filter(|id| !id.is_empty())
                .ok_or_else(|| {
                    AppError::Message(format!(
                        "{app_type} 当前没有激活的 provider，无法创建动态绑定。请先在 {app_type} 选择一个 provider，或改用固定绑定。"
                    ))
                })?;
            // 校验 provider 在该 app_type 下存在。
            db.get_provider_by_id(&current, &app_type)?.ok_or_else(|| {
                AppError::Message(format!(
                    "{app_type} 当前 provider {current} 已不存在，无法创建动态绑定"
                ))
            })?;
            bindings.insert(app_type.clone(), current);
            dynamic_apps.insert(app_type);
        }
        ensure_unique_fixed_provider_ids(&bindings, &dynamic_apps)?;
        for (app_type, provider_id) in &bindings {
            if dynamic_apps.contains(app_type) {
                continue;
            }
            ensure_fixed_provider_available(db, provider_id, None, None)?;
        }

        let record = ShareRecord {
            id,
            name: owner_email.clone(),
            owner_email,
            shared_with_emails: Vec::new(),
            market_access_mode: "selected".to_string(),
            access_by_app: derive_access_by_app(&bindings, &[], "selected"),
            for_sale_official_price_percent_by_app: HashMap::new(),
            description,
            for_sale,
            sale_market_kind,
            bindings,
            dynamic_apps,
            api_key: String::new(),
            settings_config: None,
            token_limit,
            parallel_limit,
            tokens_used: 0,
            requests_count: 0,
            expires_at: expires_at.to_rfc3339(),
            subdomain: Some(subdomain),
            tunnel_url: None,
            status: "paused".to_string(),
            auto_start: params.auto_start,
            created_at: now.to_rfc3339(),
            last_used_at: None,
        };
        Ok(record)
    }

    pub fn create(db: &Arc<Database>, record: ShareRecord) -> Result<ShareRecord, AppError> {
        // 多 share 模式：同一 cc-switch 可挂多个 share。
        // share ↔ fixed provider 1:1 已在 prepare_create / update_provider_binding 前置校验；
        // schema 的 UNIQUE(provider_id) 只作为并发写入时的最后兜底。
        db.create_share(&record)?;
        crate::tunnel::sync::schedule_sync_share(record.clone(), db);
        Ok(record)
    }

    pub fn delete(db: &Arc<Database>, share_id: &str) -> Result<(), AppError> {
        // 只删自己；多 share 模式下不能再级联删全部。
        // 调用方（commands/share.rs）负责先 stop_tunnel(share_id) 释放 SSH 通道。
        db.delete_share(share_id)?;
        // 清掉这条 share 在 model_health store 里所有 app slot 的残留条目。
        // 不做的话进程 OnceLock store 会一直留着，下次同 id share（极不可能但
        // 路径上不该假定不会发生）就会读到陈旧结果。
        let share_id = share_id.to_string();
        tauri::async_runtime::spawn(async move {
            crate::tunnel::model_health::purge_share(&share_id).await;
        });
        Ok(())
    }

    pub fn pause(db: &Arc<Database>, share_id: &str) -> Result<(), AppError> {
        db.update_share_status(share_id, "paused")?;
        if let Some(share) = db.get_share_by_id(share_id)? {
            crate::tunnel::sync::schedule_sync_share(share, db);
        }
        Ok(())
    }

    pub fn resume(db: &Arc<Database>, share_id: &str) -> Result<(), AppError> {
        db.update_share_status(share_id, "active")?;
        if let Some(share) = db.get_share_by_id(share_id)? {
            crate::tunnel::sync::schedule_sync_share(share, db);
        }
        Ok(())
    }

    pub fn list(db: &Arc<Database>) -> Result<Vec<ShareRecord>, AppError> {
        db.list_shares()
    }

    pub fn get_detail(db: &Arc<Database>, share_id: &str) -> Result<Option<ShareRecord>, AppError> {
        db.get_share_by_id(share_id)
    }

    /// Resolve a share by its id and decide whether it is currently routable.
    ///
    /// Used by the share-scoped proxy handlers and the per-share web admin
    /// surface. Authentication of the caller (owner / sharedWithEmails / Free)
    /// is performed by cc-switch-router before the request reaches us; this
    /// function only enforces share-level lifecycle (active, not expired,
    /// quota not exhausted).
    pub fn validate_share_for_invocation(
        db: &Arc<Database>,
        share_id: &str,
    ) -> Result<Option<ShareTokenValidation>, AppError> {
        let share = match db.get_share_by_id(share_id)? {
            Some(s) => s,
            None => {
                return Ok(Some(ShareTokenValidation::rejected(
                    ShareTokenRejectReason::NotFound,
                    "Share not found on this cc-switch.",
                )));
            }
        };

        if share.status != "active" {
            return Ok(Some(ShareTokenValidation::rejected(
                ShareTokenRejectReason::Inactive,
                format!(
                    "Share is not active (current status: {}). Start the share first.",
                    share.status
                ),
            )));
        }

        // Check expiry
        if let Ok(expires) = chrono::DateTime::parse_from_rfc3339(&share.expires_at) {
            if chrono::Utc::now() > expires {
                let _ = db.update_share_status(&share.id, "expired");
                return Ok(Some(ShareTokenValidation::rejected(
                    ShareTokenRejectReason::Expired,
                    "Share has expired. Extend the share expiration or create a new share.",
                )));
            }
        }

        // Check token limit
        if !Self::is_unlimited_token_limit(share.token_limit)
            && share.tokens_used >= share.token_limit
        {
            let _ = db.update_share_status(&share.id, "exhausted");
            return Ok(Some(ShareTokenValidation::rejected(
                ShareTokenRejectReason::Exhausted,
                "Share token quota has been exhausted. Reset usage or increase the token limit.",
            )));
        }

        Ok(Some(ShareTokenValidation::valid(share)))
    }

    pub fn record_request(db: &Arc<Database>, share_id: &str) -> Result<(), AppError> {
        db.increment_share_requests(share_id)?;
        if let Some(share) = db.get_share_by_id(share_id)? {
            crate::tunnel::sync::schedule_sync_share(share, db);
        }
        Ok(())
    }

    pub fn record_tokens(db: &Arc<Database>, share_id: &str, tokens: i64) -> Result<(), AppError> {
        let new_used = db.increment_share_tokens(share_id, tokens)?;

        if let Ok(Some(share)) = db.get_share_by_id(share_id) {
            if !Self::is_unlimited_token_limit(share.token_limit) && new_used >= share.token_limit {
                let _ = db.update_share_status(share_id, "exhausted");
            }
        }
        if let Some(share) = db.get_share_by_id(share_id)? {
            crate::tunnel::sync::schedule_sync_share(share, db);
        }
        Ok(())
    }

    pub fn reset_usage(db: &Arc<Database>, share_id: &str) -> Result<ShareRecord, AppError> {
        let share = db
            .get_share_by_id(share_id)?
            .ok_or_else(|| AppError::Message(format!("Share not found: {share_id}")))?;

        db.reset_share_usage(share_id)?;

        if share.status == "exhausted" {
            db.update_share_status(share_id, "paused")?;
        }

        let updated = db
            .get_share_by_id(share_id)?
            .ok_or_else(|| AppError::Message(format!("Share not found: {share_id}")))?;
        crate::tunnel::sync::schedule_sync_share(updated.clone(), db);
        Ok(updated)
    }

    pub fn update_token_limit(
        db: &Arc<Database>,
        share_id: &str,
        token_limit: i64,
    ) -> Result<ShareRecord, AppError> {
        if token_limit <= 0 && !Self::is_unlimited_token_limit(token_limit) {
            return Err(AppError::Message(
                "Token limit 必须大于 0，或设为 -1 表示无上限".to_string(),
            ));
        }

        let share = db
            .get_share_by_id(share_id)?
            .ok_or_else(|| AppError::Message(format!("Share not found: {share_id}")))?;

        db.update_share_token_limit(share_id, token_limit)?;

        if !Self::is_unlimited_token_limit(token_limit) && share.tokens_used >= token_limit {
            db.update_share_status(share_id, "exhausted")?;
        } else if share.status == "exhausted" {
            db.update_share_status(share_id, "paused")?;
        }

        let updated = db
            .get_share_by_id(share_id)?
            .ok_or_else(|| AppError::Message(format!("Share not found: {share_id}")))?;
        crate::tunnel::sync::schedule_sync_share(updated.clone(), db);
        Ok(updated)
    }

    pub fn update_parallel_limit(
        db: &Arc<Database>,
        share_id: &str,
        parallel_limit: i64,
    ) -> Result<ShareRecord, AppError> {
        let normalized = normalize_parallel_limit(parallel_limit)?;
        db.update_share_parallel_limit(share_id, normalized)?;
        let updated = db
            .get_share_by_id(share_id)?
            .ok_or_else(|| AppError::Message(format!("Share not found: {share_id}")))?;
        crate::tunnel::sync::schedule_sync_share(updated.clone(), db);
        Ok(updated)
    }

    pub fn update_subdomain(
        db: &Arc<Database>,
        share_id: &str,
        subdomain: &str,
    ) -> Result<ShareRecord, AppError> {
        let normalized = normalize_subdomain(subdomain)?;
        db.update_share_subdomain(share_id, &normalized)?;
        let updated = db
            .get_share_by_id(share_id)?
            .ok_or_else(|| AppError::Message(format!("Share not found: {share_id}")))?;
        crate::tunnel::sync::schedule_sync_share(updated.clone(), db);
        Ok(updated)
    }

    /// 改绑 / 新增 / 清空 share 的某个 app slot 的 provider 绑定。
    ///
    /// P8 多 app share：share 对每个 app_type 各自维护一个 slot。本函数处理 (share_id,
    /// app_type) 这一对。
    ///   - `new_provider_id = Some(...)` → 新建或改绑该 slot
    ///   - `new_provider_id = None`     → 清空该 slot（解绑）
    ///
    /// 校验链：
    /// - share 当前 status == 'paused'（避免请求路径取到不一致中间态）
    /// - new_provider_id 非 None 时：provider 存在 + app_type 与目标 slot 一致
    /// - UNIQUE(provider_id) 由 schema 兜底（一个 provider 不能同时占多 slot）
    /// - 乐观锁 CAS：读到的老 slot 自调用方读取后未被其他操作改动（B-1）
    /// - 与现有值相同时返回错误，避免误触发审计
    ///
    /// 成功后向 share_binding_history 写一行审计（C-3）。
    pub fn update_provider_binding(
        db: &Arc<Database>,
        share_id: &str,
        app_type: &str,
        new_provider_id: Option<&str>,
    ) -> Result<ShareRecord, AppError> {
        let share = db
            .get_share_by_id(share_id)?
            .ok_or_else(|| AppError::Message(format!("Share not found: {share_id}")))?;
        if share.status != "paused" {
            return Err(AppError::Message(format!(
                "[paused-required] Share 改绑 provider 前必须先暂停，当前状态: {}",
                share.status
            )));
        }
        let app_type = normalize_share_app_type(app_type)?;
        let old_provider_id = share.bindings.get(&app_type).cloned();

        let normalized_new = match new_provider_id {
            Some(value) if !value.trim().is_empty() => Some(normalize_provider_id(value)?),
            _ => None,
        };

        if normalized_new == old_provider_id {
            return Err(AppError::Message(if normalized_new.is_none() {
                format!("{app_type} slot 已经为空，无需操作")
            } else {
                format!("{app_type} 槽位的 provider 与当前绑定一致，无需改绑")
            }));
        }

        if let Some(pid) = &normalized_new {
            let provider = db.get_provider_by_id(pid, &app_type)?.ok_or_else(|| {
                AppError::Message(format!(
                    "Provider {pid} 在 {app_type} 应用下不存在，无法绑定 share"
                ))
            })?;
            debug_assert_eq!(provider.id, *pid);
            ensure_fixed_provider_available(db, pid, Some(share_id), Some(&app_type))?;
        }

        db.upsert_share_binding_with_history(
            share_id,
            &app_type,
            old_provider_id.as_deref(),
            normalized_new.as_deref(),
        )?;
        let updated = db
            .get_share_by_id(share_id)?
            .ok_or_else(|| AppError::Message(format!("Share not found: {share_id}")))?;
        // binding 改了（解绑或换 provider）—— 清掉旧 provider 在 model_health
        // store 里的探测结果，避免 dashboard 显示的还是上一个 provider 的状态，
        // 直到下一轮 30 min 的调度循环跑过来才覆盖掉。
        let purge_share_id = share_id.to_string();
        let purge_app_type = app_type.clone();
        tauri::async_runtime::spawn(async move {
            crate::tunnel::model_health::purge_share_app(&purge_share_id, &purge_app_type).await;
        });
        crate::tunnel::sync::schedule_sync_share(updated.clone(), db);
        Ok(updated)
    }

    /// 取 share 改绑历史。
    pub fn list_binding_history(
        db: &Arc<Database>,
        share_id: &str,
        limit: usize,
    ) -> Result<Vec<crate::database::ShareBindingHistoryEntry>, AppError> {
        db.list_share_binding_history(share_id, limit.min(100))
    }

    pub fn update_description(
        db: &Arc<Database>,
        share_id: &str,
        description: Option<String>,
    ) -> Result<ShareRecord, AppError> {
        let normalized = normalize_description(description)?;
        db.update_share_description(share_id, normalized.as_deref())?;
        let updated = db
            .get_share_by_id(share_id)?
            .ok_or_else(|| AppError::Message(format!("Share not found: {share_id}")))?;
        crate::tunnel::sync::schedule_sync_share(updated.clone(), db);
        Ok(updated)
    }

    pub fn update_for_sale(
        db: &Arc<Database>,
        share_id: &str,
        for_sale: &str,
    ) -> Result<ShareRecord, AppError> {
        let normalized = normalize_for_sale(for_sale)?;
        db.update_share_for_sale(share_id, &normalized)?;
        let updated = db
            .get_share_by_id(share_id)?
            .ok_or_else(|| AppError::Message(format!("Share not found: {share_id}")))?;
        crate::tunnel::sync::schedule_sync_share(updated.clone(), db);
        Ok(updated)
    }

    pub fn update_for_sale_official_price_percent_by_app(
        db: &Arc<Database>,
        share_id: &str,
        pricing: HashMap<String, u16>,
    ) -> Result<ShareRecord, AppError> {
        let pricing = normalize_for_sale_official_price_percent_by_app(pricing)?;
        db.update_share_for_sale_official_price_percent_by_app(share_id, &pricing)?;
        let updated = db
            .get_share_by_id(share_id)?
            .ok_or_else(|| AppError::Message(format!("Share not found: {share_id}")))?;
        crate::tunnel::sync::schedule_sync_share(updated.clone(), db);
        Ok(updated)
    }

    pub fn update_expires_at(
        db: &Arc<Database>,
        share_id: &str,
        expires_at: &str,
    ) -> Result<ShareRecord, AppError> {
        let expires = chrono::DateTime::parse_from_rfc3339(expires_at)
            .map_err(|_| AppError::Message("到期时间格式无效".to_string()))?
            .with_timezone(&chrono::Utc);
        let now = chrono::Utc::now();
        if expires <= now {
            return Err(AppError::Message("到期时间必须晚于当前时间".to_string()));
        }

        let share = db
            .get_share_by_id(share_id)?
            .ok_or_else(|| AppError::Message(format!("Share not found: {share_id}")))?;

        db.update_share_expires_at(share_id, &expires.to_rfc3339())?;

        if share.status == "expired" {
            db.update_share_status(share_id, "paused")?;
        }

        let updated = db
            .get_share_by_id(share_id)?
            .ok_or_else(|| AppError::Message(format!("Share not found: {share_id}")))?;
        crate::tunnel::sync::schedule_sync_share(updated.clone(), db);
        Ok(updated)
    }

    pub fn update_auto_start(
        db: &Arc<Database>,
        share_id: &str,
        auto_start: bool,
    ) -> Result<ShareRecord, AppError> {
        db.update_share_auto_start(share_id, auto_start)?;
        let updated = db
            .get_share_by_id(share_id)?
            .ok_or_else(|| AppError::Message(format!("Share not found: {share_id}")))?;
        crate::tunnel::sync::schedule_sync_share(updated.clone(), db);
        Ok(updated)
    }

    pub fn update_owner_email(
        db: &Arc<Database>,
        share_id: &str,
        owner_email: &str,
    ) -> Result<ShareRecord, AppError> {
        let share = db
            .get_share_by_id(share_id)?
            .ok_or_else(|| AppError::Message(format!("Share not found: {share_id}")))?;
        let owner_email = normalize_email(owner_email)?;
        let shared_with_emails =
            normalize_email_list(share.shared_with_emails.clone(), &owner_email)?;
        let market_access_mode = normalize_market_access_mode(&share.market_access_mode)?;
        let access_by_app =
            normalize_access_by_app(share.effective_access_by_app(), &owner_email, false)?;
        db.update_share_acl(
            share_id,
            &owner_email,
            &shared_with_emails,
            &market_access_mode,
            &access_by_app,
            &share.sale_market_kind,
        )?;
        let updated = db
            .get_share_by_id(share_id)?
            .ok_or_else(|| AppError::Message(format!("Share not found: {share_id}")))?;
        crate::tunnel::sync::schedule_sync_share(updated.clone(), db);
        Ok(updated)
    }

    pub fn transfer_owner_email(
        db: &Arc<Database>,
        share_id: &str,
        target_email: &str,
    ) -> Result<ShareRecord, AppError> {
        let share = db
            .get_share_by_id(share_id)?
            .ok_or_else(|| AppError::Message(format!("Share not found: {share_id}")))?;
        let old_owner_email = normalize_email(&share.owner_email)?;
        let target_email = normalize_email(target_email)?;
        if old_owner_email == target_email {
            return Err(AppError::Message(
                "新 owner 邮箱必须不同于当前 owner 邮箱".to_string(),
            ));
        }
        let current_shared_with =
            normalize_email_list(share.shared_with_emails.clone(), &old_owner_email)?;
        if !current_shared_with
            .iter()
            .any(|email| email == &target_email)
        {
            return Err(AppError::Message(
                "只能将已有 shareto email 升级为 owner".to_string(),
            ));
        }
        let mut next_shared_with = current_shared_with
            .into_iter()
            .filter(|email| email != &target_email)
            .collect::<Vec<_>>();
        if !next_shared_with
            .iter()
            .any(|email| email == &old_owner_email)
        {
            next_shared_with.push(old_owner_email.clone());
        }
        let next_shared_with = normalize_email_list(next_shared_with, &target_email)?;
        let market_access_mode = normalize_market_access_mode(&share.market_access_mode)?;
        let mut access_by_app =
            normalize_access_by_app(share.effective_access_by_app(), &target_email, false)?;
        for access in access_by_app.values_mut() {
            access
                .shared_with_emails
                .retain(|email| email != &target_email);
            if !access
                .shared_with_emails
                .iter()
                .any(|email| email == &old_owner_email)
            {
                access.shared_with_emails.push(old_owner_email.clone());
            }
            access.shared_with_emails =
                normalize_email_list(access.shared_with_emails.clone(), &target_email)?;
        }
        db.update_share_acl(
            share_id,
            &target_email,
            &next_shared_with,
            &market_access_mode,
            &access_by_app,
            &share.sale_market_kind,
        )?;
        let updated = db
            .get_share_by_id(share_id)?
            .ok_or_else(|| AppError::Message(format!("Share not found: {share_id}")))?;
        crate::tunnel::sync::schedule_sync_share(updated.clone(), db);
        Ok(updated)
    }

    pub fn update_acl(
        db: &Arc<Database>,
        share_id: &str,
        owner_email: &str,
        shared_with_emails: Vec<String>,
        market_access_mode: &str,
        access_by_app: Option<HashMap<String, ShareAppAccess>>,
        sale_market_kind: Option<&str>,
    ) -> Result<ShareRecord, AppError> {
        let share = db
            .get_share_by_id(share_id)?
            .ok_or_else(|| AppError::Message(format!("Share not found: {share_id}")))?;
        let owner_email = normalize_email(owner_email)?;
        let sale_market_kind =
            normalize_sale_market_kind(sale_market_kind.unwrap_or(&share.sale_market_kind))?;
        let allow_owner_in_acl = sale_market_kind == "share";
        let access_by_app = match access_by_app {
            Some(value) => normalize_access_by_app(value, &owner_email, allow_owner_in_acl)?,
            None => {
                let shared_with_emails = normalize_email_list_with_options(
                    shared_with_emails,
                    &owner_email,
                    allow_owner_in_acl,
                )?;
                let market_access_mode = normalize_market_access_mode(market_access_mode)?;
                derive_access_by_app(&share.bindings, &shared_with_emails, &market_access_mode)
            }
        };
        let (shared_with_emails, market_access_mode) =
            legacy_acl_from_access_by_app(&access_by_app);
        if sale_market_kind == "share" && !access_by_app_has_shared_email(&access_by_app) {
            return Err(AppError::Message(
                "Share Market 出售必须显式委托给一个 Share Market".to_string(),
            ));
        }
        db.update_share_acl(
            share_id,
            &owner_email,
            &shared_with_emails,
            &market_access_mode,
            &access_by_app,
            &sale_market_kind,
        )?;
        let updated = db
            .get_share_by_id(share_id)?
            .ok_or_else(|| AppError::Message(format!("Share not found: {share_id}")))?;
        crate::tunnel::sync::schedule_sync_share(updated.clone(), db);
        Ok(updated)
    }

    pub fn change_owner_email(
        db: &Arc<Database>,
        old_email: &str,
        new_email: &str,
    ) -> Result<Vec<ShareRecord>, AppError> {
        let old_email = normalize_email(old_email)?;
        let new_email = normalize_email(new_email)?;
        if old_email == new_email {
            return Err(AppError::Message(
                "新 owner 邮箱必须不同于当前 owner 邮箱".to_string(),
            ));
        }
        db.update_shares_owner_email(&old_email, &new_email)?;
        let updated = db
            .list_shares()?
            .into_iter()
            .filter(|share| share.owner_email == new_email)
            .collect::<Vec<_>>();
        for share in &updated {
            crate::tunnel::sync::schedule_sync_share(share.clone(), db);
        }
        Ok(updated)
    }
}

pub struct PrepareShareParams {
    pub owner_email: String,
    /// P8 多 app share：创建时一次性提交 0..3 个 binding（键 = app_type）。
    /// 全空允许（创建后再在 UI 里逐个挂 provider）。
    pub bindings: HashMap<String, String>,
    /// P17 动态绑定：被列入的 app 在该 share 上设为"跟随当前激活的 provider"。
    /// 集合内的 app 必须**未出现**在 `bindings` 里；prepare_create 会从 settings
    /// 解析该 app 的当前 provider 并自动塞进 bindings。如果当前 app 没有激活
    /// provider，整次创建会被拒绝。
    pub dynamic_apps: HashSet<String>,
    pub description: Option<String>,
    pub for_sale: String,
    pub sale_market_kind: String,
    pub token_limit: i64,
    pub parallel_limit: i64,
    pub expires_in_secs: i64,
    pub subdomain: Option<String>,
    pub auto_start: bool,
}

/// 校验 app_type 是否在多 app share 支持的集合内。
pub(crate) fn normalize_share_app_type(value: &str) -> Result<String, AppError> {
    let value = value.trim().to_ascii_lowercase();
    match value.as_str() {
        "claude" | "codex" | "gemini" => Ok(value),
        _ => Err(AppError::Message(
            "Share app_type 只支持 claude、codex、gemini".to_string(),
        )),
    }
}

fn normalize_provider_id(value: &str) -> Result<String, AppError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(AppError::Message("Share provider_id 不能为空".to_string()));
    }
    Ok(value.to_string())
}

fn normalize_subdomain(value: &str) -> Result<String, AppError> {
    let value = value.trim().to_ascii_lowercase();
    if value.len() < 3 || value.len() > 63 {
        return Err(AppError::Message(
            "子域名长度必须在 3 到 63 个字符之间".to_string(),
        ));
    }
    if value.starts_with('-') || value.ends_with('-') {
        return Err(AppError::Message("子域名不能以 - 开头或结尾".to_string()));
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
    {
        return Err(AppError::Message(
            "子域名只能包含小写字母、数字和 -".to_string(),
        ));
    }
    for reserved in ["admin", "api", "www", "cdn-cgi"] {
        if value == reserved {
            return Err(AppError::Message("该子域名为保留字，不能使用".to_string()));
        }
    }
    Ok(value)
}

fn normalize_parallel_limit(value: i64) -> Result<i64, AppError> {
    if ShareService::is_unlimited_parallel_limit(value) || value >= ShareService::MIN_PARALLEL_LIMIT
    {
        return Ok(value);
    }
    Err(AppError::Message(format!(
        "最大并发数必须大于等于 {}，或设为 -1 表示无上限",
        ShareService::MIN_PARALLEL_LIMIT
    )))
}

fn normalize_description(description: Option<String>) -> Result<Option<String>, AppError> {
    let Some(description) = description else {
        return Ok(None);
    };

    let trimmed = description.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    if trimmed.chars().count() > ShareService::MAX_DESCRIPTION_CHARS {
        return Err(AppError::Message(format!(
            "说明文字不能超过 {} 个字符",
            ShareService::MAX_DESCRIPTION_CHARS
        )));
    }

    Ok(Some(trimmed.to_string()))
}

fn normalize_for_sale(value: &str) -> Result<String, AppError> {
    match value.trim() {
        ShareService::FOR_SALE_NO => Ok(ShareService::FOR_SALE_NO.to_string()),
        ShareService::FOR_SALE_YES => Ok(ShareService::FOR_SALE_YES.to_string()),
        ShareService::FOR_SALE_FREE => Ok(ShareService::FOR_SALE_FREE.to_string()),
        _ => Err(AppError::Message(
            "For Sale 只能是 Yes、No 或 Free".to_string(),
        )),
    }
}

fn normalize_sale_market_kind(value: &str) -> Result<String, AppError> {
    match value.trim() {
        "token" => Ok("token".to_string()),
        "share" => Ok("share".to_string()),
        _ => Err(AppError::Message(
            "出售 Market 类型只能是 token 或 share".to_string(),
        )),
    }
}

fn ensure_unique_fixed_provider_ids(
    bindings: &HashMap<String, String>,
    dynamic_apps: &HashSet<String>,
) -> Result<(), AppError> {
    let mut seen: HashMap<&str, &str> = HashMap::new();
    for (app_type, provider_id) in bindings {
        if dynamic_apps.contains(app_type) {
            continue;
        }
        if let Some(previous_app) = seen.insert(provider_id.as_str(), app_type.as_str()) {
            return Err(AppError::Message(format!(
                "Provider {provider_id} 已同时选择给 {previous_app} 和 {app_type}；同一个固定 Provider 只能绑定一个 share 分支，请更换或清空其中一个"
            )));
        }
    }
    Ok(())
}

fn ensure_fixed_provider_available(
    db: &Arc<Database>,
    provider_id: &str,
    current_share_id: Option<&str>,
    current_app_type: Option<&str>,
) -> Result<(), AppError> {
    for (share, app_type) in db.list_active_shares_bound_to_provider(provider_id)? {
        if share.dynamic_apps.contains(&app_type) {
            continue;
        }
        let is_current_slot = current_share_id == Some(share.id.as_str())
            && current_app_type == Some(app_type.as_str());
        if is_current_slot {
            continue;
        }
        let share_label = share
            .subdomain
            .as_deref()
            .filter(|value| !value.is_empty())
            .unwrap_or(share.name.as_str());
        return Err(AppError::Message(format!(
            "Provider {provider_id} 已被 share {share_label} 的 {app_type} 分支绑定；同一个固定 Provider 只能绑定一个 share 分支，请先解绑原 share 后再保存"
        )));
    }
    Ok(())
}

fn access_by_app_has_shared_email(access_by_app: &HashMap<String, ShareAppAccess>) -> bool {
    access_by_app.values().any(|access| {
        access
            .shared_with_emails
            .iter()
            .any(|email| !email.trim().is_empty())
    })
}

fn normalize_market_access_mode(value: &str) -> Result<String, AppError> {
    match value.trim() {
        "selected" => Ok("selected".to_string()),
        "all" => Ok("all".to_string()),
        _ => Err(AppError::Message(
            "Market 访问模式只能是 selected 或 all".to_string(),
        )),
    }
}

fn normalize_for_sale_official_price_percent_by_app(
    pricing: HashMap<String, u16>,
) -> Result<HashMap<String, u16>, AppError> {
    let mut normalized = HashMap::new();
    for (app, percent) in pricing {
        let app = app.trim().to_ascii_lowercase();
        if !matches!(app.as_str(), "claude" | "codex" | "gemini") {
            return Err(AppError::Message(
                "模型定价只支持 claude、codex 或 gemini".to_string(),
            ));
        }
        if !(1..=100).contains(&percent) {
            return Err(AppError::Message(
                "模型定价百分比只能是 1-100 的整数".to_string(),
            ));
        }
        normalized.insert(app, percent);
    }
    Ok(normalized)
}

fn normalize_access_by_app(
    access_by_app: HashMap<String, ShareAppAccess>,
    owner_email: &str,
    allow_owner: bool,
) -> Result<HashMap<String, ShareAppAccess>, AppError> {
    let mut normalized = HashMap::new();
    for (app, access) in access_by_app {
        let app = normalize_share_app_type(&app)?;
        let shared_with_emails =
            normalize_email_list_with_options(access.shared_with_emails, owner_email, allow_owner)?;
        let market_access_mode = normalize_market_access_mode(&access.market_access_mode)?;
        normalized.insert(
            app,
            ShareAppAccess {
                shared_with_emails,
                market_access_mode,
            },
        );
    }
    Ok(normalized)
}

fn normalize_email(value: &str) -> Result<String, AppError> {
    let value = value.trim().to_ascii_lowercase();
    if value.is_empty() || !value.contains('@') {
        return Err(AppError::Message("邮箱格式无效".to_string()));
    }
    Ok(value)
}

fn normalize_email_list(values: Vec<String>, owner_email: &str) -> Result<Vec<String>, AppError> {
    normalize_email_list_with_options(values, owner_email, false)
}

fn normalize_email_list_with_options(
    values: Vec<String>,
    owner_email: &str,
    allow_owner: bool,
) -> Result<Vec<String>, AppError> {
    let mut result = Vec::new();
    for value in values {
        let email = normalize_email(&value)?;
        if (!allow_owner && email == owner_email) || result.contains(&email) {
            continue;
        }
        result.push(email);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::{Database, ShareRecord};
    use crate::provider::{Provider, ProviderMeta};
    use serde_json::json;
    use std::sync::Arc;

    fn make_provider(id: &str, app_type: &str) -> Provider {
        let mut provider = Provider::with_id(
            id.to_string(),
            format!("Provider {id}"),
            json!({ "env": {} }),
            Some(String::new()),
        );
        provider.category = Some("custom".to_string());
        provider.meta = Some(ProviderMeta {
            provider_type: Some(app_type.to_string()),
            ..Default::default()
        });
        provider
    }

    fn fresh_db() -> Arc<Database> {
        Arc::new(Database::memory().expect("memory db"))
    }

    fn base_params(provider_id: &str) -> PrepareShareParams {
        // 默认绑 claude slot；测试需要其它 slot 时直接改 bindings。
        let mut bindings = HashMap::new();
        bindings.insert("claude".to_string(), provider_id.to_string());
        PrepareShareParams {
            owner_email: "user@example.com".to_string(),
            bindings,
            dynamic_apps: HashSet::new(),
            description: None,
            for_sale: "No".to_string(),
            sale_market_kind: "token".to_string(),
            token_limit: ShareService::UNLIMITED_TOKEN_LIMIT,
            parallel_limit: ShareService::MIN_PARALLEL_LIMIT,
            expires_in_secs: 3600,
            subdomain: Some("alpha-share-01".to_string()),
            auto_start: false,
        }
    }

    fn raw_share(id: &str, provider_id: &str, subdomain: &str) -> ShareRecord {
        let mut bindings = HashMap::new();
        bindings.insert("claude".to_string(), provider_id.to_string());
        ShareRecord {
            id: id.to_string(),
            name: id.to_string(),
            owner_email: "user@example.com".to_string(),
            shared_with_emails: Vec::new(),
            market_access_mode: "selected".to_string(),
            access_by_app: HashMap::new(),
            for_sale_official_price_percent_by_app: HashMap::new(),
            description: None,
            for_sale: "No".to_string(),
            sale_market_kind: "token".to_string(),
            bindings,
            dynamic_apps: HashSet::new(),
            api_key: String::new(),
            settings_config: None,
            token_limit: ShareService::UNLIMITED_TOKEN_LIMIT,
            parallel_limit: ShareService::MIN_PARALLEL_LIMIT,
            tokens_used: 0,
            requests_count: 0,
            expires_at: "2100-01-01T00:00:00Z".to_string(),
            subdomain: Some(subdomain.to_string()),
            tunnel_url: None,
            status: "active".to_string(),
            auto_start: false,
            created_at: "2025-01-01T00:00:00Z".to_string(),
            last_used_at: None,
        }
    }

    #[test]
    fn prepare_create_rejects_empty_provider_id() {
        let db = fresh_db();
        let mut params = base_params("");
        // 显式塞一个空 provider_id 进 binding，触发 normalize_provider_id 拒绝。
        params
            .bindings
            .insert("claude".to_string(), "   ".to_string());
        let err = ShareService::prepare_create(&db, params)
            .expect_err("empty provider_id must be rejected");
        assert!(
            err.to_string().contains("provider_id 不能为空"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn prepare_create_rejects_unknown_provider() {
        let db = fresh_db();
        let err = ShareService::prepare_create(&db, base_params("ghost"))
            .expect_err("unknown provider must be rejected");
        assert!(
            err.to_string().contains("不存在"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn prepare_create_rejects_app_type_mismatch() {
        let db = fresh_db();
        // provider 注册在 codex 下，但 binding 写 ("claude", p1) — get_provider_by_id
        // 按 (id, app_type) 联合查询，结果应当为 None。
        db.save_provider("codex", &make_provider("p1", "codex"))
            .expect("save provider");
        let err = ShareService::prepare_create(&db, base_params("p1"))
            .expect_err("app_type mismatch must be rejected");
        assert!(
            err.to_string().contains("不存在"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn prepare_create_succeeds_with_valid_binding() {
        let db = fresh_db();
        db.save_provider("claude", &make_provider("p1", "claude"))
            .expect("save provider");
        let record = ShareService::prepare_create(&db, base_params("p1"))
            .expect("valid binding should succeed");
        assert_eq!(
            record.bindings.get("claude").map(String::as_str),
            Some("p1")
        );
        assert_eq!(record.primary_app().as_deref(), Some("claude"));
        assert_eq!(record.status, "paused");
    }

    #[test]
    fn prepare_create_defers_share_market_kind_until_acl_update() {
        let db = fresh_db();
        db.save_provider("claude", &make_provider("p1", "claude"))
            .expect("save provider");
        let mut params = base_params("p1");
        params.for_sale = "Yes".to_string();
        params.sale_market_kind = "share".to_string();

        let record =
            ShareService::prepare_create(&db, params).expect("share market create is deferred");

        assert_eq!(record.for_sale, "Yes");
        assert_eq!(record.sale_market_kind, "token");
    }

    #[test]
    fn update_acl_rejects_share_market_without_delegate_email() {
        let db = fresh_db();
        db.save_provider("claude", &make_provider("p1", "claude"))
            .expect("save provider");
        let mut params = base_params("p1");
        params.for_sale = "Yes".to_string();
        let record = ShareService::prepare_create(&db, params).expect("prepare ok");
        ShareService::create(&db, record.clone()).expect("create ok");

        let mut access_by_app = HashMap::new();
        access_by_app.insert(
            "claude".to_string(),
            ShareAppAccess {
                shared_with_emails: Vec::new(),
                market_access_mode: "selected".to_string(),
            },
        );
        let err = ShareService::update_acl(
            &db,
            &record.id,
            &record.owner_email,
            Vec::new(),
            "selected",
            Some(access_by_app),
            Some("share"),
        )
        .expect_err("share market delegation requires explicit market email");
        assert!(
            err.to_string().contains("显式委托"),
            "unexpected error: {err}"
        );
        let stored = db.get_share_by_id(&record.id).unwrap().unwrap();
        assert_eq!(stored.sale_market_kind, "token");
        assert!(stored.shared_with_emails.is_empty());
    }

    #[test]
    fn update_acl_accepts_share_market_with_delegate_email() {
        let db = fresh_db();
        db.save_provider("claude", &make_provider("p1", "claude"))
            .expect("save provider");
        let mut params = base_params("p1");
        params.for_sale = "Yes".to_string();
        let record = ShareService::prepare_create(&db, params).expect("prepare ok");
        ShareService::create(&db, record.clone()).expect("create ok");

        let mut access_by_app = HashMap::new();
        access_by_app.insert(
            "claude".to_string(),
            ShareAppAccess {
                shared_with_emails: vec!["share-market@example.com".to_string()],
                market_access_mode: "selected".to_string(),
            },
        );
        let updated = ShareService::update_acl(
            &db,
            &record.id,
            &record.owner_email,
            Vec::new(),
            "selected",
            Some(access_by_app),
            Some("share"),
        )
        .expect("share market delegation should save");

        assert_eq!(updated.sale_market_kind, "share");
        assert_eq!(updated.market_access_mode, "selected");
        assert_eq!(
            updated.shared_with_emails,
            vec!["share-market@example.com".to_string()]
        );
        assert_eq!(
            updated
                .access_by_app
                .get("claude")
                .map(|access| access.shared_with_emails.as_slice()),
            Some(&["share-market@example.com".to_string()][..])
        );
    }

    #[test]
    fn update_acl_accepts_share_market_delegate_equal_to_owner_email() {
        let db = fresh_db();
        db.save_provider("codex", &make_provider("p1", "codex"))
            .expect("save provider");
        let mut params = base_params("p1");
        params.owner_email = "router@jptokenswitch.cc".to_string();
        params.for_sale = "Yes".to_string();
        params.bindings.clear();
        params
            .bindings
            .insert("codex".to_string(), "p1".to_string());
        let record = ShareService::prepare_create(&db, params).expect("prepare ok");
        ShareService::create(&db, record.clone()).expect("create ok");

        let mut access_by_app = HashMap::new();
        access_by_app.insert(
            "codex".to_string(),
            ShareAppAccess {
                shared_with_emails: vec!["router@jptokenswitch.cc".to_string()],
                market_access_mode: "selected".to_string(),
            },
        );
        let updated = ShareService::update_acl(
            &db,
            &record.id,
            &record.owner_email,
            Vec::new(),
            "selected",
            Some(access_by_app),
            Some("share"),
        )
        .expect("share market delegation can use the owner email as market identity");

        assert_eq!(updated.sale_market_kind, "share");
        assert_eq!(
            updated.shared_with_emails,
            vec!["router@jptokenswitch.cc".to_string()]
        );
        assert_eq!(
            updated
                .access_by_app
                .get("codex")
                .map(|access| access.shared_with_emails.as_slice()),
            Some(&["router@jptokenswitch.cc".to_string()][..])
        );
    }

    /// P8 新增：可以一次创建多 app share，bindings 全部落库。
    #[test]
    fn prepare_create_accepts_multi_app_bindings() {
        let db = fresh_db();
        db.save_provider("claude", &make_provider("p-claude", "claude"))
            .unwrap();
        db.save_provider("codex", &make_provider("p-codex", "codex"))
            .unwrap();
        let mut params = base_params("p-claude");
        params
            .bindings
            .insert("codex".to_string(), "p-codex".to_string());
        let record = ShareService::prepare_create(&db, params).expect("multi-app prepare ok");
        ShareService::create(&db, record.clone()).expect("create ok");

        let stored = db.get_share_by_id(&record.id).unwrap().unwrap();
        assert_eq!(stored.supported_apps(), vec!["claude", "codex"]);
        assert_eq!(
            stored.bindings.get("claude").map(String::as_str),
            Some("p-claude")
        );
        assert_eq!(
            stored.bindings.get("codex").map(String::as_str),
            Some("p-codex")
        );
    }

    /// P8 新增：完全不绑也允许（用户后续逐个挂）。
    #[test]
    fn prepare_create_accepts_empty_bindings() {
        let db = fresh_db();
        let mut params = base_params("ignored");
        params.bindings.clear();
        let record = ShareService::prepare_create(&db, params).expect("empty bindings allowed");
        assert!(record.bindings.is_empty());
        assert!(record.primary_app().is_none());
    }

    #[test]
    fn prepare_create_rejects_duplicate_fixed_provider_ids() {
        let db = fresh_db();
        db.save_provider("claude", &make_provider("p1", "claude"))
            .unwrap();
        db.save_provider("codex", &make_provider("p1", "codex"))
            .unwrap();
        let mut params = base_params("p1");
        params
            .bindings
            .insert("codex".to_string(), "p1".to_string());

        let err = ShareService::prepare_create(&db, params)
            .expect_err("same fixed provider in two slots must be rejected before DB insert");
        assert!(
            err.to_string().contains("同一个固定 Provider"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn prepare_create_rejects_provider_bound_to_existing_share() {
        let db = fresh_db();
        db.save_provider("claude", &make_provider("p1", "claude"))
            .unwrap();
        db.create_share(&raw_share("s1", "p1", "sub1"))
            .expect("existing share inserts");

        let err = ShareService::prepare_create(&db, base_params("p1"))
            .expect_err("provider already bound to existing share must be rejected");
        assert!(
            err.to_string().contains("已被 share sub1"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn unique_provider_constraint_blocks_second_active_binding() {
        // 走侧表 UNIQUE INDEX：同一 provider 全局只能挂一个 share-slot。
        let db = fresh_db();
        db.create_share(&raw_share("s1", "p1", "sub1"))
            .expect("first share inserts");
        let err = db
            .create_share(&raw_share("s2", "p1", "sub2"))
            .expect_err("UNIQUE(provider_id) must reject second active binding");
        assert!(
            err.to_string().to_ascii_lowercase().contains("unique"),
            "unexpected error: {err}"
        );
    }

    /// P8 新增：同一个 provider 不能在两个 slot 里出现（侧表 UNIQUE 兜底）。
    #[test]
    fn unique_provider_blocks_same_provider_in_two_slots() {
        let db = fresh_db();
        let mut share = raw_share("s1", "p1", "sub1");
        // 试图把同一个 provider 同时绑到 claude 和 codex 两个 slot — 侧表 UNIQUE 兜底拒绝。
        share.bindings.insert("codex".to_string(), "p1".to_string());
        let err = db
            .create_share(&share)
            .expect_err("same provider in two slots must be rejected");
        assert!(
            err.to_string().to_ascii_lowercase().contains("unique"),
            "unexpected error: {err}"
        );
    }

    /// E-1：rebind 成功路径 + 写审计 + 乐观锁。
    #[test]
    fn update_provider_binding_writes_history_and_locks_optimistically() {
        let db = fresh_db();
        db.save_provider("claude", &make_provider("p1", "claude"))
            .unwrap();
        db.save_provider("claude", &make_provider("p2", "claude"))
            .unwrap();
        let mut share = raw_share("s1", "p1", "sub1");
        share.status = "paused".to_string();
        db.create_share(&share).unwrap();

        let updated = ShareService::update_provider_binding(&db, "s1", "claude", Some("p2"))
            .expect("rebind succeeds");
        assert_eq!(
            updated.bindings.get("claude").map(String::as_str),
            Some("p2")
        );

        let history = db.list_share_binding_history("s1", 10).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].old_provider_id.as_deref(), Some("p1"));
        assert_eq!(history[0].new_provider_id.as_deref(), Some("p2"));

        // 乐观锁：手动把 slot 改回 p1，再用"以为还是 p2"的快照改绑应失败。
        db.upsert_share_binding_with_history("s1", "claude", Some("p2"), Some("p1"))
            .unwrap();
        let err = db
            .upsert_share_binding_with_history("s1", "claude", Some("p2"), Some("p1"))
            .expect_err("stale snapshot rejected");
        assert!(err.to_string().contains("已被其他操作改动"));
    }

    /// E-1（cont）：rebind 必须先 paused。
    #[test]
    fn update_provider_binding_requires_paused_with_marker() {
        let db = fresh_db();
        db.save_provider("claude", &make_provider("p1", "claude"))
            .unwrap();
        db.save_provider("claude", &make_provider("p2", "claude"))
            .unwrap();
        let mut share = raw_share("s1", "p1", "sub1");
        share.status = "active".to_string();
        db.create_share(&share).unwrap();

        let err = ShareService::update_provider_binding(&db, "s1", "claude", Some("p2"))
            .expect_err("active share rebind blocked");
        assert!(
            err.to_string().contains("[paused-required]"),
            "missing marker: {err}"
        );
    }

    /// P8 新增：清空 slot（解绑）也走 update_provider_binding，传 None。
    #[test]
    fn update_provider_binding_supports_unbind() {
        let db = fresh_db();
        db.save_provider("claude", &make_provider("p1", "claude"))
            .unwrap();
        let mut share = raw_share("s1", "p1", "sub1");
        share.status = "paused".to_string();
        db.create_share(&share).unwrap();

        let updated = ShareService::update_provider_binding(&db, "s1", "claude", None)
            .expect("unbind succeeds");
        assert!(updated.bindings.is_empty());

        let history = db.list_share_binding_history("s1", 10).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].old_provider_id.as_deref(), Some("p1"));
        assert!(history[0].new_provider_id.is_none());
    }

    #[test]
    fn update_provider_binding_rejects_provider_bound_elsewhere() {
        let db = fresh_db();
        db.save_provider("claude", &make_provider("p1", "claude"))
            .unwrap();
        db.save_provider("claude", &make_provider("p2", "claude"))
            .unwrap();
        let mut first = raw_share("s1", "p1", "sub1");
        first.status = "paused".to_string();
        db.create_share(&first).unwrap();
        let mut second = raw_share("s2", "p2", "sub2");
        second.status = "paused".to_string();
        db.create_share(&second).unwrap();

        let err = ShareService::update_provider_binding(&db, "s2", "claude", Some("p1"))
            .expect_err("provider already bound to another share must be rejected");
        assert!(
            err.to_string().contains("已被 share sub1"),
            "unexpected error: {err}"
        );
    }

    /// E-4：api_key 字段是历史遗留死字段，prepare_create 永远写空串。
    #[test]
    fn prepare_create_leaves_api_key_empty_by_design() {
        let db = fresh_db();
        db.save_provider("claude", &make_provider("p1", "claude"))
            .unwrap();
        let record = ShareService::prepare_create(&db, base_params("p1")).expect("prepare ok");
        assert_eq!(record.api_key, "");
    }
}
