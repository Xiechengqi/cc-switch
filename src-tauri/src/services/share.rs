use crate::database::{Database, ShareRecord};
use crate::error::AppError;
use std::sync::Arc;

pub struct ShareService;

impl ShareService {
    pub const MAX_DESCRIPTION_CHARS: usize = 200;
    pub const FOR_SALE_NO: &'static str = "No";
    pub const FOR_SALE_YES: &'static str = "Yes";

    pub fn prepare_create(
        name: String,
        description: Option<String>,
        for_sale: String,
        token_limit: i64,
        expires_in_secs: i64,
        subdomain: Option<String>,
        api_key: Option<String>,
    ) -> Result<ShareRecord, AppError> {
        let id = uuid::Uuid::new_v4().to_string();
        let subdomain = subdomain
            .map(|value| normalize_subdomain(&value))
            .transpose()?
            .unwrap_or_else(|| format!("share-{}", &id[..8]));
        let share_token = api_key
            .map(|value| normalize_api_key(&value))
            .transpose()?
            .unwrap_or_else(Self::generate_token);
        let now = chrono::Utc::now();
        let expires_at = now + chrono::Duration::seconds(expires_in_secs);
        let description = normalize_description(description)?;
        let for_sale = normalize_for_sale(&for_sale)?;

        let record = ShareRecord {
            id,
            name,
            description,
            for_sale,
            share_token,
            app_type: "proxy".to_string(),
            provider_id: None,
            api_key: String::new(),
            settings_config: None,
            token_limit,
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

    pub fn validate_token(
        db: &Arc<Database>,
        token: &str,
    ) -> Result<Option<ShareRecord>, AppError> {
        let share = match db.get_share_by_token(token)? {
            Some(s) => s,
            None => return Ok(None),
        };

        let Some(primary_share) = Self::primary_share(db)? else {
            return Ok(None);
        };
        if share.id != primary_share.id {
            return Ok(None);
        }

        if share.status != "active" {
            return Ok(None);
        }

        // Check expiry
        if let Ok(expires) = chrono::DateTime::parse_from_rfc3339(&share.expires_at) {
            if chrono::Utc::now() > expires {
                let _ = db.update_share_status(&share.id, "expired");
                return Ok(None);
            }
        }

        // Check token limit
        if share.tokens_used >= share.token_limit {
            let _ = db.update_share_status(&share.id, "exhausted");
            return Ok(None);
        }

        Ok(Some(share))
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
            if new_used >= share.token_limit {
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
        if token_limit <= 0 {
            return Err(AppError::Message("Token limit 必须大于 0".to_string()));
        }

        let share = db
            .get_share_by_id(share_id)?
            .ok_or_else(|| AppError::Message(format!("Share not found: {share_id}")))?;

        db.update_share_token_limit(share_id, token_limit)?;

        if share.tokens_used >= token_limit {
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

    pub fn cleanup_expired(db: &Arc<Database>) -> Result<u32, AppError> {
        db.expire_shares()
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
        _ => Err(AppError::Message("For Sale 只能是 Yes 或 No".to_string())),
    }
}
