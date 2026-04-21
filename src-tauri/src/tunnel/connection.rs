use super::config::{ShareTunnelMetadata, TunnelConfig, TunnelType};
use super::error::TunnelError;
use super::identity;
use serde::Deserialize;
use tokio::time::sleep;

const PORTR_REQUEST_TIMEOUT_SECS: u64 = 20;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaseResponse {
    pub connection_id: String,
    pub ssh_username: String,
    pub ssh_password: String,
    pub ssh_addr: String,
    /// SSH host key 指纹（`SHA256:<base64-nopad>` 格式）。portr-rs ≥ 当前版本会在
    /// /v1/tunnels/lease 响应里返回，客户端据此校验 SSH 服务端身份，防止中间人。
    /// 老服务端没有这个字段时为 None；此时退化为 "跳过校验 + 日志告警"。
    #[serde(default)]
    pub ssh_host_fingerprint: Option<String>,
}

#[derive(Deserialize)]
struct ErrorResponse {
    message: String,
}

async fn read_error_message(resp: reqwest::Response) -> String {
    let status = resp.status();
    let body: Result<ErrorResponse, _> = resp.json().await;
    body.map(|b| b.message)
        .unwrap_or_else(|_| format!("HTTP {status}"))
}

fn describe_portr_send_error(operation: &str, url: &str, err: reqwest::Error) -> String {
    if err.is_timeout() {
        return format!(
            "{operation} timed out after {PORTR_REQUEST_TIMEOUT_SECS}s: {url}. 请检查分享节点是否可访问，或切换到其他分享节点后重试"
        );
    }
    if err.is_connect() {
        return format!(
            "{operation} connection failed: {url}. 请检查网络、DNS、代理或防火墙，或切换到其他分享节点后重试: {err}"
        );
    }
    format!("{operation} request failed: {url}: {err}")
}

async fn send_portr_request(
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
                    TunnelError::Api(describe_portr_send_error(operation, url, retry_err))
                })
        }
        Err(err) => Err(TunnelError::Api(describe_portr_send_error(
            operation, url, err,
        ))),
    }
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

    let resp = send_portr_request(
        client
            .post(&url)
            .json(&payload)
            .timeout(std::time::Duration::from_secs(PORTR_REQUEST_TIMEOUT_SECS)),
        "issue tunnel lease",
        &url,
    )
    .await?;

    if resp.status().is_success() {
        return resp
            .json()
            .await
            .map_err(|e| TunnelError::Api(format!("parse response: {e}")));
    }

    let msg = read_error_message(resp).await;

    if allow_identity_reset_retry && identity::should_reset_identity_for_api_error(&msg) {
        log::warn!(
            "[Tunnel] lease request rejected for installation {}, resetting identity and retrying once: {}",
            identity.installation_id,
            msg
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
    let timestamp_ms = chrono::Utc::now().timestamp_millis();
    let nonce = uuid::Uuid::new_v4().to_string();
    let signature = identity::sign_action_payload(
        &identity,
        &identity.installation_id,
        "share_claim_subdomain",
        share_metadata,
        timestamp_ms,
        &nonce,
    )?;
    let resp = send_portr_request(
        client
            .post(&url)
            .json(&serde_json::json!({
                "installationId": identity.installation_id,
                "timestampMs": timestamp_ms,
                "nonce": nonce,
                "signature": signature,
                "share": share_metadata,
            }))
            .timeout(std::time::Duration::from_secs(PORTR_REQUEST_TIMEOUT_SECS)),
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
            "[Tunnel] share subdomain claim rejected for installation {}, resetting identity and retrying once: {}",
            identity.installation_id,
            message
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
