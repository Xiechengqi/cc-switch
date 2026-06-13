use crate::database::{lock_conn, Database};
use crate::error::AppError;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

const SHARE_APP_TYPES: &[&str] = &["claude", "codex", "gemini"];

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ShareAppAccess {
    #[serde(default)]
    pub shared_with_emails: Vec<String>,
    #[serde(default = "default_market_access_mode_string")]
    pub market_access_mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareAppSettings {
    #[serde(default = "default_share_app_for_sale_string")]
    pub for_sale: String,
    #[serde(default = "default_sale_market_kind_string")]
    pub sale_market_kind: String,
    #[serde(default = "default_market_access_mode_string")]
    pub market_access_mode: String,
    #[serde(default)]
    pub shared_with_emails: Vec<String>,
    #[serde(default)]
    pub token_limit: i64,
    #[serde(default)]
    pub parallel_limit: i64,
    #[serde(default)]
    pub expires_at: String,
}

impl Default for ShareAppSettings {
    fn default() -> Self {
        Self {
            for_sale: default_share_app_for_sale_string(),
            sale_market_kind: default_sale_market_kind_string(),
            market_access_mode: default_market_access_mode_string(),
            shared_with_emails: Vec::new(),
            token_limit: -1,
            parallel_limit: 3,
            expires_at: String::new(),
        }
    }
}

fn default_market_access_mode_string() -> String {
    "selected".to_string()
}

fn default_share_app_for_sale_string() -> String {
    "No".to_string()
}

fn default_sale_market_kind_string() -> String {
    "token".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareRecord {
    pub id: String,
    pub name: String,
    pub owner_email: String,
    pub shared_with_emails: Vec<String>,
    pub market_access_mode: String,
    #[serde(default)]
    pub access_by_app: HashMap<String, ShareAppAccess>,
    #[serde(default)]
    pub app_settings: HashMap<String, ShareAppSettings>,
    pub for_sale_official_price_percent_by_app: HashMap<String, u16>,
    pub description: Option<String>,
    pub for_sale: String,
    #[serde(default = "default_sale_market_kind_string")]
    pub sale_market_kind: String,
    /// P8: 多 app share。一个 share 可同时给 claude / codex / gemini 分别绑定 0/1 个
    /// provider。键为 app_type，值为该 slot 当前绑定的 provider id。slot 为空 = 该
    /// app 不可用，请求路径会拒绝并 emit share-needs-rebind。
    ///
    /// 数据源：`share_provider_bindings` 侧表。`shares` 表本身不再持有 app_type /
    /// provider_id 字段。DAO 在每次 SELECT 后用 `load_share_bindings` 填充本字段。
    #[serde(default)]
    pub bindings: HashMap<String, String>,
    /// P17: 标记哪些 slot 是"动态绑定"——provider_id 仍然是当前激活的具体 id，但
    /// 当用户切换该 app 的 active provider 时，cc-switch 会扫表把这些 slot 也指过去。
    /// 持久化为 share_provider_bindings.dynamic = 1。包含 app_type 字符串集合；不
    /// 在集合内的 app 视为固定绑定。
    #[serde(default)]
    pub dynamic_apps: HashSet<String>,
    /// 历史遗留字段，保留为空字符串。请求路径不读取此字段——上游 API key 始终
    /// 在请求时从绑定的 provider 实时读取。schema NOT NULL 约束要求非空，所以
    /// 用 `""` 占位。可视为已废弃，未来 schema 重整可移除。
    pub api_key: String,
    pub settings_config: Option<String>,
    pub token_limit: i64,
    pub parallel_limit: i64,
    pub tokens_used: i64,
    pub requests_count: i64,
    pub expires_at: String,
    pub subdomain: Option<String>,
    pub tunnel_url: Option<String>,
    pub status: String,
    pub auto_start: bool,
    pub created_at: String,
    pub last_used_at: Option<String>,
}

impl ShareRecord {
    /// 该 share 支持的 app_type 列表（按字母序，方便日志/测试断言）。
    pub fn supported_apps(&self) -> Vec<String> {
        let mut apps: Vec<String> = self.bindings.keys().cloned().collect();
        apps.sort();
        apps
    }

    /// 主 app：用于 router back-compat 字段（`ShareTunnelMetadata.app_type` 等）。
    /// 优先级 claude > codex > gemini > 其它字母序；无绑定时返回 `None`。
    pub fn primary_app(&self) -> Option<String> {
        const PRIORITY: &[&str] = &["claude", "codex", "gemini"];
        for app in PRIORITY {
            if self.bindings.contains_key(*app) {
                return Some((*app).to_string());
            }
        }
        self.supported_apps().into_iter().next()
    }

    /// 主 provider：对应 `primary_app()` 的 provider id。供 router back-compat 使用。
    pub fn primary_provider_id(&self) -> Option<String> {
        self.primary_app()
            .and_then(|app| self.bindings.get(&app).cloned())
    }

    pub fn effective_access_by_app(&self) -> HashMap<String, ShareAppAccess> {
        if !self.access_by_app.is_empty() {
            return self.access_by_app.clone();
        }
        derive_access_by_app(
            &self.bindings,
            &self.shared_with_emails,
            &self.market_access_mode,
        )
    }

    pub fn effective_app_settings(&self) -> HashMap<String, ShareAppSettings> {
        derive_app_settings(
            &self.bindings,
            &self.effective_access_by_app(),
            &self.app_settings,
            &self.for_sale,
            &self.sale_market_kind,
            &self.market_access_mode,
            self.token_limit,
            self.parallel_limit,
            &self.expires_at,
        )
    }
}

#[allow(clippy::too_many_arguments)]
pub fn derive_app_settings(
    bindings: &HashMap<String, String>,
    access_by_app: &HashMap<String, ShareAppAccess>,
    app_settings: &HashMap<String, ShareAppSettings>,
    for_sale: &str,
    sale_market_kind: &str,
    market_access_mode: &str,
    token_limit: i64,
    parallel_limit: i64,
    expires_at: &str,
) -> HashMap<String, ShareAppSettings> {
    let apps: Vec<&str> = if bindings.is_empty() {
        SHARE_APP_TYPES.to_vec()
    } else {
        SHARE_APP_TYPES
            .iter()
            .copied()
            .filter(|app| bindings.contains_key(*app))
            .collect()
    };
    apps.into_iter()
        .map(|app| {
            let access = access_by_app.get(app);
            let mut settings = app_settings.get(app).cloned().unwrap_or_else(|| ShareAppSettings {
                for_sale: for_sale.to_string(),
                sale_market_kind: sale_market_kind.to_string(),
                market_access_mode: access
                    .map(|entry| entry.market_access_mode.clone())
                    .unwrap_or_else(|| market_access_mode.to_string()),
                shared_with_emails: access
                    .map(|entry| entry.shared_with_emails.clone())
                    .unwrap_or_default(),
                token_limit,
                parallel_limit,
                expires_at: expires_at.to_string(),
            });
            settings.market_access_mode = access
                .map(|entry| entry.market_access_mode.clone())
                .unwrap_or(settings.market_access_mode);
            settings.shared_with_emails = access
                .map(|entry| entry.shared_with_emails.clone())
                .unwrap_or(settings.shared_with_emails);
            (app.to_string(), settings)
        })
        .collect()
}

pub fn derive_access_by_app(
    bindings: &HashMap<String, String>,
    shared_with_emails: &[String],
    market_access_mode: &str,
) -> HashMap<String, ShareAppAccess> {
    let apps: Vec<&str> = if bindings.is_empty() {
        SHARE_APP_TYPES.to_vec()
    } else {
        SHARE_APP_TYPES
            .iter()
            .copied()
            .filter(|app| bindings.contains_key(*app))
            .collect()
    };
    apps.into_iter()
        .map(|app| {
            (
                app.to_string(),
                ShareAppAccess {
                    shared_with_emails: shared_with_emails.to_vec(),
                    market_access_mode: market_access_mode.to_string(),
                },
            )
        })
        .collect()
}

pub fn legacy_acl_from_access_by_app(
    access_by_app: &HashMap<String, ShareAppAccess>,
) -> (Vec<String>, String) {
    if access_by_app.is_empty() {
        return (Vec::new(), "selected".to_string());
    }
    let all_apps_open = access_by_app
        .values()
        .all(|access| access.market_access_mode == "all");
    let mut seen = HashSet::new();
    let mut emails = Vec::new();
    for access in access_by_app.values() {
        for email in &access.shared_with_emails {
            let email = email.trim().to_ascii_lowercase();
            if !email.is_empty() && seen.insert(email.clone()) {
                emails.push(email);
            }
        }
    }
    emails.sort();
    (
        emails,
        if all_apps_open { "all" } else { "selected" }.to_string(),
    )
}

/// Share 绑定 provider 的审计历史条目。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareBindingHistoryEntry {
    pub id: i64,
    /// `None` 表示该 slot 此前为空（首次绑定）。
    pub old_provider_id: Option<String>,
    /// `None` 表示这是一次 "清空 slot" 事件（解绑）。
    pub new_provider_id: Option<String>,
    pub app_type: String,
    pub changed_at: String,
}

impl Database {
    const SHARE_SELECT_COLUMNS: &str = "id, name, owner_email, shared_with_emails_json, market_access_mode, access_by_app_json, app_settings_json, for_sale_official_price_percent_json, description, for_sale, sale_market_kind, api_key, settings_config, token_limit, parallel_limit, tokens_used, requests_count, expires_at, subdomain, tunnel_url, status, auto_start, created_at, last_used_at";

    pub fn create_share(&self, share: &ShareRecord) -> Result<(), AppError> {
        let mut conn = lock_conn!(self.conn);
        let tx = conn
            .transaction()
            .map_err(|e| AppError::Database(e.to_string()))?;
        tx.execute(
            "INSERT INTO shares (id, name, owner_email, shared_with_emails_json, market_access_mode, access_by_app_json, app_settings_json, for_sale_official_price_percent_json, description, for_sale, sale_market_kind, api_key,
             settings_config, token_limit, parallel_limit, tokens_used, requests_count, expires_at,
             subdomain, tunnel_url, status, auto_start, created_at, last_used_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24)",
            params![
                share.id,
                share.name,
                share.owner_email,
                serde_json::to_string(&share.shared_with_emails)
                    .map_err(|e| AppError::Database(e.to_string()))?,
                share.market_access_mode,
                serde_json::to_string(&share.effective_access_by_app())
                    .map_err(|e| AppError::Database(e.to_string()))?,
                serde_json::to_string(&share.effective_app_settings())
                    .map_err(|e| AppError::Database(e.to_string()))?,
                serde_json::to_string(&share.for_sale_official_price_percent_by_app)
                    .map_err(|e| AppError::Database(e.to_string()))?,
                share.description,
                share.for_sale,
                share.sale_market_kind,
                share.api_key,
                share.settings_config,
                share.token_limit,
                share.parallel_limit,
                share.tokens_used,
                share.requests_count,
                share.expires_at,
                share.subdomain,
                share.tunnel_url,
                share.status,
                share.auto_start,
                share.created_at,
                share.last_used_at,
            ],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        // 同事务写入所有 bindings，确保 share 行和 binding 行原子可见。
        for (app_type, provider_id) in &share.bindings {
            let dynamic_flag: i64 = if share.dynamic_apps.contains(app_type) {
                1
            } else {
                0
            };
            tx.execute(
                "INSERT INTO share_provider_bindings (share_id, app_type, provider_id, dynamic)
                 VALUES (?1, ?2, ?3, ?4)",
                params![share.id, app_type, provider_id, dynamic_flag],
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        }
        tx.commit().map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn get_share_by_id(&self, id: &str) -> Result<Option<ShareRecord>, AppError> {
        let conn = lock_conn!(self.conn);
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {} FROM shares WHERE id = ?1",
                Self::SHARE_SELECT_COLUMNS
            ))
            .map_err(|e| AppError::Database(e.to_string()))?;
        let mut rows = stmt
            .query(params![id])
            .map_err(|e| AppError::Database(e.to_string()))?;
        match rows.next().map_err(|e| AppError::Database(e.to_string()))? {
            Some(row) => {
                let mut share = Self::row_to_share(row)?;
                let (bindings, dynamic_apps) = Self::load_share_bindings_on_conn(&conn, &share.id)?;
                share.bindings = bindings;
                share.dynamic_apps = dynamic_apps;
                Ok(Some(share))
            }
            None => Ok(None),
        }
    }

    pub fn list_shares(&self) -> Result<Vec<ShareRecord>, AppError> {
        let conn = lock_conn!(self.conn);
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {} FROM shares ORDER BY created_at DESC",
                Self::SHARE_SELECT_COLUMNS
            ))
            .map_err(|e| AppError::Database(e.to_string()))?;
        let mut rows = stmt
            .query([])
            .map_err(|e| AppError::Database(e.to_string()))?;
        let mut result = Vec::new();
        while let Some(row) = rows.next().map_err(|e| AppError::Database(e.to_string()))? {
            let mut share = Self::row_to_share(row)?;
            let (bindings, dynamic_apps) = Self::load_share_bindings_on_conn(&conn, &share.id)?;
            share.bindings = bindings;
            share.dynamic_apps = dynamic_apps;
            result.push(share);
        }
        Ok(result)
    }

    /// 列出绑定到指定 provider 的活跃 share，并附带命中的 app_type slot。
    ///
    /// 多 app share 模式下，删除 provider 时必须确认没有 share 还在绑它（任一 slot），
    /// 否则那条 slot 上的请求路径会拿到 NoAvailableProvider。每个返回项含：
    /// - share record（已 hydrate bindings）
    /// - 该 provider 命中的 app_type slot
    pub fn list_active_shares_bound_to_provider(
        &self,
        provider_id: &str,
    ) -> Result<Vec<(ShareRecord, String)>, AppError> {
        let conn = lock_conn!(self.conn);
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {}, spb.app_type
                 FROM shares
                 JOIN share_provider_bindings spb ON spb.share_id = shares.id
                 WHERE spb.provider_id = ?1 AND shares.status != 'deleted'
                 ORDER BY shares.created_at DESC",
                Self::SHARE_SELECT_COLUMNS
                    .split(", ")
                    .map(|c| format!("shares.{c}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
            .map_err(|e| AppError::Database(e.to_string()))?;
        let mut rows = stmt
            .query(params![provider_id])
            .map_err(|e| AppError::Database(e.to_string()))?;
        let mut result = Vec::new();
        while let Some(row) = rows.next().map_err(|e| AppError::Database(e.to_string()))? {
            let mut share = Self::row_to_share(row)?;
            // 最后一列是 JOIN 出来的 app_type。row_to_share 只读前 N 列。
            let app_type: String = row
                .get::<_, String>(Self::share_column_count())
                .map_err(|e| AppError::Database(e.to_string()))?;
            let (bindings, dynamic_apps) = Self::load_share_bindings_on_conn(&conn, &share.id)?;
            share.bindings = bindings;
            share.dynamic_apps = dynamic_apps;
            result.push((share, app_type));
        }
        Ok(result)
    }

    pub fn update_share_status(&self, id: &str, status: &str) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "UPDATE shares SET status = ?2 WHERE id = ?1",
            params![id, status],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn update_share_tunnel(
        &self,
        id: &str,
        subdomain: &str,
        tunnel_url: &str,
    ) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "UPDATE shares SET subdomain = ?2, tunnel_url = ?3 WHERE id = ?1",
            params![id, subdomain, tunnel_url],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn update_share_subdomain(&self, id: &str, subdomain: &str) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "UPDATE shares SET subdomain = ?2, tunnel_url = NULL WHERE id = ?1",
            params![id, subdomain],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn clear_share_tunnel(&self, id: &str) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "UPDATE shares SET tunnel_url = NULL WHERE id = ?1",
            params![id],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn increment_share_requests(&self, id: &str) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        let last_used_at = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE shares SET requests_count = requests_count + 1, last_used_at = ?2 WHERE id = ?1",
            params![id, last_used_at],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    /// Atomically increment token usage counters. Returns new tokens_used.
    pub fn increment_share_tokens(&self, id: &str, tokens_delta: i64) -> Result<i64, AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "UPDATE shares SET tokens_used = tokens_used + ?2 WHERE id = ?1",
            params![id, tokens_delta],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        let new_used: i64 = conn
            .query_row(
                "SELECT tokens_used FROM shares WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(new_used)
    }

    pub fn reset_share_usage(&self, id: &str) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "UPDATE shares
             SET tokens_used = 0,
                 requests_count = 0,
                 last_used_at = NULL
             WHERE id = ?1",
            params![id],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn update_share_token_limit(&self, id: &str, token_limit: i64) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "UPDATE shares SET token_limit = ?2 WHERE id = ?1",
            params![id, token_limit],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn update_share_parallel_limit(
        &self,
        id: &str,
        parallel_limit: i64,
    ) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "UPDATE shares SET parallel_limit = ?2 WHERE id = ?1",
            params![id, parallel_limit],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn update_share_description(
        &self,
        id: &str,
        description: Option<&str>,
    ) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "UPDATE shares SET description = ?2 WHERE id = ?1",
            params![id, description],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn update_share_acl(
        &self,
        id: &str,
        owner_email: &str,
        shared_with_emails: &[String],
        market_access_mode: &str,
        access_by_app: &HashMap<String, ShareAppAccess>,
        sale_market_kind: &str,
    ) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        let shared_with_emails_json = serde_json::to_string(shared_with_emails)
            .map_err(|e| AppError::Database(e.to_string()))?;
        let access_by_app_json =
            serde_json::to_string(access_by_app).map_err(|e| AppError::Database(e.to_string()))?;
        conn.execute(
            "UPDATE shares
                 SET name = ?2,
                     owner_email = ?2,
                     shared_with_emails_json = ?3,
                     market_access_mode = ?4,
                     access_by_app_json = ?5,
                     sale_market_kind = ?6
                 WHERE id = ?1",
            params![
                id,
                owner_email,
                shared_with_emails_json,
                market_access_mode,
                access_by_app_json,
                sale_market_kind
            ],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn update_share_app_settings(
        &self,
        id: &str,
        app_settings: &HashMap<String, ShareAppSettings>,
    ) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        let app_settings_json =
            serde_json::to_string(app_settings).map_err(|e| AppError::Database(e.to_string()))?;
        conn.execute(
            "UPDATE shares SET app_settings_json = ?2 WHERE id = ?1",
            params![id, app_settings_json],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn update_shares_owner_email(
        &self,
        old_email: &str,
        new_email: &str,
    ) -> Result<usize, AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "UPDATE shares
             SET name = ?2,
                 owner_email = ?2
             WHERE owner_email = ?1",
            params![old_email, new_email],
        )
        .map_err(|e| AppError::Database(e.to_string()))
    }

    pub fn update_share_for_sale(&self, id: &str, for_sale: &str) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "UPDATE shares SET for_sale = ?2 WHERE id = ?1",
            params![id, for_sale],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn update_share_for_sale_official_price_percent_by_app(
        &self,
        id: &str,
        pricing: &HashMap<String, u16>,
    ) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        let pricing_json =
            serde_json::to_string(pricing).map_err(|e| AppError::Database(e.to_string()))?;
        conn.execute(
            "UPDATE shares SET for_sale_official_price_percent_json = ?2 WHERE id = ?1",
            params![id, pricing_json],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn update_share_expires_at(&self, id: &str, expires_at: &str) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "UPDATE shares SET expires_at = ?2 WHERE id = ?1",
            params![id, expires_at],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn update_share_auto_start(&self, id: &str, auto_start: bool) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "UPDATE shares SET auto_start = ?2 WHERE id = ?1",
            params![id, auto_start],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    /// 改绑 share 到新的 provider。
    ///
    /// P8：读单 slot 的当前绑定 provider id（不存在返回 None）。
    pub fn get_share_binding(
        &self,
        share_id: &str,
        app_type: &str,
    ) -> Result<Option<String>, AppError> {
        let conn = lock_conn!(self.conn);
        let mut stmt = conn
            .prepare(
                "SELECT provider_id FROM share_provider_bindings
                 WHERE share_id = ?1 AND app_type = ?2",
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        let mut rows = stmt
            .query(params![share_id, app_type])
            .map_err(|e| AppError::Database(e.to_string()))?;
        match rows.next().map_err(|e| AppError::Database(e.to_string()))? {
            Some(row) => Ok(Some(
                row.get(0).map_err(|e| AppError::Database(e.to_string()))?,
            )),
            None => Ok(None),
        }
    }

    /// P8：写单 slot 绑定 + 乐观锁 + 写审计，单事务原子完成。
    ///
    /// 三个语义合并在一个 API 里以便事务原子：
    ///   - 老 slot 为空 + 新 provider 非空 → INSERT（首次绑定）
    ///   - 老 slot 非空 + 新 provider 非空 → UPDATE（改绑）
    ///   - 老 slot 非空 + 新 provider 为空 → DELETE（清空 slot）
    ///   - 老 slot 为空 + 新 provider 为空 → no-op（直接返回 Err，避免误触发）
    ///
    /// `expected_old_provider_id` 是调用方读到的"老 provider id"快照；写入时做 CAS，
    /// 中间被别处改了就拒绝（B-1 乐观锁）。
    ///
    /// 任一成功路径都写一行 share_binding_history，便于事后追溯。
    pub fn upsert_share_binding_with_history(
        &self,
        share_id: &str,
        app_type: &str,
        expected_old_provider_id: Option<&str>,
        new_provider_id: Option<&str>,
    ) -> Result<(), AppError> {
        if expected_old_provider_id.is_none() && new_provider_id.is_none() {
            return Err(AppError::Message(
                "改绑失败：当前 slot 已经为空，无需操作".to_string(),
            ));
        }
        let mut conn = lock_conn!(self.conn);
        let tx = conn
            .transaction()
            .map_err(|e| AppError::Database(e.to_string()))?;

        let affected = match (expected_old_provider_id, new_provider_id) {
            // INSERT
            (None, Some(new_pid)) => tx
                .execute(
                    "INSERT INTO share_provider_bindings (share_id, app_type, provider_id)
                     SELECT ?1, ?2, ?3
                     WHERE NOT EXISTS (
                         SELECT 1 FROM share_provider_bindings
                         WHERE share_id = ?1 AND app_type = ?2
                     )",
                    params![share_id, app_type, new_pid],
                )
                .map_err(|e| AppError::Database(e.to_string()))?,
            // UPDATE：手动改绑同时清掉 dynamic 标记。如果用户原本是动态绑定，主动选了
            // 一个固定 provider，则视为退出动态模式。
            (Some(old_pid), Some(new_pid)) => tx
                .execute(
                    "UPDATE share_provider_bindings SET provider_id = ?3, dynamic = 0
                     WHERE share_id = ?1 AND app_type = ?2 AND provider_id = ?4",
                    params![share_id, app_type, new_pid, old_pid],
                )
                .map_err(|e| AppError::Database(e.to_string()))?,
            // DELETE
            (Some(old_pid), None) => tx
                .execute(
                    "DELETE FROM share_provider_bindings
                     WHERE share_id = ?1 AND app_type = ?2 AND provider_id = ?3",
                    params![share_id, app_type, old_pid],
                )
                .map_err(|e| AppError::Database(e.to_string()))?,
            (None, None) => unreachable!(),
        };
        if affected == 0 {
            return Err(AppError::Message(
                "改绑失败：share slot 已被其他操作改动，请刷新后重试".to_string(),
            ));
        }
        // history.new_provider_id 是 NOT NULL TEXT，"解绑"事件用 "" 表示，读层译回 None。
        let history_new = new_provider_id.unwrap_or("");
        tx.execute(
            "INSERT INTO share_binding_history (share_id, old_provider_id, new_provider_id, app_type)
             VALUES (?1, ?2, ?3, ?4)",
            params![share_id, expected_old_provider_id, history_new, app_type],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        tx.commit().map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    /// 读 share 最近 N 条 binding 历史，按时间倒序。
    pub fn list_share_binding_history(
        &self,
        share_id: &str,
        limit: usize,
    ) -> Result<Vec<ShareBindingHistoryEntry>, AppError> {
        let conn = lock_conn!(self.conn);
        let mut stmt = conn
            .prepare(
                "SELECT id, old_provider_id, new_provider_id, app_type, changed_at
                 FROM share_binding_history WHERE share_id = ?1
                 ORDER BY changed_at DESC LIMIT ?2",
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        let rows = stmt
            .query_map(params![share_id, limit as i64], |row| {
                let raw_new: String = row.get(2)?;
                Ok(ShareBindingHistoryEntry {
                    id: row.get(0)?,
                    old_provider_id: row.get(1)?,
                    // "" 是"清空 slot"事件的 sentinel —— 见 upsert_share_binding_with_history。
                    new_provider_id: if raw_new.is_empty() {
                        None
                    } else {
                        Some(raw_new)
                    },
                    app_type: row.get(3)?,
                    changed_at: row.get(4)?,
                })
            })
            .map_err(|e| AppError::Database(e.to_string()))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| AppError::Database(e.to_string()))?);
        }
        Ok(out)
    }

    pub fn delete_share(&self, id: &str) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute("DELETE FROM shares WHERE id = ?1", params![id])
            .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    /// Mark all expired shares as 'expired'.
    pub fn expire_shares(&self) -> Result<u32, AppError> {
        let conn = lock_conn!(self.conn);
        let count = conn
            .execute(
                "UPDATE shares SET status = 'expired'
                 WHERE status = 'active' AND expires_at < datetime('now')",
                [],
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(count as u32)
    }

    fn row_to_share(row: &rusqlite::Row) -> Result<ShareRecord, AppError> {
        // P8 后字段顺序：见 SHARE_SELECT_COLUMNS。app_type / provider_id 已被剥离到
        // share_provider_bindings 侧表。bindings 字段由调用方在 row_to_share 之后
        // 通过 load_share_bindings_on_conn 填充。
        let shared_with_emails = serde_json::from_str::<Vec<String>>(
            &row.get::<_, String>(3)
                .map_err(|e| AppError::Database(e.to_string()))?,
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        let market_access_mode: String =
            row.get(4).map_err(|e| AppError::Database(e.to_string()))?;
        let access_by_app = serde_json::from_str::<HashMap<String, ShareAppAccess>>(
            &row.get::<_, String>(5)
                .map_err(|e| AppError::Database(e.to_string()))?,
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        let app_settings = serde_json::from_str::<HashMap<String, ShareAppSettings>>(
            &row.get::<_, String>(6)
                .map_err(|e| AppError::Database(e.to_string()))?,
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(ShareRecord {
            id: row.get(0).map_err(|e| AppError::Database(e.to_string()))?,
            name: row.get(1).map_err(|e| AppError::Database(e.to_string()))?,
            owner_email: row.get(2).map_err(|e| AppError::Database(e.to_string()))?,
            shared_with_emails,
            market_access_mode,
            access_by_app,
            app_settings,
            for_sale_official_price_percent_by_app: serde_json::from_str::<HashMap<String, u16>>(
                &row.get::<_, String>(7)
                    .map_err(|e| AppError::Database(e.to_string()))?,
            )
            .map_err(|e| AppError::Database(e.to_string()))?,
            description: row.get(8).map_err(|e| AppError::Database(e.to_string()))?,
            for_sale: row.get(9).map_err(|e| AppError::Database(e.to_string()))?,
            sale_market_kind: row.get(10).map_err(|e| AppError::Database(e.to_string()))?,
            bindings: HashMap::new(),
            dynamic_apps: HashSet::new(),
            api_key: row.get(11).map_err(|e| AppError::Database(e.to_string()))?,
            settings_config: row.get(12).map_err(|e| AppError::Database(e.to_string()))?,
            token_limit: row.get(13).map_err(|e| AppError::Database(e.to_string()))?,
            parallel_limit: row.get(14).map_err(|e| AppError::Database(e.to_string()))?,
            tokens_used: row.get(15).map_err(|e| AppError::Database(e.to_string()))?,
            requests_count: row.get(16).map_err(|e| AppError::Database(e.to_string()))?,
            expires_at: row.get(17).map_err(|e| AppError::Database(e.to_string()))?,
            subdomain: row.get(18).map_err(|e| AppError::Database(e.to_string()))?,
            tunnel_url: row.get(19).map_err(|e| AppError::Database(e.to_string()))?,
            status: row.get(20).map_err(|e| AppError::Database(e.to_string()))?,
            auto_start: row.get(21).map_err(|e| AppError::Database(e.to_string()))?,
            created_at: row.get(22).map_err(|e| AppError::Database(e.to_string()))?,
            last_used_at: row.get(23).map_err(|e| AppError::Database(e.to_string()))?,
        })
    }

    /// SHARE_SELECT_COLUMNS 的列数（用于 JOIN 查询定位附加列下标）。
    const fn share_column_count() -> usize {
        24
    }

    /// 读侧表中 share_id 对应的所有 binding（app_type → provider_id）。
    /// 把所有 `app_type` 上 dynamic=1 的 slot 改成 `new_provider_id`。返回受影响
    /// 的 share_id 列表，供调用方触发 router 同步 / 前端事件。注意：partial UNIQUE
    /// 索引豁免了 dynamic=1 行，所以即便多个 share 都指向同一个 provider 也不会
    /// 触发 UNIQUE 冲突。
    pub fn redirect_dynamic_bindings_for_app(
        &self,
        app_type: &str,
        new_provider_id: &str,
    ) -> Result<Vec<String>, AppError> {
        let mut conn = lock_conn!(self.conn);
        let tx = conn
            .transaction()
            .map_err(|e| AppError::Database(e.to_string()))?;

        let affected_shares: Vec<(String, Option<String>)> = {
            let mut stmt = tx
                .prepare(
                    "SELECT share_id, provider_id FROM share_provider_bindings
                     WHERE app_type = ?1 AND dynamic = 1 AND provider_id != ?2",
                )
                .map_err(|e| AppError::Database(e.to_string()))?;
            let mut rows = stmt
                .query(params![app_type, new_provider_id])
                .map_err(|e| AppError::Database(e.to_string()))?;
            let mut acc = Vec::new();
            while let Some(row) = rows.next().map_err(|e| AppError::Database(e.to_string()))? {
                let sid: String = row.get(0).map_err(|e| AppError::Database(e.to_string()))?;
                let old: Option<String> =
                    row.get(1).map_err(|e| AppError::Database(e.to_string()))?;
                acc.push((sid, old));
            }
            acc
        };

        if affected_shares.is_empty() {
            tx.commit().map_err(|e| AppError::Database(e.to_string()))?;
            return Ok(Vec::new());
        }

        tx.execute(
            "UPDATE share_provider_bindings SET provider_id = ?2
             WHERE app_type = ?1 AND dynamic = 1 AND provider_id != ?2",
            params![app_type, new_provider_id],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;

        for (sid, old) in &affected_shares {
            tx.execute(
                "INSERT INTO share_binding_history (share_id, old_provider_id, new_provider_id, app_type)
                 VALUES (?1, ?2, ?3, ?4)",
                params![sid, old.as_deref(), new_provider_id, app_type],
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        }

        tx.commit().map_err(|e| AppError::Database(e.to_string()))?;
        Ok(affected_shares.into_iter().map(|(sid, _)| sid).collect())
    }

    fn load_share_bindings_on_conn(
        conn: &rusqlite::Connection,
        share_id: &str,
    ) -> Result<(HashMap<String, String>, HashSet<String>), AppError> {
        let mut stmt = conn
            .prepare(
                "SELECT app_type, provider_id, dynamic FROM share_provider_bindings WHERE share_id = ?1",
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        let mut rows = stmt
            .query(params![share_id])
            .map_err(|e| AppError::Database(e.to_string()))?;
        let mut bindings = HashMap::new();
        let mut dynamic_apps = HashSet::new();
        while let Some(row) = rows.next().map_err(|e| AppError::Database(e.to_string()))? {
            let app: String = row.get(0).map_err(|e| AppError::Database(e.to_string()))?;
            let pid: String = row.get(1).map_err(|e| AppError::Database(e.to_string()))?;
            let dynamic: i64 = row.get(2).map_err(|e| AppError::Database(e.to_string()))?;
            if dynamic != 0 {
                dynamic_apps.insert(app.clone());
            }
            bindings.insert(app, pid);
        }
        Ok((bindings, dynamic_apps))
    }
}
