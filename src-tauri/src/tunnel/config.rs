use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const KNOWN_PUBLIC_SHARE_ROUTER_DOMAINS: &[&str] = &["jptokenswitch.cc", "sgptokenswitch.cc"];

/// Share router configuration — stored in AppSettings
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelConfig {
    /// Share router domain (e.g. "example.com" or "127.0.0.1:8787")
    pub domain: String,
}

impl TunnelConfig {
    pub fn default_public_service() -> Self {
        Self {
            domain: "jptokenswitch.cc".to_string(),
        }
    }

    /// Whether this is a local/dev domain (localhost, 127.0.0.1, 0.0.0.0)
    pub fn is_local(&self) -> bool {
        let host = self.domain.split(':').next().unwrap_or(&self.domain);
        matches!(host, "localhost" | "127.0.0.1" | "0.0.0.0")
    }

    pub fn get_server_addr(&self) -> String {
        let domain = self.domain.trim().trim_end_matches('/');
        if domain.starts_with("http://") || domain.starts_with("https://") {
            return domain.to_string();
        }
        let proto = if self.is_local() { "http" } else { "https" };
        format!("{proto}://{domain}")
    }

    pub fn get_tunnel_addr(&self, subdomain: &str) -> String {
        let proto = if self.is_local() { "http" } else { "https" };
        format!("{proto}://{subdomain}.{}", self.domain)
    }

    pub fn matches_tunnel_url(&self, url_or_host: &str) -> bool {
        let Some(authority) = extract_authority(url_or_host) else {
            return false;
        };

        authority == self.domain || authority.ends_with(&format!(".{}", self.domain))
    }
}

impl Default for TunnelConfig {
    fn default() -> Self {
        Self::default_public_service()
    }
}

pub fn current_tunnel_config() -> Option<TunnelConfig> {
    crate::settings::get_settings()
        .current_share_router_domain()
        .map(|domain| TunnelConfig {
            domain: domain.to_string(),
        })
}

pub fn is_share_tunnel_url(url_or_host: &str) -> bool {
    let Some(authority) = extract_authority(url_or_host) else {
        return false;
    };

    share_router_domains()
        .into_iter()
        .any(|domain| authority == domain || authority.ends_with(&format!(".{domain}")))
}

fn share_router_domains() -> Vec<String> {
    let mut domains = KNOWN_PUBLIC_SHARE_ROUTER_DOMAINS
        .iter()
        .map(|domain| domain.to_string())
        .collect::<Vec<_>>();

    if let Some(config) = current_tunnel_config() {
        let Some(configured) = extract_authority(&config.domain) else {
            return domains;
        };
        if !configured.is_empty() && !domains.iter().any(|domain| domain == &configured) {
            domains.push(configured);
        }
    }

    domains
}

fn extract_authority(url_or_host: &str) -> Option<String> {
    let trimmed = url_or_host.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }

    let lower_trimmed = trimmed.to_ascii_lowercase();
    if lower_trimmed.starts_with("http://") || lower_trimmed.starts_with("https://") {
        return reqwest::Url::parse(trimmed)
            .ok()
            .and_then(|url| {
                url.host_str().map(|host| match url.port() {
                    Some(port) => format!("{host}:{port}"),
                    None => host.to_string(),
                })
            })
            .map(|authority| authority.to_ascii_lowercase());
    }

    Some(trimmed.split('/').next()?.to_ascii_lowercase())
}

/// Request to start a new tunnel
#[derive(Debug, Clone)]
pub struct TunnelRequest {
    pub tunnel_type: TunnelType,
    pub subdomain: String,
    pub local_addr: String,
    pub share_metadata: Option<ShareTunnelMetadata>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TunnelType {
    Http,
    Tcp,
}

impl TunnelType {
    pub fn as_str(&self) -> &'static str {
        match self {
            TunnelType::Http => "http",
            TunnelType::Tcp => "tcp",
        }
    }
}

/// Tunnel status returned to callers
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelInfo {
    pub tunnel_url: String,
    pub subdomain: String,
    pub remote_port: u16,
    pub healthy: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareTunnelStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub info: Option<TunnelInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default)]
    pub requires_owner_login: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ShareSupport {
    pub claude: bool,
    pub codex: bool,
    pub gemini: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareUpstreamQuotaTier {
    pub label: String,
    pub utilization: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareUpstreamQuota {
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queried_at: Option<i64>,
    #[serde(default)]
    pub tiers: Vec<ShareUpstreamQuotaTier>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareUpstreamModel {
    pub slot: String,
    pub actual_model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareUpstreamProvider {
    pub kind: String,
    pub app: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub for_sale_official_price_percent: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota: Option<ShareUpstreamQuota>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<ShareUpstreamModel>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareAppProvider {
    pub id: String,
    pub name: String,
    pub app: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_type: Option<String>,
    #[serde(default)]
    pub is_current: bool,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub for_sale_official_price_percent: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota: Option<ShareUpstreamQuota>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<ShareUpstreamModel>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareAppProviders {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub claude: Vec<ShareAppProvider>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub codex: Vec<ShareAppProvider>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gemini: Vec<ShareAppProvider>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareAppRuntimes {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude: Option<ShareUpstreamProvider>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex: Option<ShareUpstreamProvider>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gemini: Option<ShareUpstreamProvider>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kiro: Option<ShareUpstreamProvider>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<ShareUpstreamProvider>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub antigravity: Option<ShareUpstreamProvider>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copilot: Option<ShareUpstreamProvider>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareModelHealthSummary {
    #[serde(default)]
    pub claude: Vec<ShareModelHealthResult>,
    #[serde(default)]
    pub codex: Vec<ShareModelHealthResult>,
    #[serde(default)]
    pub gemini: Vec<ShareModelHealthResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareModelHealthResult {
    pub app_type: String,
    pub requested_model: String,
    pub actual_model: String,
    pub status: String,
    #[serde(default)]
    pub recent_results: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_code: Option<u16>,
    pub latency_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    pub checked_at: i64,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareRuntimeSnapshot {
    pub share_id: String,
    pub queried_at: i64,
    pub token_limit: i64,
    pub tokens_used: i64,
    pub requests_count: i64,
    pub share_status: String,
    pub support: ShareSupport,
    pub app_runtimes: ShareAppRuntimes,
    #[serde(default)]
    pub app_providers: ShareAppProviders,
    #[serde(default)]
    pub model_health: ShareModelHealthSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareTunnelMetadata {
    pub share_id: String,
    pub share_name: String,
    pub owner_email: String,
    #[serde(default)]
    pub shared_with_emails: Vec<String>,
    #[serde(default = "default_market_access_mode")]
    pub market_access_mode: String,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub for_sale_official_price_percent_by_app: HashMap<String, u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub for_sale: String,
    pub subdomain: String,
    pub share_token: String,
    pub app_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    /// P9 多 app share：每个 app_type 当前绑定的 provider id。router 端 ShareDescriptor
    /// 通过 #[serde(default)] 接收老 cc-switch 不带这个字段时为空 map，路由功能不受影响。
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub bindings: HashMap<String, String>,
    pub token_limit: i64,
    pub parallel_limit: i64,
    pub tokens_used: i64,
    pub requests_count: i64,
    pub share_status: String,
    // Local-only setting. The deployed router does not include autoStart in the
    // signed ShareDescriptor, so serializing it would make router signature
    // verification fail after deserialization drops the unknown field.
    #[serde(default, skip_serializing)]
    pub auto_start: bool,
    pub created_at: String,
    pub expires_at: String,
    #[serde(default)]
    pub support: ShareSupport,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_provider: Option<ShareUpstreamProvider>,
    #[serde(default)]
    pub app_runtimes: ShareAppRuntimes,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareClaimPayload {
    pub share_id: String,
    pub subdomain: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_email: Option<String>,
}

impl ShareTunnelMetadata {
    pub fn claim_payload(&self) -> ShareClaimPayload {
        ShareClaimPayload {
            share_id: self.share_id.clone(),
            subdomain: self.subdomain.clone(),
            owner_email: Some(self.owner_email.clone()),
        }
    }
}

fn default_market_access_mode() -> String {
    "selected".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareTunnelRequestLog {
    pub request_id: String,
    pub share_id: String,
    pub share_name: String,
    pub provider_id: String,
    pub provider_name: String,
    pub app_type: String,
    pub model: String,
    pub request_model: String,
    pub request_agent: String,
    pub requested_model: String,
    pub actual_model: String,
    pub actual_model_source: String,
    pub status_code: u16,
    pub latency_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_token_ms: Option<u64>,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_creation_tokens: u32,
    pub is_streaming: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_email: Option<String>,
    pub created_at: i64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn matches_share_subdomain() {
        let config = TunnelConfig {
            domain: "share.example.com".to_string(),
        };

        assert!(config.matches_tunnel_url("https://alpha.share.example.com/v1"));
        assert!(config.matches_tunnel_url("alpha.share.example.com"));
        assert!(!config.matches_tunnel_url("https://api.openai.com/v1"));
    }

    #[test]
    fn detects_known_public_share_router_domains() {
        assert!(is_share_tunnel_url("https://alpha.jptokenswitch.cc/v1"));
        assert!(is_share_tunnel_url("beta.sgptokenswitch.cc"));
        assert!(!is_share_tunnel_url("https://jptokenswitch.com/v1"));
        assert!(!is_share_tunnel_url("https://api.openai.com/v1"));
    }

    #[test]
    fn detects_configured_share_router_domain_with_scheme_and_case() {
        let mut settings = crate::settings::AppSettings::default();
        settings.share_router_domain = Some("HTTPS://Share.Example.Com/".to_string());
        crate::settings::update_settings(settings).unwrap();

        assert!(is_share_tunnel_url("https://alpha.share.example.com/v1"));
        assert!(is_share_tunnel_url("ALPHA.SHARE.EXAMPLE.COM"));
        assert!(!is_share_tunnel_url("https://alpha.other-example.com/v1"));
    }

    #[test]
    fn omits_null_provider_id_when_serializing_share_metadata() {
        let metadata = ShareTunnelMetadata {
            share_id: "share-1".to_string(),
            share_name: "Test".to_string(),
            owner_email: "owner@example.com".to_string(),
            shared_with_emails: vec!["friend@example.com".to_string()],
            market_access_mode: "selected".to_string(),
            for_sale_official_price_percent_by_app: HashMap::new(),
            description: None,
            for_sale: "No".to_string(),
            subdomain: "demo".to_string(),
            share_token: "token".to_string(),
            app_type: "codex".to_string(),
            provider_id: None,
            bindings: HashMap::new(),
            token_limit: 100,
            parallel_limit: 3,
            tokens_used: 0,
            requests_count: 0,
            share_status: "active".to_string(),
            auto_start: true,
            created_at: "2026-04-21T00:00:00Z".to_string(),
            expires_at: "2026-04-22T00:00:00Z".to_string(),
            support: ShareSupport::default(),
            upstream_provider: None,
            app_runtimes: ShareAppRuntimes::default(),
        };

        let value = serde_json::to_value(&metadata).expect("serialize share metadata");
        assert_eq!(
            value,
            json!({
                "shareId": "share-1",
                "shareName": "Test",
                "ownerEmail": "owner@example.com",
                "sharedWithEmails": ["friend@example.com"],
                "marketAccessMode": "selected",
                "forSale": "No",
                "subdomain": "demo",
                "shareToken": "token",
                "appType": "codex",
                "tokenLimit": 100,
                "parallelLimit": 3,
                "tokensUsed": 0,
                "requestsCount": 0,
                "shareStatus": "active",
                "createdAt": "2026-04-21T00:00:00Z",
                "expiresAt": "2026-04-22T00:00:00Z",
                "support": {
                    "claude": false,
                    "codex": false,
                    "gemini": false
                },
                "appRuntimes": {}
            })
        );
    }

    #[test]
    fn share_claim_payload_uses_stable_minimal_fields() {
        let mut pricing = HashMap::new();
        pricing.insert("codex".to_string(), 5);
        let metadata = ShareTunnelMetadata {
            share_id: "share-1".to_string(),
            share_name: "Test".to_string(),
            owner_email: "owner@example.com".to_string(),
            shared_with_emails: vec!["friend@example.com".to_string()],
            market_access_mode: "all".to_string(),
            for_sale_official_price_percent_by_app: pricing,
            description: Some("not signed by claim".to_string()),
            for_sale: "Yes".to_string(),
            subdomain: "demo".to_string(),
            share_token: "token".to_string(),
            app_type: "codex".to_string(),
            provider_id: Some("provider-1".to_string()),
            bindings: HashMap::new(),
            token_limit: -1,
            parallel_limit: -1,
            tokens_used: 10,
            requests_count: 2,
            share_status: "active".to_string(),
            auto_start: true,
            created_at: "2026-04-21T00:00:00Z".to_string(),
            expires_at: "2026-04-22T00:00:00Z".to_string(),
            support: ShareSupport::default(),
            upstream_provider: None,
            app_runtimes: ShareAppRuntimes::default(),
        };

        let value = serde_json::to_value(metadata.claim_payload()).expect("serialize claim");
        assert_eq!(
            value,
            json!({
                "shareId": "share-1",
                "subdomain": "demo",
                "ownerEmail": "owner@example.com"
            })
        );
    }
}
