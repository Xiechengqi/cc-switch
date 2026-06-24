use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OllamaModel {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owned_by: Option<String>,
}

pub fn parse_models_response(value: &Value) -> Vec<OllamaModel> {
    value
        .get("data")
        .and_then(|data| data.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let id = item.get("id").and_then(|v| v.as_str())?.to_string();
                    Some(OllamaModel {
                        display_name: Some(id.clone()),
                        owned_by: item
                            .get("owned_by")
                            .or_else(|| item.get("ownedBy"))
                            .and_then(|v| v.as_str())
                            .map(ToString::to_string),
                        id,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn parse_tags_response(value: &Value) -> Vec<OllamaModel> {
    value
        .get("models")
        .and_then(|models| models.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let id = item
                        .get("name")
                        .or_else(|| item.get("model"))
                        .and_then(|v| v.as_str())?
                        .to_string();
                    Some(OllamaModel {
                        display_name: Some(id.clone()),
                        owned_by: None,
                        id,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}
