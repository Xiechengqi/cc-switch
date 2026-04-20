use super::config::TunnelConfig;
use super::connection::LeaseResponse;
use super::error::TunnelError;
use super::forward;
use async_trait::async_trait;
use rand::Rng;
use russh::client;
use russh::keys::key::PublicKey;
use russh::{Channel, Disconnect};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, Mutex};

/// SSH tunnel that maintains a connection to the portr server and forwards
/// incoming traffic to a local address.
pub struct SshTunnel {
    handle: Arc<Mutex<Option<client::Handle<TunnelHandler>>>>,
    remote_port: u16,
    local_addr: String,
    config: TunnelConfig,
    connection_id: String,
    shutdown_tx: broadcast::Sender<()>,
}

impl SshTunnel {
    /// Establish SSH connection, authenticate, and start remote port forwarding.
    pub async fn connect(
        config: &TunnelConfig,
        lease: &LeaseResponse,
        local_addr: &str,
        shutdown_tx: broadcast::Sender<()>,
    ) -> Result<Self, TunnelError> {
        let (handle, remote_port) =
            Self::establish_connection(config, lease, local_addr, shutdown_tx.clone()).await?;

        Ok(Self {
            handle: Arc::new(Mutex::new(Some(handle))),
            remote_port,
            local_addr: local_addr.to_string(),
            config: config.clone(),
            connection_id: lease.connection_id.clone(),
            shutdown_tx,
        })
    }

    async fn establish_connection(
        _config: &TunnelConfig,
        lease: &LeaseResponse,
        local_addr: &str,
        shutdown_tx: broadcast::Sender<()>,
    ) -> Result<(client::Handle<TunnelHandler>, u16), TunnelError> {
        let ssh_config = Arc::new(client::Config {
            keepalive_interval: Some(std::time::Duration::from_secs(15)),
            keepalive_max: 3,
            ..Default::default()
        });

        let (fwd_tx, fwd_rx) = mpsc::unbounded_channel();
        let handler = TunnelHandler {
            fwd_tx,
            expected_fingerprint: lease.ssh_host_fingerprint.clone(),
            ssh_addr: lease.ssh_addr.clone(),
        };

        let ssh_addr = &lease.ssh_addr;
        let mut handle = client::connect(ssh_config, ssh_addr, handler)
            .await
            .map_err(|e| {
                TunnelError::SshConnect(describe_connect_error(ssh_addr, &e.to_string()))
            })?;

        let auth_ok = handle
            .authenticate_password(&lease.ssh_username, &lease.ssh_password)
            .await
            .map_err(|e| TunnelError::SshConnect(format!("auth error: {e}")))?;

        if !auth_ok {
            return Err(TunnelError::SshAuth);
        }

        // Try random ports in 20000-30000 for remote forwarding
        let remote_port = Self::request_forward(&mut handle, 10).await?;

        // Spawn accept loop to handle forwarded connections
        let local_addr_owned = local_addr.to_string();
        let mut shutdown_rx = shutdown_tx.subscribe();
        tokio::spawn(async move {
            Self::accept_loop(fwd_rx, &local_addr_owned, &mut shutdown_rx).await;
        });

        Ok((handle, remote_port))
    }

    /// Try multiple random ports until one succeeds
    async fn request_forward(
        handle: &mut client::Handle<TunnelHandler>,
        attempts: usize,
    ) -> Result<u16, TunnelError> {
        for _ in 0..attempts {
            let port: u16 = rand::thread_rng().gen_range(20000..30000);
            match handle.tcpip_forward("0.0.0.0", port as u32).await {
                Ok(bound_port) => {
                    let effective_port = if bound_port == 0 {
                        port
                    } else {
                        bound_port as u16
                    };
                    log::info!(
                        "[Tunnel] Remote port forward established on port {}",
                        effective_port
                    );
                    return Ok(effective_port);
                }
                Err(e) => {
                    log::debug!("[Tunnel] Port {port} failed: {e}, trying next...");
                    continue;
                }
            }
        }

        Err(TunnelError::PortForward(format!(
            "all {attempts} port attempts failed"
        )))
    }

    /// Accept forwarded connections and spawn TCP forwarding tasks
    async fn accept_loop(
        mut fwd_rx: mpsc::UnboundedReceiver<Channel<client::Msg>>,
        local_addr: &str,
        shutdown_rx: &mut broadcast::Receiver<()>,
    ) {
        loop {
            tokio::select! {
                Some(channel) = fwd_rx.recv() => {
                    let addr = local_addr.to_string();
                    tokio::spawn(async move {
                        let stream = channel.into_stream();
                        if let Err(e) = forward::forward_tcp(stream, &addr).await {
                            log::debug!("[Tunnel] forward error: {e}");
                        }
                    });
                }
                _ = shutdown_rx.recv() => {
                    log::info!("[Tunnel] Accept loop shutting down");
                    break;
                }
                else => break,
            }
        }
    }

    pub fn remote_port(&self) -> u16 {
        self.remote_port
    }

    /// Reconnect: close existing session, re-establish everything.
    pub async fn reconnect(&mut self, lease: &LeaseResponse) -> Result<(), TunnelError> {
        // Close existing handle
        if let Some(handle) = self.handle.lock().await.take() {
            let _ = handle.disconnect(Disconnect::ByApplication, "", "en").await;
        }

        let (new_handle, new_port) = Self::establish_connection(
            &self.config,
            lease,
            &self.local_addr,
            self.shutdown_tx.clone(),
        )
        .await?;

        *self.handle.lock().await = Some(new_handle);
        self.remote_port = new_port;
        self.connection_id = lease.connection_id.clone();

        log::info!("[Tunnel] Reconnected on port {}", self.remote_port);
        Ok(())
    }

    pub async fn close(&mut self) {
        if let Some(handle) = self.handle.lock().await.take() {
            let _ = handle.disconnect(Disconnect::ByApplication, "", "en").await;
        }
    }
}

/// 常量时间字符串比较，避免时序侧信道。
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn describe_connect_error(ssh_addr: &str, raw: &str) -> String {
    if raw.contains("Disconnect") || raw.contains("Disconnected") {
        return format!(
            "{raw}. SSH URL `{ssh_addr}` likely points to a non-SSH service, or portr-rs SSH listener is not actually running on that port"
        );
    }
    raw.to_string()
}

/// russh client handler that receives forwarded TCP channels from the server.
struct TunnelHandler {
    fwd_tx: mpsc::UnboundedSender<Channel<client::Msg>>,
    /// 由 lease 响应提供的期望 host key 指纹（`SHA256:<base64-nopad>`）。
    /// None 表示老服务端未返回指纹，为向后兼容放行但打印告警。
    expected_fingerprint: Option<String>,
    ssh_addr: String,
}

#[async_trait]
impl client::Handler for TunnelHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKey,
    ) -> Result<bool, Self::Error> {
        let actual = format!("SHA256:{}", server_public_key.fingerprint());
        match &self.expected_fingerprint {
            Some(expected) => {
                if constant_time_eq(expected.as_bytes(), actual.as_bytes()) {
                    log::info!(
                        "[Tunnel] SSH host key 校验通过 (addr={}, fp={})",
                        self.ssh_addr,
                        actual
                    );
                    Ok(true)
                } else {
                    log::error!(
                        "[Tunnel] SSH host key 不匹配！可能存在中间人攻击 (addr={}, expected={}, actual={})",
                        self.ssh_addr,
                        expected,
                        actual
                    );
                    Ok(false)
                }
            }
            None => {
                log::warn!(
                    "[Tunnel] portr-rs 未返回 ssh_host_fingerprint，跳过 host key 校验 (addr={}, actual={})。请升级服务端以启用校验。",
                    self.ssh_addr,
                    actual
                );
                Ok(true)
            }
        }
    }

    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: Channel<client::Msg>,
        _connected_address: &str,
        _connected_port: u32,
        _originator_address: &str,
        _originator_port: u32,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        // Send the forwarded channel to the accept loop
        let _ = self.fwd_tx.send(channel);
        Ok(())
    }
}
