use crate::database::{lock_conn, Database};
use crate::error::AppError;
use rusqlite::params;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareRecord {
    pub id: String,
    pub name: String,
    pub owner_email: String,
    pub shared_with_emails: Vec<String>,
    pub description: Option<String>,
    pub for_sale: String,
    pub share_token: String,
    pub app_type: String,
    pub provider_id: Option<String>,
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
    pub created_at: String,
    pub last_used_at: Option<String>,
}

impl Database {
    const SHARE_SELECT_COLUMNS: &str = "id, name, owner_email, shared_with_emails_json, description, for_sale, share_token, app_type, provider_id, api_key, settings_config, token_limit, parallel_limit, tokens_used, requests_count, expires_at, subdomain, tunnel_url, status, created_at, last_used_at";

    pub fn create_share(&self, share: &ShareRecord) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "INSERT INTO shares (id, name, owner_email, shared_with_emails_json, description, for_sale, share_token, app_type, provider_id, api_key,
             settings_config, token_limit, parallel_limit, tokens_used, requests_count, expires_at,
             subdomain, tunnel_url, status, created_at, last_used_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)",
            params![
                share.id,
                share.name,
                share.owner_email,
                serde_json::to_string(&share.shared_with_emails)
                    .map_err(|e| AppError::Database(e.to_string()))?,
                share.description,
                share.for_sale,
                share.share_token,
                share.app_type,
                share.provider_id,
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
                share.created_at,
                share.last_used_at,
            ],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
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
            Some(row) => Ok(Some(Self::row_to_share(row)?)),
            None => Ok(None),
        }
    }

    pub fn get_share_by_token(&self, token: &str) -> Result<Option<ShareRecord>, AppError> {
        let conn = lock_conn!(self.conn);
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {} FROM shares WHERE share_token = ?1",
                Self::SHARE_SELECT_COLUMNS
            ))
            .map_err(|e| AppError::Database(e.to_string()))?;
        let mut rows = stmt
            .query(params![token])
            .map_err(|e| AppError::Database(e.to_string()))?;
        match rows.next().map_err(|e| AppError::Database(e.to_string()))? {
            Some(row) => Ok(Some(Self::row_to_share(row)?)),
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
            result.push(Self::row_to_share(row)?);
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

    pub fn update_share_api_key(&self, id: &str, api_key: &str) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "UPDATE shares SET share_token = ?2 WHERE id = ?1",
            params![id, api_key],
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
    ) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        let shared_with_emails_json =
            serde_json::to_string(shared_with_emails).map_err(|e| AppError::Database(e.to_string()))?;
        conn.execute(
            "UPDATE shares
             SET name = ?2,
                 owner_email = ?2,
                 shared_with_emails_json = ?3
             WHERE id = ?1",
            params![id, owner_email, shared_with_emails_json],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
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

    pub fn update_share_expires_at(&self, id: &str, expires_at: &str) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "UPDATE shares SET expires_at = ?2 WHERE id = ?1",
            params![id, expires_at],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
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
        Ok(ShareRecord {
            id: row.get(0).map_err(|e| AppError::Database(e.to_string()))?,
            name: row.get(1).map_err(|e| AppError::Database(e.to_string()))?,
            owner_email: row.get(2).map_err(|e| AppError::Database(e.to_string()))?,
            shared_with_emails: serde_json::from_str::<Vec<String>>(
                &row.get::<_, String>(3)
                    .map_err(|e| AppError::Database(e.to_string()))?,
            )
            .map_err(|e| AppError::Database(e.to_string()))?,
            description: row.get(4).map_err(|e| AppError::Database(e.to_string()))?,
            for_sale: row.get(5).map_err(|e| AppError::Database(e.to_string()))?,
            share_token: row.get(6).map_err(|e| AppError::Database(e.to_string()))?,
            app_type: row.get(7).map_err(|e| AppError::Database(e.to_string()))?,
            provider_id: row.get(8).map_err(|e| AppError::Database(e.to_string()))?,
            api_key: row.get(9).map_err(|e| AppError::Database(e.to_string()))?,
            settings_config: row.get(10).map_err(|e| AppError::Database(e.to_string()))?,
            token_limit: row.get(11).map_err(|e| AppError::Database(e.to_string()))?,
            parallel_limit: row.get(12).map_err(|e| AppError::Database(e.to_string()))?,
            tokens_used: row.get(13).map_err(|e| AppError::Database(e.to_string()))?,
            requests_count: row.get(14).map_err(|e| AppError::Database(e.to_string()))?,
            expires_at: row.get(15).map_err(|e| AppError::Database(e.to_string()))?,
            subdomain: row.get(16).map_err(|e| AppError::Database(e.to_string()))?,
            tunnel_url: row.get(17).map_err(|e| AppError::Database(e.to_string()))?,
            status: row.get(18).map_err(|e| AppError::Database(e.to_string()))?,
            created_at: row.get(19).map_err(|e| AppError::Database(e.to_string()))?,
            last_used_at: row.get(20).map_err(|e| AppError::Database(e.to_string()))?,
        })
    }
}
