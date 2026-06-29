use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::database::{ShareAppAccess, ShareAppSettings};

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

    pub fn from_settings_or_default() -> Self {
        let settings = crate::settings::get_settings();
        if let Some(domain) = settings.current_share_router_domain() {
            match normalize_tunnel_domain(domain) {
                Ok(domain) => return Self { domain },
                Err(err) => {
                    log::warn!(
                        "已忽略无效 share router 域名配置 `{}`，将使用默认节点: {}",
                        domain,
                        err
                    );
                }
            }
        }
        Self::default_public_service()
    }

    /// Whether this is a local/dev domain (localhost, 127.0.0.1, 0.0.0.0)
    pub fn is_local(&self) -> bool {
        let authority = extract_authority(&self.domain).unwrap_or_else(|| self.domain.clone());
        let host = authority.split(':').next().unwrap_or(&authority);
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
        let domain = extract_authority(&self.domain).unwrap_or_else(|| self.domain.clone());
        let proto = if self.is_local() { "http" } else { "https" };
        format!("{proto}://{subdomain}.{domain}")
    }

    pub fn matches_tunnel_url(&self, url_or_host: &str) -> bool {
        let Some(authority) = extract_authority(url_or_host) else {
            return false;
        };

        authority == self.domain || authority.ends_with(&format!(".{}", self.domain))
    }
}

pub fn normalize_tunnel_domain(input: &str) -> Result<String, String> {
    let trimmed = input.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err("Router domain is required".to_string());
    }
    if trimmed.chars().any(char::is_whitespace) {
        return Err("Router domain must not contain spaces".to_string());
    }

    let lower = trimmed.to_ascii_lowercase();
    let authority = if lower.starts_with("http://") || lower.starts_with("https://") {
        let url =
            reqwest::Url::parse(trimmed).map_err(|_| "Router domain URL is invalid".to_string())?;
        if url.scheme() != "http" && url.scheme() != "https" {
            return Err("Router domain must use http or https".to_string());
        }
        if url.username() != "" || url.password().is_some() {
            return Err("Router domain must not include credentials".to_string());
        }
        if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
            return Err("Router domain must not include path, query, or fragment".to_string());
        }
        url.host_str()
            .map(|host| match url.port() {
                Some(port) => format!("{host}:{port}"),
                None => host.to_string(),
            })
            .ok_or_else(|| "Router domain host is required".to_string())?
    } else {
        if trimmed.contains("://") {
            return Err("Router domain must use http or https".to_string());
        }
        if trimmed.contains('/') || trimmed.contains('?') || trimmed.contains('#') {
            return Err("Router domain must not include path, query, or fragment".to_string());
        }
        trimmed.to_string()
    }
    .to_ascii_lowercase();

    validate_tunnel_authority(&authority)?;
    if is_placeholder_tunnel_domain(&authority) {
        return Err("Router domain must not be an example placeholder".to_string());
    }
    Ok(authority)
}

pub fn is_placeholder_tunnel_domain(input: &str) -> bool {
    extract_authority(input)
        .as_deref()
        .is_some_and(is_placeholder_tunnel_authority)
}

fn is_placeholder_tunnel_authority(authority: &str) -> bool {
    let host = authority
        .rsplit_once(':')
        .filter(|(_, maybe_port)| maybe_port.chars().all(|ch| ch.is_ascii_digit()))
        .map_or(authority, |(host, _)| host);
    host == "example.com" || host.ends_with(".example.com")
}

fn validate_tunnel_authority(authority: &str) -> Result<(), String> {
    if authority.is_empty() || authority.len() > 253 {
        return Err("Router domain is invalid".to_string());
    }
    if authority.contains('@') || authority.contains('[') || authority.contains(']') {
        return Err("Router domain must be a hostname or IPv4 address".to_string());
    }

    let (host, port) = authority
        .rsplit_once(':')
        .filter(|(_, maybe_port)| maybe_port.chars().all(|ch| ch.is_ascii_digit()))
        .map_or((authority, None), |(host, port)| (host, Some(port)));

    if let Some(port) = port {
        let parsed = port
            .parse::<u16>()
            .map_err(|_| "Router domain port is invalid".to_string())?;
        if parsed == 0 {
            return Err("Router domain port is invalid".to_string());
        }
    }

    if host == "localhost" || host == "127.0.0.1" || host == "0.0.0.0" {
        return Ok(());
    }

    if host.parse::<std::net::Ipv4Addr>().is_ok() {
        return Ok(());
    }

    if !host.contains('.') {
        return Err("Router domain must be a valid hostname".to_string());
    }

    for label in host.split('.') {
        if label.is_empty()
            || label.len() > 63
            || label.starts_with('-')
            || label.ends_with('-')
            || !label
                .as_bytes()
                .iter()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
        {
            return Err("Router domain must be a valid hostname".to_string());
        }
    }

    Ok(())
}

impl Default for TunnelConfig {
    fn default() -> Self {
        Self::default_public_service()
    }
}

pub fn current_tunnel_config() -> Option<TunnelConfig> {
    crate::settings::get_settings()
        .current_share_router_domain()
        .and_then(|domain| match normalize_tunnel_domain(domain) {
            Ok(domain) => Some(TunnelConfig { domain }),
            Err(err) => {
                log::warn!("已忽略无效 share router 域名配置 `{}`: {}", domain, err);
                None
            }
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
    ClientWebHttp,
    Tcp,
}

impl TunnelType {
    pub fn as_str(&self) -> &'static str {
        match self {
            TunnelType::Http => "http",
            TunnelType::ClientWebHttp => "client-web-http",
            TunnelType::Tcp => "tcp",
        }
    }

    pub fn is_http_like(&self) -> bool {
        matches!(self, TunnelType::Http | TunnelType::ClientWebHttp)
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareUpstreamQuota {
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queried_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription_period_end: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub availability: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_until: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatch_limit_percent: Option<f64>,
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
    pub provider_type: Option<String>,
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
    #[serde(default)]
    pub codex_image_generation_enabled: bool,
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
    #[serde(default = "default_sale_market_kind")]
    pub sale_market_kind: String,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub access_by_app: HashMap<String, ShareAppAccess>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub app_settings: HashMap<String, ShareAppSettings>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub for_sale_official_price_percent_by_app: HashMap<String, u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub for_sale: String,
    pub subdomain: String,
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

fn default_sale_market_kind() -> String {
    "token".to_string()
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_country: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_country_iso3: Option<String>,
    pub created_at: i64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn matches_share_subdomain() {
        let config = TunnelConfig {
            domain: "share.custom-router.com".to_string(),
        };

        assert!(config.matches_tunnel_url("https://alpha.share.custom-router.com/v1"));
        assert!(config.matches_tunnel_url("alpha.share.custom-router.com"));
        assert!(!config.matches_tunnel_url("https://api.openai.com/v1"));
    }

    #[test]
    fn normalizes_custom_router_domains() {
        assert_eq!(
            normalize_tunnel_domain(" HTTPS://Share.Custom-Router.Com/ ").unwrap(),
            "share.custom-router.com"
        );
        assert_eq!(
            normalize_tunnel_domain("localhost:8787").unwrap(),
            "localhost:8787"
        );
        assert_eq!(
            normalize_tunnel_domain("http://127.0.0.1:8787").unwrap(),
            "127.0.0.1:8787"
        );
    }

    #[test]
    fn rejects_custom_router_domains_with_paths_or_credentials() {
        assert!(normalize_tunnel_domain("https://share.custom-router.com/v1").is_err());
        assert!(normalize_tunnel_domain("https://u:p@share.custom-router.com").is_err());
        assert!(normalize_tunnel_domain("ftp://share.custom-router.com").is_err());
        assert!(normalize_tunnel_domain("share.example.com").is_err());
        assert!(normalize_tunnel_domain("share").is_err());
        assert!(normalize_tunnel_domain("bad host.example.com").is_err());
    }

    #[test]
    fn tunnel_addr_uses_authority_for_historical_scheme_values() {
        let config = TunnelConfig {
            domain: "https://Share.Custom-Router.Com/".to_string(),
        };

        assert_eq!(
            config.get_tunnel_addr("alpha"),
            "https://alpha.share.custom-router.com"
        );
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
        settings.share_router_domain = Some("HTTPS://Share.Custom-Router.Com/".to_string());
        crate::settings::update_settings(settings).unwrap();

        assert!(is_share_tunnel_url(
            "https://alpha.share.custom-router.com/v1"
        ));
        assert!(is_share_tunnel_url("ALPHA.SHARE.CUSTOM-ROUTER.COM"));
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
            access_by_app: HashMap::new(),
            app_settings: HashMap::new(),
            for_sale_official_price_percent_by_app: HashMap::new(),
            description: None,
            for_sale: "No".to_string(),
            sale_market_kind: "token".to_string(),
            subdomain: "demo".to_string(),
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
                "saleMarketKind": "token",
                "subdomain": "demo",
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
            access_by_app: HashMap::new(),
            app_settings: HashMap::new(),
            for_sale_official_price_percent_by_app: pricing,
            description: Some("not signed by claim".to_string()),
            for_sale: "Yes".to_string(),
            sale_market_kind: "token".to_string(),
            subdomain: "demo".to_string(),
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
