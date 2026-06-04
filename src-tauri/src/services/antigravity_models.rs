use crate::services::model_fetch::FetchedModel;
use crate::services::subscription::QuotaTier;
use serde::Deserialize;
use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

const FETCH_AVAILABLE_MODELS_ENDPOINTS: &[&str] = &[
    "https://daily-cloudcode-pa.sandbox.googleapis.com/v1internal:fetchAvailableModels",
    "https://daily-cloudcode-pa.googleapis.com/v1internal:fetchAvailableModels",
    "https://cloudcode-pa.googleapis.com/v1internal:fetchAvailableModels",
];

const FETCH_TIMEOUT_SECS: u64 = 15;

#[derive(Debug, Clone, Copy)]
pub struct AntigravityModelDef {
    pub id: &'static str,
    pub display_name: &'static str,
    pub owned_by: &'static str,
}

pub const ANTIGRAVITY_FREE_MODELS: &[AntigravityModelDef] = &[
    AntigravityModelDef {
        id: "gemini-3.5-flash-medium",
        display_name: "Gemini 3.5 Flash (Medium)",
        owned_by: "google",
    },
    AntigravityModelDef {
        id: "gemini-3.5-flash-high",
        display_name: "Gemini 3.5 Flash (High)",
        owned_by: "google",
    },
    AntigravityModelDef {
        id: "gemini-3.5-flash-low",
        display_name: "Gemini 3.5 Flash (Low)",
        owned_by: "google",
    },
    AntigravityModelDef {
        id: "gemini-3.1-pro-low",
        display_name: "Gemini 3.1 Pro (Low)",
        owned_by: "google",
    },
    AntigravityModelDef {
        id: "gemini-3.1-pro-high",
        display_name: "Gemini 3.1 Pro (High)",
        owned_by: "google",
    },
    AntigravityModelDef {
        id: "claude-sonnet-4-6-thinking",
        display_name: "Claude Sonnet 4.6 (Thinking)",
        owned_by: "anthropic",
    },
    AntigravityModelDef {
        id: "claude-opus-4-6-thinking",
        display_name: "Claude Opus 4.6 (Thinking)",
        owned_by: "anthropic",
    },
    AntigravityModelDef {
        id: "gpt-oss-120b-medium",
        display_name: "GPT-OSS 120B (Medium)",
        owned_by: "openai",
    },
];

#[derive(Debug, Clone)]
pub struct AntigravityAvailableModel {
    pub id: String,
    pub display_name: Option<String>,
    pub remaining_fraction: Option<f64>,
    pub reset_time: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FetchAvailableModelsResponse {
    models: BTreeMap<String, FetchAvailableModelInfo>,
}

#[derive(Debug, Deserialize)]
struct FetchAvailableModelInfo {
    #[serde(rename = "displayName")]
    display_name: Option<String>,
    #[serde(rename = "quotaInfo")]
    quota_info: Option<FetchAvailableQuotaInfo>,
}

#[derive(Debug, Deserialize)]
struct FetchAvailableQuotaInfo {
    #[serde(rename = "remainingFraction")]
    remaining_fraction: Option<f64>,
    #[serde(rename = "resetTime")]
    reset_time: Option<String>,
}

pub fn normalize_antigravity_model_id(model: &str) -> String {
    match model.trim() {
        "claude-4.6-sonnet-thinking" => "claude-sonnet-4-6-thinking".to_string(),
        "claude-4.6-opus-thinking" => "claude-opus-4-6-thinking".to_string(),
        "gemini-3-pro-low" => "gemini-3.1-pro-low".to_string(),
        "gemini-3-pro-high" => "gemini-3.1-pro-high".to_string(),
        other => other.to_string(),
    }
}

pub fn antigravity_model_display_name(model: &str) -> String {
    let canonical = normalize_antigravity_model_id(model);
    ANTIGRAVITY_FREE_MODELS
        .iter()
        .find(|def| def.id == canonical)
        .map(|def| def.display_name.to_string())
        .unwrap_or(canonical)
}

pub fn static_antigravity_models() -> Vec<FetchedModel> {
    ANTIGRAVITY_FREE_MODELS
        .iter()
        .map(|model| FetchedModel {
            id: model.id.to_string(),
            owned_by: Some(model.owned_by.to_string()),
            display_name: Some(model.display_name.to_string()),
        })
        .collect()
}

pub fn merge_static_and_dynamic_models(
    dynamic: Vec<AntigravityAvailableModel>,
) -> Vec<FetchedModel> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();

    for model in static_antigravity_models() {
        seen.insert(model.id.clone());
        out.push(model);
    }

    for model in dynamic {
        let id = normalize_antigravity_model_id(&model.id);
        if id.trim().is_empty() || !seen.insert(id.clone()) {
            continue;
        }
        out.push(FetchedModel {
            owned_by: Some(owner_for_model(&id).to_string()),
            display_name: model
                .display_name
                .or_else(|| Some(antigravity_model_display_name(&id))),
            id,
        });
    }

    out
}

pub fn antigravity_models_to_quota_tiers(models: &[AntigravityAvailableModel]) -> Vec<QuotaTier> {
    let mut tiers: Vec<QuotaTier> = models
        .iter()
        .filter_map(|model| {
            let remaining = model.remaining_fraction?.clamp(0.0, 1.0);
            let canonical = normalize_antigravity_model_id(&model.id);
            Some(QuotaTier {
                name: model
                    .display_name
                    .clone()
                    .unwrap_or_else(|| antigravity_model_display_name(&canonical)),
                utilization: (1.0 - remaining) * 100.0,
                resets_at: model.reset_time.clone(),
                used: None,
                limit: None,
                unit: None,
            })
        })
        .collect();

    tiers.sort_by_key(|tier| sort_weight_for_display_name(&tier.name));
    tiers
}

pub async fn fetch_antigravity_available_models(
    access_token: &str,
    project_id: Option<&str>,
) -> Result<Vec<AntigravityAvailableModel>, String> {
    if access_token.trim().is_empty() {
        return Err("Antigravity OAuth access token is required".to_string());
    }

    let mut body = serde_json::json!({});
    if let Some(project_id) = project_id.map(str::trim).filter(|id| !id.is_empty()) {
        body["project"] = serde_json::Value::String(project_id.to_string());
    }

    let client = crate::proxy::http_client::get();
    let mut last_error: Option<String> = None;

    for (index, endpoint) in FETCH_AVAILABLE_MODELS_ENDPOINTS.iter().enumerate() {
        let has_next = index + 1 < FETCH_AVAILABLE_MODELS_ENDPOINTS.len();
        let mut current_body = body.clone();
        let mut retried_without_project = false;

        loop {
            let response = client
                .post(*endpoint)
                .bearer_auth(access_token)
                .header("Content-Type", "application/json")
                .json(&current_body)
                .timeout(Duration::from_secs(FETCH_TIMEOUT_SECS))
                .send()
                .await;

            let response = match response {
                Ok(response) => response,
                Err(err) => {
                    last_error = Some(format!("{} network error: {err}", endpoint));
                    break;
                }
            };

            let status = response.status();
            if status.is_success() {
                let parsed: FetchAvailableModelsResponse = response
                    .json()
                    .await
                    .map_err(|err| format!("Failed to parse Antigravity model list: {err}"))?;
                return Ok(parsed
                    .models
                    .into_iter()
                    .filter(|(id, _)| is_public_model_id(id))
                    .map(|(id, info)| AntigravityAvailableModel {
                        id,
                        display_name: info.display_name,
                        remaining_fraction: info
                            .quota_info
                            .as_ref()
                            .and_then(|quota| quota.remaining_fraction),
                        reset_time: info.quota_info.and_then(|quota| quota.reset_time),
                    })
                    .collect());
            }

            if status == reqwest::StatusCode::FORBIDDEN
                && current_body.get("project").is_some()
                && !retried_without_project
            {
                current_body = serde_json::json!({});
                retried_without_project = true;
                continue;
            }

            let body_text = response.text().await.unwrap_or_default();
            last_error = Some(format!(
                "{} returned HTTP {}: {}",
                endpoint,
                status,
                truncate_error_body(&body_text)
            ));
            if has_next
                && (status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error())
            {
                break;
            }
            if has_next && status == reqwest::StatusCode::NOT_FOUND {
                break;
            }
            return Err(last_error.unwrap_or_else(|| "Antigravity model list failed".to_string()));
        }
    }

    Err(last_error.unwrap_or_else(|| "Antigravity model list failed".to_string()))
}

fn owner_for_model(model: &str) -> &'static str {
    if model.starts_with("claude") {
        "anthropic"
    } else if model.starts_with("gpt") {
        "openai"
    } else {
        "google"
    }
}

fn is_public_model_id(model: &str) -> bool {
    model.starts_with("gemini")
        || model.starts_with("claude")
        || model.starts_with("gpt")
        || model.starts_with("image")
        || model.starts_with("imagen")
}

fn sort_weight_for_display_name(name: &str) -> usize {
    let lower = name.to_lowercase();
    if lower.contains("gemini 3.5 flash") {
        if lower.contains("low") {
            10
        } else if lower.contains("medium") {
            11
        } else {
            12
        }
    } else if lower.contains("gemini 3.1 pro") {
        if lower.contains("low") {
            20
        } else {
            21
        }
    } else if lower.contains("claude sonnet") {
        30
    } else if lower.contains("claude opus") {
        31
    } else if lower.contains("gpt-oss") {
        40
    } else {
        100
    }
}

fn truncate_error_body(body: &str) -> String {
    const MAX_CHARS: usize = 512;
    let trimmed = body.trim();
    if trimmed.chars().count() <= MAX_CHARS {
        trimmed.to_string()
    } else {
        let mut out: String = trimmed.chars().take(MAX_CHARS).collect();
        out.push_str("...");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_plugin_style_claude_aliases() {
        assert_eq!(
            normalize_antigravity_model_id("claude-4.6-sonnet-thinking"),
            "claude-sonnet-4-6-thinking"
        );
        assert_eq!(
            normalize_antigravity_model_id("claude-4.6-opus-thinking"),
            "claude-opus-4-6-thinking"
        );
    }

    #[test]
    fn static_catalog_contains_free_account_models() {
        let ids: Vec<_> = static_antigravity_models()
            .into_iter()
            .map(|model| model.id)
            .collect();
        assert!(ids.contains(&"gemini-3.5-flash-medium".to_string()));
        assert!(ids.contains(&"gemini-3.5-flash-high".to_string()));
        assert!(ids.contains(&"gemini-3.5-flash-low".to_string()));
        assert!(ids.contains(&"gemini-3.1-pro-low".to_string()));
        assert!(ids.contains(&"gemini-3.1-pro-high".to_string()));
        assert!(ids.contains(&"claude-sonnet-4-6-thinking".to_string()));
        assert!(ids.contains(&"claude-opus-4-6-thinking".to_string()));
        assert!(ids.contains(&"gpt-oss-120b-medium".to_string()));
    }

    #[test]
    fn converts_available_models_to_utilization_tiers() {
        let tiers = antigravity_models_to_quota_tiers(&[AntigravityAvailableModel {
            id: "gemini-3.5-flash-medium".to_string(),
            display_name: Some("Gemini 3.5 Flash (Medium)".to_string()),
            remaining_fraction: Some(0.75),
            reset_time: Some("2026-06-05T00:00:00Z".to_string()),
        }]);
        assert_eq!(tiers.len(), 1);
        assert_eq!(tiers[0].name, "Gemini 3.5 Flash (Medium)");
        assert_eq!(tiers[0].utilization, 25.0);
    }
}
