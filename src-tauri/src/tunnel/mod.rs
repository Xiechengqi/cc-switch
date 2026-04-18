pub mod config;
pub mod connection;
pub mod error;
mod forward;
mod health;
mod identity;
mod ssh;
pub mod sync;

use config::{TunnelConfig, TunnelInfo, TunnelRequest, TunnelType};
use error::TunnelError;
use health::HealthChecker;
use ssh::SshTunnel;

use crate::database::Database;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, RwLock};
use tokio::task::JoinHandle;

struct TunnelHandle {
    pub info: TunnelInfo,
    tunnel: Arc<RwLock<SshTunnel>>,
    shutdown_tx: broadcast::Sender<()>,
    healthy: Arc<AtomicBool>,
    _health_task: JoinHandle<()>,
    _reconnect_task: JoinHandle<()>,
    _share_sync_task: JoinHandle<()>,
}

/// Manages multiple portr tunnels, each identified by a string key (share_id).
pub struct TunnelManager {
    config: Option<TunnelConfig>,
    tunnels: HashMap<String, TunnelHandle>,
    http_client: reqwest::Client,
}

impl TunnelManager {
    pub fn new() -> Self {
        Self {
            config: None,
            tunnels: HashMap::new(),
            http_client: reqwest::Client::new(),
        }
    }

    pub fn set_config(&mut self, config: TunnelConfig) {
        self.config = Some(config);
    }

    pub fn is_configured(&self) -> bool {
        self.config.is_some()
    }

    /// Start a new tunnel. Returns tunnel info including the public URL.
    pub async fn start_tunnel(
        &mut self,
        id: &str,
        req: TunnelRequest,
        db: Arc<Database>,
    ) -> Result<TunnelInfo, TunnelError> {
        if self.tunnels.contains_key(id) {
            return Err(TunnelError::AlreadyExists(id.to_string()));
        }

        let config = self
            .config
            .as_ref()
            .ok_or_else(|| TunnelError::NotConfigured("portr config not set".to_string()))?
            .clone();

        // 1. Create connection via portr API
        let lease = connection::issue_lease(
            &self.http_client,
            &config,
            req.tunnel_type,
            &req.subdomain,
            req.share_metadata.clone(),
        )
        .await?;

        // 2. Establish SSH tunnel
        let (shutdown_tx, _) = broadcast::channel(16);
        let ssh_tunnel =
            SshTunnel::connect(&config, &lease, &req.local_addr, shutdown_tx.clone()).await?;

        let remote_port = ssh_tunnel.remote_port();
        let tunnel_url = config.get_tunnel_addr(&req.subdomain);

        let info = TunnelInfo {
            tunnel_url: tunnel_url.clone(),
            subdomain: req.subdomain.clone(),
            remote_port,
            healthy: true,
        };

        let healthy = Arc::new(AtomicBool::new(true));
        let tunnel = Arc::new(RwLock::new(ssh_tunnel));

        // 3. Start health check (HTTP only)
        let (reconnect_tx, reconnect_rx) = mpsc::channel(1);
        let health_task = if req.tunnel_type == TunnelType::Http {
            let checker = HealthChecker::new(
                config.clone(),
                req.subdomain.clone(),
                self.http_client.clone(),
                healthy.clone(),
            );
            let shutdown_rx = shutdown_tx.subscribe();
            tokio::spawn(async move {
                checker.run(shutdown_rx, reconnect_tx).await;
            })
        } else {
            tokio::spawn(async {})
        };

        // 4. Start reconnect handler
        let reconnect_tunnel = tunnel.clone();
        let reconnect_config = config.clone();
        let reconnect_http = self.http_client.clone();
        let reconnect_subdomain = req.subdomain.clone();
        let reconnect_type = req.tunnel_type;
        let reconnect_share_metadata = req.share_metadata.clone();
        let reconnect_healthy = healthy.clone();
        let mut shutdown_for_reconnect = shutdown_tx.subscribe();

        let reconnect_task = tokio::spawn(async move {
            Self::reconnect_loop(
                reconnect_rx,
                reconnect_tunnel,
                reconnect_config,
                reconnect_http,
                reconnect_type,
                reconnect_subdomain,
                reconnect_share_metadata,
                reconnect_healthy,
                &mut shutdown_for_reconnect,
            )
            .await;
        });

        let share_sync_task = if let Some(share_metadata) = req.share_metadata.clone() {
            let share_id_for_sync = share_metadata.share_id.clone();
            let db_for_sync = db.clone();
            let mut shutdown_for_share_sync = shutdown_tx.subscribe();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(30));
                loop {
                    tokio::select! {
                        _ = interval.tick() => {
                            let latest = match db_for_sync.get_share_by_id(&share_id_for_sync) {
                                Ok(Some(share)) => {
                                    let mut metadata = sync::share_metadata_from_record(&share);
                                    metadata.support = sync::query_share_support(&db_for_sync).await;
                                    metadata
                                }
                                Ok(None) => {
                                    log::debug!(
                                        "[Tunnel] share {} missing during periodic sync",
                                        share_id_for_sync
                                    );
                                    continue;
                                }
                                Err(err) => {
                                    log::warn!(
                                        "[Tunnel] read share {} for periodic sync failed: {}",
                                        share_id_for_sync, err
                                    );
                                    continue;
                                }
                            };
                            if let Err(err) = sync::sync_share_metadata_now(latest).await {
                                log::warn!(
                                    "[Tunnel] periodic share sync failed for {}: {}",
                                    share_id_for_sync, err
                                );
                            }
                        }
                        _ = shutdown_for_share_sync.recv() => break,
                    }
                }
            })
        } else {
            tokio::spawn(async {})
        };

        self.tunnels.insert(
            id.to_string(),
            TunnelHandle {
                info,
                tunnel,
                shutdown_tx,
                healthy,
                _health_task: health_task,
                _reconnect_task: reconnect_task,
                _share_sync_task: share_sync_task,
            },
        );

        Ok(self.tunnels[id].info.clone())
    }

    async fn reconnect_loop(
        mut reconnect_rx: mpsc::Receiver<()>,
        tunnel: Arc<RwLock<SshTunnel>>,
        config: TunnelConfig,
        http_client: reqwest::Client,
        tunnel_type: TunnelType,
        subdomain: String,
        share_metadata: Option<config::ShareTunnelMetadata>,
        healthy: Arc<AtomicBool>,
        shutdown_rx: &mut broadcast::Receiver<()>,
    ) {
        loop {
            tokio::select! {
                Some(()) = reconnect_rx.recv() => {
                    log::info!("[Tunnel] Reconnecting tunnel for {subdomain}...");
                    match connection::issue_lease(
                        &http_client,
                        &config,
                        tunnel_type,
                        &subdomain,
                        share_metadata.clone(),
                    )
                    .await
                    {
                        Ok(lease) => {
                            let mut t = tunnel.write().await;
                            match t.reconnect(&lease).await {
                                Ok(()) => {
                                    healthy.store(true, Ordering::Relaxed);
                                    log::info!("[Tunnel] Reconnected successfully for {subdomain}");
                                }
                                Err(e) => {
                                    log::error!("[Tunnel] Reconnect SSH failed: {e}");
                                }
                            }
                        }
                        Err(e) => {
                            log::error!("[Tunnel] Reconnect API failed: {e}");
                        }
                    }
                }
                _ = shutdown_rx.recv() => break,
                else => break,
            }
        }
    }

    /// Stop and remove a tunnel.
    pub async fn stop_tunnel(&mut self, id: &str) -> Result<(), TunnelError> {
        let handle = self
            .tunnels
            .remove(id)
            .ok_or_else(|| TunnelError::NotFound(id.to_string()))?;

        let _ = handle.shutdown_tx.send(());
        handle.tunnel.write().await.close().await;

        log::info!("[Tunnel] Stopped tunnel {id}");
        Ok(())
    }

    /// Get tunnel info (with live healthy status).
    pub fn get_info(&self, id: &str) -> Option<TunnelInfo> {
        self.tunnels.get(id).map(|h| {
            let mut info = h.info.clone();
            info.healthy = h.healthy.load(Ordering::Relaxed);
            info
        })
    }

    pub fn list_tunnels(&self) -> Vec<(String, TunnelInfo)> {
        self.tunnels
            .iter()
            .map(|(k, h)| {
                let mut info = h.info.clone();
                info.healthy = h.healthy.load(Ordering::Relaxed);
                (k.clone(), info)
            })
            .collect()
    }

    /// Shutdown all tunnels.
    pub async fn shutdown(&mut self) {
        let ids: Vec<String> = self.tunnels.keys().cloned().collect();
        for id in ids {
            let _ = self.stop_tunnel(&id).await;
        }
    }
}

impl Default for TunnelManager {
    fn default() -> Self {
        Self::new()
    }
}
