use super::config::TunnelConfig;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;

/// HTTP health checker for an active tunnel.
///
/// Sends periodic pings to `https://{subdomain}.{tunnel_url}` with the
/// `X-Portr-Ping-Request: true` header. Consecutive failures trigger a
/// reconnect callback.
pub struct HealthChecker {
    config: TunnelConfig,
    subdomain: String,
    http_client: reqwest::Client,
    consecutive_failures: AtomicU32,
    max_retries: u32,
    healthy: Arc<AtomicBool>,
}

impl HealthChecker {
    pub fn new(
        config: TunnelConfig,
        subdomain: String,
        http_client: reqwest::Client,
        healthy: Arc<AtomicBool>,
    ) -> Self {
        Self {
            config,
            subdomain,
            http_client,
            consecutive_failures: AtomicU32::new(0),
            max_retries: 3,
            healthy,
        }
    }

    /// Run health check loop. Returns when shutdown signal received or max retries exceeded.
    pub async fn run(
        &self,
        mut shutdown_rx: broadcast::Receiver<()>,
        reconnect_tx: tokio::sync::mpsc::Sender<()>,
    ) {
        let mut interval = tokio::time::interval(Duration::from_secs(3));

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    match self.check_once().await {
                        Ok(()) => {
                            self.consecutive_failures.store(0, Ordering::Relaxed);
                            self.healthy.store(true, Ordering::Relaxed);
                        }
                        Err(e) => {
                            let failures = self.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1;
                            let reconnect_now = should_reconnect_immediately(&e);
                            log::debug!(
                                "[Tunnel] Health check failed ({}/{}): {}",
                                failures, self.max_retries, e
                            );
                            self.healthy.store(false, Ordering::Relaxed);

                            if reconnect_now || failures >= self.max_retries {
                                log::warn!(
                                    "[Tunnel] {} requesting reconnect",
                                    if reconnect_now {
                                        "Health check hit a terminal tunnel error,"
                                    } else {
                                        "Max health check retries exceeded,"
                                    }
                                );
                                let _ = reconnect_tx.send(()).await;
                                self.consecutive_failures.store(0, Ordering::Relaxed);
                            }
                        }
                    }
                }
                _ = shutdown_rx.recv() => {
                    break;
                }
            }
        }
    }

    async fn check_once(&self) -> Result<(), String> {
        let url = self.config.get_tunnel_addr(&self.subdomain);

        let resp = self
            .http_client
            .get(&url)
            .header("X-Portr-Ping-Request", "true")
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .map_err(|e| format!("request failed: {e}"))?;

        if resp
            .headers()
            .get("X-Portr-Error")
            .and_then(|v| v.to_str().ok())
            == Some("true")
        {
            let reason = resp
                .headers()
                .get("X-Portr-Error-Reason")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("unknown");
            return Err(format!("portr error: {reason}"));
        }

        Ok(())
    }
}

fn should_reconnect_immediately(error: &str) -> bool {
    error.contains("portr error: unregistered-subdomain")
        || error.contains("connection-lost")
        || error.contains("request failed")
}
