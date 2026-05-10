use super::{
    deepseek_account_auth::{deepseek_base_headers, ensure_ok},
    deepseek_pow::{solve_and_build_header, DeepSeekPowChallenge},
};
use reqwest::{Client, Response};
use serde_json::Value;

const CREATE_SESSION_URL: &str = "https://chat.deepseek.com/api/v0/chat_session/create";
const CREATE_POW_URL: &str = "https://chat.deepseek.com/api/v0/chat/create_pow_challenge";
const COMPLETION_URL: &str = "https://chat.deepseek.com/api/v0/chat/completion";
const COMPLETION_TARGET_PATH: &str = "/api/v0/chat/completion";

#[derive(Clone)]
pub struct DeepSeekWebClient {
    http: Client,
}

impl DeepSeekWebClient {
    pub fn new() -> Self {
        Self {
            http: Client::builder()
                .user_agent("DeepSeek/2.0.4 Android/35")
                .build()
                .unwrap_or_else(|_| Client::new()),
        }
    }

    pub async fn create_session(&self, token: &str) -> anyhow::Result<String> {
        let value = self
            .post_json(
                CREATE_SESSION_URL,
                token,
                &serde_json::json!({"agent":"chat"}),
            )
            .await?;
        ensure_ok(&value, "create_session").map_err(|e| anyhow::anyhow!(e.to_string()))?;
        extract_session_id(&value).ok_or_else(|| anyhow::anyhow!("create_session missing id"))
    }

    pub async fn create_pow_header(&self, token: &str) -> anyhow::Result<String> {
        let value = self
            .post_json(
                CREATE_POW_URL,
                token,
                &serde_json::json!({"target_path": COMPLETION_TARGET_PATH}),
            )
            .await?;
        ensure_ok(&value, "create_pow").map_err(|e| anyhow::anyhow!(e.to_string()))?;
        let challenge_value = value
            .pointer("/data/biz_data/challenge")
            .ok_or_else(|| anyhow::anyhow!("create_pow missing challenge"))?
            .clone();
        let challenge: DeepSeekPowChallenge = serde_json::from_value(challenge_value)?;
        solve_and_build_header(&challenge).await
    }

    pub async fn completion(
        &self,
        token: &str,
        session_id: &str,
        pow_header: &str,
        model: &str,
        prompt: &str,
    ) -> anyhow::Result<Response> {
        let payload = completion_payload(session_id, model, prompt);
        Ok(self
            .http
            .post(COMPLETION_URL)
            .headers(deepseek_base_headers())
            .bearer_auth(token)
            .header("x-ds-pow-response", pow_header)
            .json(&payload)
            .send()
            .await?)
    }

    async fn post_json(&self, url: &str, token: &str, payload: &Value) -> anyhow::Result<Value> {
        let resp = self
            .http
            .post(url)
            .headers(deepseek_base_headers())
            .bearer_auth(token)
            .json(payload)
            .send()
            .await?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("{url} returned HTTP {status}: {body}");
        }
        Ok(serde_json::from_str(&body)?)
    }
}

fn extract_session_id(value: &Value) -> Option<String> {
    value
        .pointer("/data/biz_data/id")
        .or_else(|| value.pointer("/data/biz_data/chat_session/id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

fn completion_payload(session_id: &str, model: &str, prompt: &str) -> Value {
    serde_json::json!({
        "chat_session_id": session_id,
        "parent_message_id": null,
        "prompt": prompt,
        "ref_file_ids": [],
        "thinking_enabled": false,
        "search_enabled": false,
        "model_type": model_type(model),
    })
}

fn model_type(model: &str) -> &'static str {
    match model {
        "deepseek-v4-pro"
        | "deepseek-v4-pro-nothinking"
        | "deepseek-v4-pro-search"
        | "deepseek-v4-pro-search-nothinking" => "expert",
        "deepseek-v4-vision" | "deepseek-v4-vision-nothinking" => "vision",
        _ => "default",
    }
}
