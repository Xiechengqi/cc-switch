//! Temporary Codex Banked Reset helper.
//!
//! Keep this module isolated from subscription quota and OAuth account storage so
//! the limited-time campaign can be removed without schema or provider changes.

use reqwest::header;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::time::Duration;

pub const CODEX_BANKED_RESET_ENABLED: bool = true;

const REFERRAL_KEY: &str = "codex_referral_persistent_invite";
const BACKEND_API_BASE: &str = "https://chatgpt.com/backend-api";
const DEFAULT_USER_AGENT: &str = "Codex Desktop/0.0.0 (Linux; x86_64)";
const REQUEST_TIMEOUT_SECS: u64 = 15;
const ERROR_BODY_MAX_CHARS: usize = 512;
const MAX_INVITE_EMAILS: usize = 5;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexBankedResetStatus {
    pub referral_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invite_eligibility: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invite_eligibility_error: Option<String>,
    pub eligibility_rules: Vec<String>,
    pub requires_consent: bool,
    pub available_count: i64,
    pub credits: Vec<CodexBankedResetCredit>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexBankedResetCredit {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub granted_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_image_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexBankedResetInviteResult {
    pub invites: Vec<Value>,
    pub failed_emails: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexBankedResetConsumeResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub credit_id: String,
    pub redeem_request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub available_count: Option<i64>,
    pub remaining_credits: Vec<Value>,
}

pub async fn get_status(token: &str, account_id: &str) -> Result<CodexBankedResetStatus, String> {
    ensure_enabled()?;

    let eligibility = match get_json(
        token,
        account_id,
        "/referrals/invite/eligibility",
        &[("referral_key", REFERRAL_KEY)],
    )
    .await
    {
        Ok(value) => Some(value),
        Err(err) => {
            log::warn!("Codex Banked Reset invite eligibility unavailable: {err}");
            None
        }
    };
    let rules_raw = get_json(
        token,
        account_id,
        "/wham/referrals/eligibility_rules",
        &[("referral_key", REFERRAL_KEY)],
    )
    .await?;
    let credits_raw = get_json(token, account_id, "/wham/rate-limit-reset-credits", &[]).await?;

    let credits = normalize_credits(&credits_raw);
    let mut available_count = int_field(&credits_raw, "available_count").unwrap_or(0);
    if available_count == 0 {
        available_count = credits
            .iter()
            .filter(|credit| {
                credit
                    .status
                    .as_deref()
                    .map(|status| status.eq_ignore_ascii_case("available"))
                    .unwrap_or(false)
            })
            .count() as i64;
    }

    Ok(CodexBankedResetStatus {
        referral_key: REFERRAL_KEY.to_string(),
        invite_eligibility: eligibility.clone(),
        invite_eligibility_error: eligibility
            .is_none()
            .then(|| "Invite eligibility endpoint is unavailable".to_string()),
        eligibility_rules: normalize_rules(&rules_raw),
        requires_consent: eligibility
            .as_ref()
            .and_then(|value| bool_field(value, "requires_explicit_confirmation"))
            .or_else(|| bool_field(&rules_raw, "requires_explicit_confirmation"))
            .unwrap_or(true),
        available_count,
        credits,
    })
}

pub async fn send_invite(
    token: &str,
    account_id: &str,
    emails: Vec<String>,
) -> Result<CodexBankedResetInviteResult, String> {
    ensure_enabled()?;
    let normalized = normalize_emails(emails)?;
    let raw = post_json(
        token,
        account_id,
        "/wham/referrals/invite",
        json!({
            "referral_key": REFERRAL_KEY,
            "emails": normalized,
        }),
    )
    .await?;

    Ok(CodexBankedResetInviteResult {
        invites: value_array_field(&raw, "invites"),
        failed_emails: string_array_field(&raw, "failed_emails"),
        message: string_field(&raw, "message"),
    })
}

pub async fn consume(
    token: &str,
    account_id: &str,
    credit_id: String,
) -> Result<CodexBankedResetConsumeResult, String> {
    ensure_enabled()?;
    let credit_id = credit_id.trim();
    if credit_id.is_empty() {
        return Err("credit_id is required".to_string());
    }

    let redeem_request_id = uuid::Uuid::new_v4().to_string();
    let raw = post_json(
        token,
        account_id,
        "/wham/rate-limit-reset-credits/consume",
        json!({
            "credit_id": credit_id,
            "redeem_request_id": redeem_request_id,
        }),
    )
    .await?;

    Ok(CodexBankedResetConsumeResult {
        code: string_field(&raw, "code"),
        credit_id: credit_id.to_string(),
        redeem_request_id,
        available_count: int_field(&raw, "available_count"),
        remaining_credits: value_array_field(&raw, "credits"),
    })
}

async fn get_json(
    token: &str,
    account_id: &str,
    path: &str,
    query: &[(&str, &str)],
) -> Result<Value, String> {
    let client = crate::proxy::http_client::get();
    let mut request = apply_headers(client.get(build_url(path)?), token, account_id);
    if !query.is_empty() {
        request = request.query(query);
    }
    send_json(request).await
}

async fn post_json(
    token: &str,
    account_id: &str,
    path: &str,
    body: Value,
) -> Result<Value, String> {
    let client = crate::proxy::http_client::get();
    send_json(
        apply_headers(client.post(build_url(path)?), token, account_id)
            .header(header::CONTENT_TYPE, "application/json")
            .json(&body),
    )
    .await
}

fn apply_headers(
    request: reqwest::RequestBuilder,
    token: &str,
    account_id: &str,
) -> reqwest::RequestBuilder {
    request
        .bearer_auth(token)
        .header(header::ACCEPT, "application/json")
        .header(header::USER_AGENT, DEFAULT_USER_AGENT)
        .header("OAI-Language", "zh-CN")
        .header("OAI-Product-Sku", "CODEX")
        .header("originator", "Codex Desktop")
        .header("X-OpenAI-Attach-Auth", "1")
        .header("X-OpenAI-Attach-Integrity-State", "1")
        .header("chatgpt-account-id", account_id)
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
}

async fn send_json(request: reqwest::RequestBuilder) -> Result<Value, String> {
    let response = request
        .send()
        .await
        .map_err(|err| format!("Codex Banked Reset request failed: {err}"))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|err| format!("Failed to read Codex Banked Reset response: {err}"))?;
    if !status.is_success() {
        return Err(format!(
            "Codex Banked Reset upstream returned HTTP {status}: {}",
            truncate_body(&text)
        ));
    }
    if text.trim().is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    serde_json::from_str(&text)
        .map_err(|err| format!("Failed to parse Codex Banked Reset response: {err}"))
}

fn build_url(path: &str) -> Result<String, String> {
    let path = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };
    Ok(format!("{BACKEND_API_BASE}{path}"))
}

fn ensure_enabled() -> Result<(), String> {
    if CODEX_BANKED_RESET_ENABLED {
        Ok(())
    } else {
        Err("Codex Banked Reset is disabled".to_string())
    }
}

fn normalize_emails(emails: Vec<String>) -> Result<Vec<String>, String> {
    let mut result: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for raw in emails {
        for email in raw
            .split(|ch: char| ch == ',' || ch == ';' || ch.is_whitespace())
            .map(str::trim)
            .filter(|email| !email.is_empty())
        {
            let key = email.to_ascii_lowercase();
            if !seen.insert(key) {
                continue;
            }
            if !is_valid_email(email) {
                return Err(format!("invalid email: {email}"));
            }
            result.push(email.to_string());
            if result.len() > MAX_INVITE_EMAILS {
                return Err(format!(
                    "You can invite at most {MAX_INVITE_EMAILS} emails at once"
                ));
            }
        }
    }
    if result.is_empty() {
        return Err("emails are required".to_string());
    }
    Ok(result)
}

fn is_valid_email(email: &str) -> bool {
    let mut parts = email.split('@');
    let Some(local) = parts.next() else {
        return false;
    };
    let Some(domain) = parts.next() else {
        return false;
    };
    parts.next().is_none()
        && !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !email.chars().any(char::is_whitespace)
}

fn normalize_credits(raw: &Value) -> Vec<CodexBankedResetCredit> {
    raw.get("credits")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let id = string_field(item, "id")?;
            Some(CodexBankedResetCredit {
                id,
                status: string_field(item, "status"),
                granted_at: string_field_any(
                    item,
                    &["granted_at", "grantedAt", "grant_at", "grantAt"],
                ),
                expires_at: string_field_any(item, &["expires_at", "expiresAt"]),
                title: string_field(item, "title"),
                description: string_field(item, "description"),
                profile_user_id: string_field(item, "profile_user_id"),
                profile_image_url: string_field(item, "profile_image_url"),
            })
        })
        .collect()
}

fn normalize_rules(raw: &Value) -> Vec<String> {
    raw.get("rules")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            if let Some(text) = item.as_str().map(str::trim).filter(|text| !text.is_empty()) {
                return Some(text.to_string());
            }
            ["text", "description", "message", "title"]
                .iter()
                .find_map(|key| string_field(item, key))
        })
        .collect()
}

fn string_field(raw: &Value, key: &str) -> Option<String> {
    raw.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn string_field_any(raw: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        raw.get(*key).and_then(|value| match value {
            Value::String(text) => {
                let text = text.trim();
                (!text.is_empty()).then(|| text.to_string())
            }
            Value::Number(number) => Some(number.to_string()),
            _ => None,
        })
    })
}

fn bool_field(raw: &Value, key: &str) -> Option<bool> {
    raw.get(key).and_then(Value::as_bool)
}

fn int_field(raw: &Value, key: &str) -> Option<i64> {
    raw.get(key).and_then(|value| {
        value
            .as_i64()
            .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
            .or_else(|| value.as_f64().map(|value| value as i64))
    })
}

fn string_array_field(raw: &Value, key: &str) -> Vec<String> {
    raw.get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|value| value.as_str().map(str::trim))
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

fn value_array_field(raw: &Value, key: &str) -> Vec<Value> {
    raw.get(key)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn truncate_body(body: &str) -> String {
    if body.chars().count() <= ERROR_BODY_MAX_CHARS {
        body.to_string()
    } else {
        let mut truncated: String = body.chars().take(ERROR_BODY_MAX_CHARS).collect();
        truncated.push_str("...");
        truncated
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalize_emails_splits_deduplicates_and_limits() {
        let emails = normalize_emails(vec![
            "a@example.com, b@example.com".to_string(),
            "A@example.com; c@example.com".to_string(),
        ])
        .expect("emails should normalize");

        assert_eq!(
            emails,
            vec![
                "a@example.com".to_string(),
                "b@example.com".to_string(),
                "c@example.com".to_string()
            ]
        );

        assert!(normalize_emails(vec!["bad-email".to_string()]).is_err());
        assert!(normalize_emails(vec![
            "a@x.com,b@x.com,c@x.com,d@x.com,e@x.com,f@x.com".to_string()
        ])
        .is_err());
    }

    #[test]
    fn normalizes_credits_and_rules() {
        let credits = normalize_credits(&json!({
            "credits": [
                {
                    "id": "credit-1",
                    "status": "available",
                    "title": "Reset",
                    "granted_at": "2026-06-15T03:32:00Z",
                    "expiresAt": "2026-07-15T03:32:00Z"
                },
                { "status": "missing-id" }
            ]
        }));

        assert_eq!(credits.len(), 1);
        assert_eq!(credits[0].id, "credit-1");
        assert_eq!(credits[0].title.as_deref(), Some("Reset"));
        assert_eq!(
            credits[0].granted_at.as_deref(),
            Some("2026-06-15T03:32:00Z")
        );
        assert_eq!(
            credits[0].expires_at.as_deref(),
            Some("2026-07-15T03:32:00Z")
        );

        let rules = normalize_rules(&json!({
            "rules": [
                { "text": "Friend sends first Codex message" },
                "Limited-time campaign"
            ]
        }));

        assert_eq!(
            rules,
            vec![
                "Friend sends first Codex message".to_string(),
                "Limited-time campaign".to_string()
            ]
        );
    }

    #[test]
    fn missing_credit_status_is_not_available() {
        let credits = normalize_credits(&json!({
            "credits": [
                { "id": "credit-1", "status": "available" },
                { "id": "credit-2" },
                { "id": "credit-3", "status": "redeemed" }
            ]
        }));

        let available_count = credits
            .iter()
            .filter(|credit| {
                credit
                    .status
                    .as_deref()
                    .map(|status| status.eq_ignore_ascii_case("available"))
                    .unwrap_or(false)
            })
            .count();

        assert_eq!(available_count, 1);
    }

    #[test]
    fn normalizes_credit_time_aliases() {
        let credits = normalize_credits(&json!({
            "credits": [
                {
                    "id": "credit-1",
                    "status": "available",
                    "grantAt": 1781494124,
                    "expires_at": 1784086124
                }
            ]
        }));

        assert_eq!(credits.len(), 1);
        assert_eq!(credits[0].granted_at.as_deref(), Some("1781494124"));
        assert_eq!(credits[0].expires_at.as_deref(), Some("1784086124"));
    }
}
