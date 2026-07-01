//! Ollama Cloud 账户信息查询
//!
//! 使用 API Key (Bearer token) 调用 `POST https://ollama.com/api/me`，
//! 获取订阅等级 (Plan) 和账号邮箱 (Email)。
//! 返回 `SubscriptionQuota`，复用 OAuth quota 缓存体系。

use std::time::Duration;

use crate::services::subscription::{
    CredentialStatus, QuotaTier, SubscriptionExpiresKind, SubscriptionInfo, SubscriptionQuota,
};

/// 调用 `/api/me` 获取 Ollama 账户信息，返回 `SubscriptionQuota`。
///
/// `credential_message` 携带 Plan 等级（如 "pro"），
/// `tiers[0].name` 携带邮箱地址，`utilization` 仅为内部缓存占位。
/// Ollama Cloud 不提供用量百分比，share runtime 上报时会清空 tier，
/// 避免 router 把占位 0% 当作真实 quota 信号。
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
        .body("{}")
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
    let subscription = build_ollama_subscription_info(&plan, period_end.clone());

    SubscriptionQuota {
        tool: "ollama_cloud".to_string(),
        credential_status: CredentialStatus::Valid,
        credential_message: Some(plan.clone()),
        subscription: Some(subscription),
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

fn build_ollama_subscription_info(plan: &str, period_end: Option<String>) -> SubscriptionInfo {
    let plan_type = plan.trim().to_string();
    let plan_label = if plan_type.is_empty() {
        "Ollama".to_string()
    } else if plan_type.to_lowercase().contains("ollama") {
        plan_type.clone()
    } else {
        format!("Ollama {}", capitalize_first(&plan_type))
    };
    let expires_source = period_end
        .as_ref()
        .map(|_| "ollama_api.me.SubscriptionPeriodEnd.Time".to_string());
    let expires_kind = if period_end.is_some() {
        Some(SubscriptionExpiresKind::BillingPeriod)
    } else {
        Some(SubscriptionExpiresKind::Unknown)
    };

    SubscriptionInfo {
        plan_type: Some(plan_type),
        plan_label: Some(plan_label),
        expires_at: period_end,
        expires_source,
        expires_kind,
    }
}

fn capitalize_first(value: &str) -> String {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
        None => String::new(),
    }
}

fn now_millis() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ollama_subscription_info_uses_period_end() {
        let subscription =
            build_ollama_subscription_info("pro", Some("2026-07-25T04:49:24Z".to_string()));

        assert_eq!(subscription.plan_type.as_deref(), Some("pro"));
        assert_eq!(subscription.plan_label.as_deref(), Some("Ollama Pro"));
        assert_eq!(
            subscription.expires_at.as_deref(),
            Some("2026-07-25T04:49:24Z")
        );
        assert_eq!(
            subscription.expires_source.as_deref(),
            Some("ollama_api.me.SubscriptionPeriodEnd.Time")
        );
        assert_eq!(
            subscription.expires_kind.as_ref(),
            Some(&SubscriptionExpiresKind::BillingPeriod)
        );
    }
}
