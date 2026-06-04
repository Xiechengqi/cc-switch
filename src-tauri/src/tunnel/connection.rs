use super::config::{ShareTunnelMetadata, TunnelConfig, TunnelType};
use super::error::TunnelError;
use super::identity;
use serde::Deserialize;
use serde::Serialize;
use tokio::time::sleep;

const SHARE_ROUTER_REQUEST_TIMEOUT_SECS: u64 = 20;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaseResponse {
    pub connection_id: String,
    pub ssh_username: String,
    pub ssh_password: String,
    pub ssh_addr: String,
    /// SSH host key 指纹（`SHA256:<base64-nopad>` 格式）。cc-switch-router ≥ 当前版本会在
    /// /v1/tunnels/lease 响应里返回，客户端据此校验 SSH 服务端身份，防止中间人。
    /// 老服务端没有这个字段时为 None；此时退化为 "跳过校验 + 日志告警"。
    #[serde(default)]
    pub ssh_host_fingerprint: Option<String>,
}

#[derive(Deserialize)]
struct ErrorResponse {
    message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientTunnelClaim {
    pub owner_email: String,
    pub subdomain: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientTunnelView {
    pub installation_id: String,
    pub owner_email: String,
    pub subdomain: String,
    pub enabled: bool,
    pub tunnel_url: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub last_seen_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientTunnelResponse {
    pub ok: bool,
    #[serde(default)]
    pub tunnel: Option<ClientTunnelView>,
}

async fn read_error_message(resp: reqwest::Response) -> String {
    let status = resp.status();
    let body: Result<ErrorResponse, _> = resp.json().await;
    body.map(|b| b.message)
        .unwrap_or_else(|_| format!("HTTP {status}"))
}

fn describe_share_router_send_error(operation: &str, url: &str, err: reqwest::Error) -> String {
    if err.is_timeout() {
        return format!(
            "{operation} timed out after {SHARE_ROUTER_REQUEST_TIMEOUT_SECS}s: {url}. 请检查分享节点是否可访问，或切换到其他分享节点后重试"
        );
    }
    if err.is_connect() {
        return format!(
            "{operation} connection failed: {url}. 请检查网络、DNS、代理或防火墙，或切换到其他分享节点后重试: {err}"
        );
    }
    format!("{operation} request failed: {url}: {err}")
}

async fn send_share_router_request(
    request: reqwest::RequestBuilder,
    operation: &str,
    url: &str,
) -> Result<reqwest::Response, TunnelError> {
    let retry_request = request.try_clone();
    match request.send().await {
        Ok(resp) => Ok(resp),
        Err(err) if (err.is_timeout() || err.is_connect()) && retry_request.is_some() => {
            log::warn!("[Tunnel] {operation} failed once for {url}, retrying: {err}");
            sleep(std::time::Duration::from_millis(500)).await;
            retry_request
                .expect("checked is_some")
                .send()
                .await
                .map_err(|retry_err| {
                    TunnelError::Api(describe_share_router_send_error(operation, url, retry_err))
                })
        }
        Err(err) => Err(TunnelError::Api(describe_share_router_send_error(
            operation, url, err,
        ))),
    }
}

/// Request a short-lived tunnel lease from the cc-switch-router service.
pub async fn issue_lease(
    client: &reqwest::Client,
    config: &TunnelConfig,
    tunnel_type: TunnelType,
    subdomain: &str,
    share_metadata: Option<ShareTunnelMetadata>,
) -> Result<LeaseResponse, TunnelError> {
    issue_lease_inner(client, config, tunnel_type, subdomain, share_metadata, true).await
}

pub async fn claim_client_tunnel(
    client: &reqwest::Client,
    config: &TunnelConfig,
    claim: &ClientTunnelClaim,
) -> Result<ClientTunnelView, TunnelError> {
    write_client_tunnel(client, config, "client_tunnel_claim", claim, true).await
}

pub async fn update_client_tunnel(
    client: &reqwest::Client,
    config: &TunnelConfig,
    claim: &ClientTunnelClaim,
) -> Result<ClientTunnelView, TunnelError> {
    write_client_tunnel(client, config, "client_tunnel_update", claim, true).await
}

async fn write_client_tunnel(
    client: &reqwest::Client,
    config: &TunnelConfig,
    action: &str,
    claim: &ClientTunnelClaim,
    allow_identity_reset_retry: bool,
) -> Result<ClientTunnelView, TunnelError> {
    let identity = identity::ensure_identity(client, config).await?;
    let timestamp_ms = chrono::Utc::now().timestamp_millis();
    let nonce = uuid::Uuid::new_v4().to_string();
    let signature = identity::sign_action_payload(
        &identity,
        &identity.installation_id,
        action,
        claim,
        timestamp_ms,
        &nonce,
    )?;
    let url = if action == "client_tunnel_claim" {
        format!(
            "{}/v1/installations/client-tunnel/claim",
            config.get_server_addr()
        )
    } else {
        format!(
            "{}/v1/installations/client-tunnel",
            config.get_server_addr()
        )
    };
    let request = if action == "client_tunnel_claim" {
        client.post(&url)
    } else {
        client.patch(&url)
    };
    let resp = send_share_router_request(
        request
            .json(&serde_json::json!({
                "installationId": identity.installation_id,
                "timestampMs": timestamp_ms,
                "nonce": nonce,
                "signature": signature,
                "tunnel": claim,
            }))
            .timeout(std::time::Duration::from_secs(
                SHARE_ROUTER_REQUEST_TIMEOUT_SECS,
            )),
        "write client tunnel",
        &url,
    )
    .await?;
    if resp.status().is_success() {
        let body: ClientTunnelResponse = resp
            .json()
            .await
            .map_err(|e| TunnelError::Api(format!("parse client tunnel response: {e}")))?;
        if body.ok {
            return body
                .tunnel
                .ok_or_else(|| TunnelError::Api("client tunnel response missing tunnel".into()));
        }
        return Err(TunnelError::Api(
            "client tunnel request was not accepted".into(),
        ));
    }

    let message = read_error_message(resp).await;
    if allow_identity_reset_retry && identity::should_reset_identity_for_api_error(&message) {
        identity::refresh_installation_registration(client, config).await?;
        return Box::pin(write_client_tunnel(client, config, action, claim, false)).await;
    }
    Err(TunnelError::Api(message))
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
    let request = client
        .post(&url)
        .json(&payload)
        .timeout(std::time::Duration::from_secs(
            SHARE_ROUTER_REQUEST_TIMEOUT_SECS,
        ));

    let resp = send_share_router_request(request, "issue tunnel lease", &url).await?;

    if resp.status().is_success() {
        let lease: LeaseResponse = resp
            .json()
            .await
            .map_err(|e| TunnelError::Api(format!("parse response: {e}")))?;
        validate_lease_response(&lease, subdomain)?;
        log::info!(
            "[Tunnel] lease issued subdomain={} connection_id={} ssh_username={} ssh_addr={}",
            subdomain,
            lease.connection_id,
            lease.ssh_username,
            lease.ssh_addr
        );
        return Ok(lease);
    }

    let msg = read_error_message(resp).await;

    if allow_identity_reset_retry && identity::should_reset_identity_for_api_error(&msg) {
        log::warn!(
            "[Tunnel] lease request rejected for installation {}, refreshing identity and retrying once: {}",
            identity.installation_id,
            msg
        );
        identity::refresh_installation_registration(client, config).await?;
        if let Some(ref share) = share_metadata {
            log::warn!(
                "[Tunnel] Re-claiming share subdomain {} after installation refresh",
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

    if allow_identity_reset_retry && msg.contains("share subdomain is not claimed") {
        if let Some(share) = share_metadata.as_ref() {
            log::warn!(
                "[Tunnel] share subdomain {} is no longer claimed on cc-switch-router, reclaiming before retry",
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
    let timestamp_ms = chrono::Utc::now().timestamp_millis();
    let nonce = uuid::Uuid::new_v4().to_string();
    let claim = share_metadata.claim_payload();
    let signature = identity::sign_action_payload(
        &identity,
        &identity.installation_id,
        "share_claim_subdomain",
        &claim,
        timestamp_ms,
        &nonce,
    )?;
    let resp = send_share_router_request(
        client
            .post(&url)
            .json(&serde_json::json!({
                "installationId": identity.installation_id,
                "timestampMs": timestamp_ms,
                "nonce": nonce,
                "signature": signature,
                "claim": claim,
                "share": share_metadata,
            }))
            .timeout(std::time::Duration::from_secs(
                SHARE_ROUTER_REQUEST_TIMEOUT_SECS,
            )),
        "claim share subdomain",
        &url,
    )
    .await?;

    if resp.status().is_success() {
        return Ok(());
    }

    let message = read_error_message(resp).await;

    if allow_identity_reset_retry && identity::should_reset_identity_for_api_error(&message) {
        log::warn!(
            "[Tunnel] share subdomain claim rejected for installation {}, refreshing identity and retrying once: {}",
            identity.installation_id,
            message
        );
        identity::refresh_installation_registration(client, config).await?;
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

fn validate_lease_response(lease: &LeaseResponse, subdomain: &str) -> Result<(), TunnelError> {
    if lease.connection_id.trim().is_empty() {
        return Err(TunnelError::Api(format!(
            "share router returned invalid tunnel lease for {subdomain}: empty connection id"
        )));
    }
    let username = lease.ssh_username.trim();
    if username.is_empty() || username == "root" {
        return Err(TunnelError::Api(format!(
            "share router returned invalid tunnel lease for {subdomain}: unexpected SSH username `{username}`"
        )));
    }
    if lease.ssh_addr.trim().is_empty() {
        return Err(TunnelError::Api(format!(
            "share router returned invalid tunnel lease for {subdomain}: empty SSH address"
        )));
    }
    Ok(())
}
