//! 流式健康检查日志 DAO

use crate::database::{lock_conn, Database};
use crate::error::AppError;
use crate::services::stream_check::{StreamCheckConfig, StreamCheckResult};

pub trait StreamCheckLogResult {
    fn status_for_log(&self) -> String;
    fn success_for_log(&self) -> bool;
    fn message_for_log(&self) -> &str;
    fn response_time_ms_for_log(&self) -> Option<u64>;
    fn http_status_for_log(&self) -> Option<u16>;
    fn model_used_for_log(&self) -> &str;
    fn retry_count_for_log(&self) -> u32;
    fn tested_at_for_log(&self) -> i64;
}

impl StreamCheckLogResult for StreamCheckResult {
    fn status_for_log(&self) -> String {
        format!("{:?}", self.status).to_lowercase()
    }
    fn success_for_log(&self) -> bool {
        self.success
    }
    fn message_for_log(&self) -> &str {
        &self.message
    }
    fn response_time_ms_for_log(&self) -> Option<u64> {
        self.response_time_ms
    }
    fn http_status_for_log(&self) -> Option<u16> {
        self.http_status
    }
    fn model_used_for_log(&self) -> &str {
        &self.model_used
    }
    fn retry_count_for_log(&self) -> u32 {
        self.retry_count
    }
    fn tested_at_for_log(&self) -> i64 {
        self.tested_at
    }
}

impl StreamCheckLogResult for crate::services::model_test::StreamCheckResult {
    fn status_for_log(&self) -> String {
        format!("{:?}", self.status).to_lowercase()
    }
    fn success_for_log(&self) -> bool {
        self.success
    }
    fn message_for_log(&self) -> &str {
        &self.message
    }
    fn response_time_ms_for_log(&self) -> Option<u64> {
        self.response_time_ms
    }
    fn http_status_for_log(&self) -> Option<u16> {
        self.http_status
    }
    fn model_used_for_log(&self) -> &str {
        &self.model_used
    }
    fn retry_count_for_log(&self) -> u32 {
        self.retry_count
    }
    fn tested_at_for_log(&self) -> i64 {
        self.tested_at
    }
}

impl Database {
    /// 保存流式检查日志
    pub fn save_stream_check_log<R: StreamCheckLogResult>(
        &self,
        provider_id: &str,
        provider_name: &str,
        app_type: &str,
        result: &R,
    ) -> Result<i64, AppError> {
        let conn = lock_conn!(self.conn);

        conn.execute(
            "INSERT INTO stream_check_logs 
             (provider_id, provider_name, app_type, status, success, message, 
              response_time_ms, http_status, model_used, retry_count, tested_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                provider_id,
                provider_name,
                app_type,
                result.status_for_log(),
                result.success_for_log(),
                result.message_for_log(),
                result.response_time_ms_for_log().map(|t| t as i64),
                result.http_status_for_log().map(|s| s as i64),
                result.model_used_for_log(),
                result.retry_count_for_log() as i64,
                result.tested_at_for_log(),
            ],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;

        Ok(conn.last_insert_rowid())
    }

    /// 获取流式检查配置
    pub fn get_stream_check_config(&self) -> Result<StreamCheckConfig, AppError> {
        match self.get_setting("stream_check_config")? {
            Some(json) => serde_json::from_str(&json)
                .map_err(|e| AppError::Message(format!("解析配置失败: {e}"))),
            None => Ok(StreamCheckConfig::default()),
        }
    }

    /// Delete stream check logs older than `retain_days` days.
    /// Returns the number of deleted rows.
    pub fn cleanup_old_stream_check_logs(&self, retain_days: i64) -> Result<u64, AppError> {
        let cutoff = chrono::Utc::now().timestamp() - retain_days * 86400;
        let conn = lock_conn!(self.conn);
        let deleted = conn
            .execute(
                "DELETE FROM stream_check_logs WHERE tested_at < ?1",
                [cutoff],
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        if deleted > 0 {
            log::info!("Cleaned up {deleted} stream_check_logs older than {retain_days} days");
        }
        Ok(deleted as u64)
    }

    /// 保存流式检查配置
    pub fn save_stream_check_config(&self, config: &StreamCheckConfig) -> Result<(), AppError> {
        let json = serde_json::to_string(config)
            .map_err(|e| AppError::Message(format!("序列化配置失败: {e}")))?;
        self.set_setting("stream_check_config", &json)
    }
}
