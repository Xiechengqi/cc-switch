use crate::database::{Database, ShareRecord};
use crate::error::AppError;
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

    pub fn prepare_create(params: PrepareShareParams) -> Result<ShareRecord, AppError> {
        let id = uuid::Uuid::new_v4().to_string();
        let subdomain = params
            .subdomain
            .map(|value| normalize_subdomain(&value))
            .transpose()?
            .unwrap_or_else(|| format!("share-{}", &id[..8]));
        let share_token = params
            .api_key
            .map(|value| normalize_api_key(&value))
            .transpose()?
            .unwrap_or_else(Self::generate_token);
        let now = chrono::Utc::now();
        let expires_at = now + chrono::Duration::seconds(params.expires_in_secs);
        let description = normalize_description(params.description)?;
        let for_sale = normalize_for_sale(&params.for_sale)?;
        let parallel_limit = normalize_parallel_limit(params.parallel_limit)?;
        let owner_email = normalize_email(&params.owner_email)?;
        let token_limit = params.token_limit;

        let record = ShareRecord {
            id,
            name: owner_email.clone(),
            owner_email,
            shared_with_emails: Vec::new(),
            description,
            for_sale,
            share_token,
            app_type: "proxy".to_string(),
            provider_id: None,
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
            created_at: now.to_rfc3339(),
            last_used_at: None,
        };
        Ok(record)
    }

    pub fn create(db: &Arc<Database>, record: ShareRecord) -> Result<ShareRecord, AppError> {
        if !db.list_shares()?.is_empty() {
            return Err(AppError::Message(
                "当前版本的分享能力基于本地代理服务，一个 cc-switch 只能创建一个分享".to_string(),
            ));
        }
        db.create_share(&record)?;
        crate::tunnel::sync::schedule_sync_share(record.clone(), db);
        Ok(record)
    }

    pub fn delete(db: &Arc<Database>, share_id: &str) -> Result<(), AppError> {
        if let Some(primary) = Self::primary_share(db)? {
            if primary.id == share_id {
                for share in db.list_shares()? {
                    db.delete_share(&share.id)?;
                }
            }
        }
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
        Ok(Self::primary_share(db)?.into_iter().collect())
    }

    pub fn get_detail(db: &Arc<Database>, share_id: &str) -> Result<Option<ShareRecord>, AppError> {
        Ok(Self::primary_share(db)?.filter(|share| share.id == share_id))
    }

    pub fn validate_token_with_reason(
        db: &Arc<Database>,
        token: &str,
    ) -> Result<Option<ShareTokenValidation>, AppError> {
        let share = match db.get_share_by_token(token)? {
            Some(s) => s,
            None => {
                return Ok(Some(ShareTokenValidation::rejected(
                    ShareTokenRejectReason::NotFound,
                    "Share token not found on this cc-switch. Copy the latest API Key from Share > Connect Info.",
                )));
            }
        };

        let Some(primary_share) = Self::primary_share(db)? else {
            return Ok(Some(ShareTokenValidation::rejected(
                ShareTokenRejectReason::NotFound,
                "No share exists on this cc-switch.",
            )));
        };
        if share.id != primary_share.id {
            return Ok(Some(ShareTokenValidation::rejected(
                ShareTokenRejectReason::NotFound,
                "Share token belongs to a non-primary share on this cc-switch.",
            )));
        }

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
                    "Share token has expired. Extend the share expiration or create a new share.",
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

    pub fn update_api_key(
        db: &Arc<Database>,
        share_id: &str,
        api_key: &str,
    ) -> Result<ShareRecord, AppError> {
        let normalized = normalize_api_key(api_key)?;
        db.update_share_api_key(share_id, &normalized)?;
        let updated = db
            .get_share_by_id(share_id)?
            .ok_or_else(|| AppError::Message(format!("Share not found: {share_id}")))?;
        crate::tunnel::sync::schedule_sync_share(updated.clone(), db);
        Ok(updated)
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

    pub fn update_acl(
        db: &Arc<Database>,
        share_id: &str,
        owner_email: &str,
        shared_with_emails: Vec<String>,
    ) -> Result<ShareRecord, AppError> {
        let owner_email = normalize_email(owner_email)?;
        let shared_with_emails = normalize_email_list(shared_with_emails, &owner_email)?;
        db.update_share_acl(share_id, &owner_email, &shared_with_emails)?;
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

    fn primary_share(db: &Arc<Database>) -> Result<Option<ShareRecord>, AppError> {
        Ok(db.list_shares()?.into_iter().next())
    }

    fn generate_token() -> String {
        use rand::Rng;
        const CHARSET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        let mut rng = rand::thread_rng();
        (0..32)
            .map(|_| {
                let idx = rng.gen_range(0..CHARSET.len());
                CHARSET[idx] as char
            })
            .collect()
    }
}

pub struct PrepareShareParams {
    pub owner_email: String,
    pub description: Option<String>,
    pub for_sale: String,
    pub token_limit: i64,
    pub parallel_limit: i64,
    pub expires_in_secs: i64,
    pub subdomain: Option<String>,
    pub api_key: Option<String>,
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

fn normalize_api_key(value: &str) -> Result<String, AppError> {
    let value = value.trim();
    if value.len() < 8 || value.len() > 128 {
        return Err(AppError::Message(
            "API Key 长度必须在 8 到 128 个字符之间".to_string(),
        ));
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return Err(AppError::Message(
            "API Key 只能包含字母、数字、-、_、.".to_string(),
        ));
    }
    Ok(value.to_string())
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

fn normalize_email(value: &str) -> Result<String, AppError> {
    let value = value.trim().to_ascii_lowercase();
    if value.is_empty() || !value.contains('@') {
        return Err(AppError::Message("邮箱格式无效".to_string()));
    }
    Ok(value)
}

fn normalize_email_list(values: Vec<String>, owner_email: &str) -> Result<Vec<String>, AppError> {
    let mut result = Vec::new();
    for value in values {
        let email = normalize_email(&value)?;
        if email == owner_email || result.contains(&email) {
            continue;
        }
        result.push(email);
    }
    Ok(result)
}
