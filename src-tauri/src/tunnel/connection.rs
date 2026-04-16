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
        if let Some(ref share) = share_metadata {
            log::warn!(
                "[Tunnel] Re-claiming share subdomain {} after installation reset",
                share.subdomain
            );
            claim_share_subdomain_inner(client, config, share, false).await?;
        }
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

    if allow_identity_reset_retry
        && msg.contains("share subdomain is not claimed")
        && share_metadata.is_some()
    {
        let share = share_metadata.as_ref().expect("checked is_some");
        log::warn!(
            "[Tunnel] share subdomain {} is no longer claimed on portr-rs, reclaiming before retry",
            share.subdomain
        );
        claim_share_subdomain_inner(client, config, share, true).await?;
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

async fn claim_share_subdomain_inner(
    client: &reqwest::Client,
    config: &TunnelConfig,
    share_metadata: &ShareTunnelMetadata,
    allow_identity_reset_retry: bool,
) -> Result<(), TunnelError> {
    let url = format!("{}/v1/shares/claim-subdomain", config.get_server_addr());
    let identity = identity::ensure_identity(client, config).await?;
    let resp = client
        .post(&url)
        .json(&serde_json::json!({
            "installationId": identity.installation_id,
            "share": share_metadata,
        }))
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| TunnelError::Api(format!("claim share subdomain request failed: {e}")))?;

    if resp.status().is_success() {
        return Ok(());
    }

    let status = resp.status();
    let text = resp
        .text()
        .await
        .unwrap_or_else(|_| format!("HTTP {status}"));
    let message = serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|value| {
            value
                .get("message")
                .and_then(|msg| msg.as_str())
                .map(str::to_string)
        })
        .unwrap_or(text);

    if allow_identity_reset_retry && message.contains("installation not found") {
        log::warn!(
            "[Tunnel] portr-rs no longer recognizes installation {}, re-registering identity before subdomain claim",
            identity.installation_id
        );
        identity::reset_identity()?;
        return Box::pin(claim_share_subdomain_inner(
            client,
            config,
            share_metadata,
            false,
        ))
        .await;
    }

    Err(TunnelError::Api(format!(
        "claim subdomain request failed: {message}"
    )))
}
