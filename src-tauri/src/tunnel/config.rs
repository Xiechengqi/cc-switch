use serde::{Deserialize, Serialize};

/// Portr server configuration — stored in AppSettings
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelConfig {
    /// Portr domain (e.g. "example.com" or "127.0.0.1:8787")
    pub domain: String,
}

impl TunnelConfig {
    pub fn default_public_service() -> Self {
        Self {
            domain: "127.0.0.1:8787".to_string(),
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
}

impl Default for TunnelConfig {
    fn default() -> Self {
        Self::default_public_service()
    }
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ShareSupport {
    pub claude: bool,
    pub codex: bool,
    pub gemini: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareTunnelMetadata {
    pub share_id: String,
    pub share_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub for_sale: String,
    pub subdomain: String,
    pub share_token: String,
    pub app_type: String,
    pub provider_id: Option<String>,
    pub token_limit: i64,
    pub tokens_used: i64,
    pub requests_count: i64,
    pub share_status: String,
    pub created_at: String,
    pub expires_at: String,
    #[serde(default)]
    pub support: ShareSupport,
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
    pub created_at: i64,
}
