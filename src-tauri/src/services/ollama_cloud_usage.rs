//! Ollama Cloud 账户信息查询
//!
//! 使用 API Key (Bearer token) 调用 `POST https://ollama.com/api/me`，
//! 获取订阅等级 (Plan) 和账号邮箱 (Email)。
//! 返回 `SubscriptionQuota`，复用 OAuth quota 缓存体系。

use std::time::Duration;

use crate::services::subscription::{CredentialStatus, QuotaTier, SubscriptionQuota};

/// 调用 `/api/me` 获取 Ollama 账户信息，返回 `SubscriptionQuota`。
///
/// `credential_message` 携带 Plan 等级（如 "pro"），
/// `tiers[0].name` 携带邮箱地址，`utilization` 固定为 0（无用量百分比数据）。
pub async fn get_ollama_cloud_account_info(api_key: &str) -> SubscriptionQuota {
    if api_key.trim().is_empty() {
        return SubscriptionQuota::error(
            "ollama_cloud",
            CredentialStatus::NotFound,
            "Ollama API Key is empty".to_string(),
        );
    }

    let client = crate::proxy::http_client::get();
    let resp = client
        .post("https://ollama.com/api/me")
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .timeout(Duration::from_secs(15))
        .send()
        .await;

    let resp = match resp {
        Ok(r) => r,
        Err(e) => {
            return SubscriptionQuota::error(
                "ollama_cloud",
                CredentialStatus::Valid,
                format!("Ollama network error: {e}"),
            )
        }
    };

    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return SubscriptionQuota::error(
            "ollama_cloud",
            CredentialStatus::Expired,
            format!("Ollama API Key invalid (HTTP {status})"),
        );
    }
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return SubscriptionQuota::error(
            "ollama_cloud",
            CredentialStatus::Valid,
            format!("Ollama API error (HTTP {status}): {body}"),
        );
    }

    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            return SubscriptionQuota::error(
                "ollama_cloud",
                CredentialStatus::ParseError,
                format!("Failed to parse Ollama response: {e}"),
            )
        }
    };

    let plan = body
        .get("Plan")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let email = body
        .get("Email")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let period_end = body
        .get("SubscriptionPeriodEnd")
        .and_then(|v| v.get("Time"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    SubscriptionQuota {
        tool: "ollama_cloud".to_string(),
        credential_status: CredentialStatus::Valid,
        credential_message: Some(plan),
        success: true,
        tiers: vec![QuotaTier {
            name: email,
            utilization: 0.0,
            resets_at: period_end,
            used: None,
            limit: None,
            unit: None,
            used_value_usd: None,
            max_value_usd: None,
        }],
        extra_usage: None,
        error: None,
        queried_at: Some(now_millis()),
        failure: None,
    }
}

fn now_millis() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
