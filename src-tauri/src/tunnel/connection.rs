use super::config::{ShareTunnelMetadata, TunnelConfig, TunnelType};
use super::error::TunnelError;
use super::identity;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaseResponse {
    pub connection_id: String,
    pub ssh_username: String,
    pub ssh_password: String,
    pub ssh_addr: String,
}

#[derive(Deserialize)]
struct ErrorResponse {
    message: String,
}

/// Request a short-lived tunnel lease from portr-rs.
pub async fn issue_lease(
    client: &reqwest::Client,
    config: &TunnelConfig,
    tunnel_type: TunnelType,
    subdomain: &str,
    share_metadata: Option<ShareTunnelMetadata>,
) -> Result<LeaseResponse, TunnelError> {
    issue_lease_inner(client, config, tunnel_type, subdomain, share_metadata, true).await
}

async fn issue_lease_inner(
    client: &reqwest::Client,
    config: &TunnelConfig,
    tunnel_type: TunnelType,
    subdomain: &str,
    share_metadata: Option<ShareTunnelMetadata>,
    allow_identity_reset_retry: bool,
) -> Result<LeaseResponse, TunnelError> {
    let url = format!("{}/v1/tunnels/lease", config.get_server_addr());
    let identity = identity::ensure_identity(client, config).await?;
    let timestamp_ms = chrono::Utc::now().timestamp_millis();
    let nonce = uuid::Uuid::new_v4().to_string();

    let payload = serde_json::json!({
        "installationId": identity.installation_id,
        "requestedSubdomain": subdomain,
        "tunnelType": tunnel_type.as_str(),
        "timestampMs": timestamp_ms,
        "nonce": nonce,
        "signature": identity::sign_lease_payload(
            &identity,
            &identity.installation_id,
            subdomain,
            tunnel_type.as_str(),
            timestamp_ms,
            &nonce,
        ),
        "share": share_metadata,
    });

    let resp = client
        .post(&url)
        .json(&payload)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| TunnelError::Api(format!("request failed: {e}")))?;

    if resp.status().is_success() {
        return resp
            .json()
            .await
            .map_err(|e| TunnelError::Api(format!("parse response: {e}")));
    }

    let status = resp.status();
    let body: Result<ErrorResponse, _> = resp.json().await;
    let msg = body
        .map(|b| b.message)
        .unwrap_or_else(|_| format!("HTTP {status}"));

    if allow_identity_reset_retry && msg.contains("installation not found") {
        log::warn!(
            "[Tunnel] portr-rs no longer recognizes installation {}, re-registering identity",
            identity.installation_id
        );
        identity::reset_identity()?;
        return Box::pin(issue_lease_inner(
            client,
            config,
            tunnel_type,
            subdomain,
            share_metadata,
            false,
        ))
        .await;
    }

    Err(TunnelError::Api(msg))
}
