//! Active failover health probing.
//!
//! This worker complements request-path failover.  Request failures still update
//! the circuit breaker immediately; this worker periodically probes the active
//! provider so an idle client can fail over or recover without waiting for the
//! next user request.

use super::{failover_switch::FailoverSwitchManager, provider_router::ProviderRouter};
use crate::app_config::AppType;
use crate::database::Database;
use crate::provider::Provider;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

const NORMAL_INTERVAL: Duration = Duration::from_secs(5 * 60);
const MAX_BACKOFF_INTERVAL: Duration = Duration::from_secs(60 * 60);
const LOOP_TICK: Duration = Duration::from_secs(15);
const CANDIDATE_SWITCH_SETTLE_DELAY: Duration = Duration::from_millis(250);
const FAILOVER_PROBE_APPS: [AppType; 3] = [AppType::Claude, AppType::Codex, AppType::Gemini];

#[derive(Debug, Clone)]
struct AppProbeState {
    remaining_delay: Duration,
    all_failed_delay: Duration,
    all_failed: bool,
}

impl Default for AppProbeState {
    fn default() -> Self {
        Self {
            remaining_delay: NORMAL_INTERVAL,
            all_failed_delay: NORMAL_INTERVAL,
            all_failed: false,
        }
    }
}

impl AppProbeState {
    fn mark_success(&mut self) {
        self.remaining_delay = NORMAL_INTERVAL;
        self.all_failed_delay = NORMAL_INTERVAL;
        self.all_failed = false;
    }

    fn mark_all_failed(&mut self) {
        self.all_failed = true;
        self.remaining_delay = self.all_failed_delay;
        self.all_failed_delay = (self.all_failed_delay * 2).min(MAX_BACKOFF_INTERVAL);
    }

    fn tick(&mut self, elapsed: Duration) -> bool {
        self.remaining_delay = self.remaining_delay.saturating_sub(elapsed);
        self.remaining_delay.is_zero()
    }
}

pub fn spawn_failover_health_probe_scheduler(
    db: Arc<Database>,
    app_handle: Option<tauri::AppHandle>,
    provider_router: Arc<ProviderRouter>,
    failover_manager: Arc<FailoverSwitchManager>,
    proxy_status: Arc<RwLock<super::types::ProxyStatus>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut states: HashMap<&'static str, AppProbeState> = FAILOVER_PROBE_APPS
            .iter()
            .map(|app| (app.as_str(), AppProbeState::default()))
            .collect();

        loop {
            if !proxy_status.read().await.running {
                tokio::time::sleep(LOOP_TICK).await;
                continue;
            }

            for app_type in FAILOVER_PROBE_APPS {
                let Some(state) = states.get_mut(app_type.as_str()) else {
                    continue;
                };

                if !state.tick(LOOP_TICK) {
                    continue;
                }

                if !is_failover_probe_enabled(&db, app_type.as_str()).await {
                    state.mark_success();
                    continue;
                }

                match run_app_probe_cycle(
                    &db,
                    app_handle.as_ref(),
                    provider_router.as_ref(),
                    failover_manager.as_ref(),
                    &app_type,
                    state.all_failed,
                )
                .await
                {
                    Ok(ProbeCycleOutcome::Healthy { provider_id }) => {
                        log::debug!(
                            "[FailoverHealth] {} probe healthy: provider_id={provider_id}",
                            app_type.as_str()
                        );
                        state.mark_success();
                    }
                    Ok(ProbeCycleOutcome::AllFailed) => {
                        state.mark_all_failed();
                        log::warn!(
                            "[FailoverHealth] {} all failover providers failed; next probe in {}s",
                            app_type.as_str(),
                            state.remaining_delay.as_secs()
                        );
                    }
                    Ok(ProbeCycleOutcome::Skipped) => {
                        state.mark_success();
                    }
                    Err(err) => {
                        log::warn!(
                            "[FailoverHealth] {} probe cycle failed: {err}",
                            app_type.as_str()
                        );
                    }
                }
            }

            tokio::time::sleep(LOOP_TICK).await;
        }
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ProbeCycleOutcome {
    Healthy { provider_id: String },
    AllFailed,
    Skipped,
}

async fn is_failover_probe_enabled(db: &Arc<Database>, app_type: &str) -> bool {
    db.get_proxy_config_for_app(app_type)
        .await
        .map(|config| config.enabled && config.auto_failover_enabled)
        .unwrap_or(false)
}

async fn run_app_probe_cycle(
    db: &Arc<Database>,
    app_handle: Option<&tauri::AppHandle>,
    provider_router: &ProviderRouter,
    failover_manager: &FailoverSwitchManager,
    app_type: &AppType,
    probe_full_queue: bool,
) -> Result<ProbeCycleOutcome, crate::error::AppError> {
    let app = app_type.as_str();
    let queue = db.get_failover_queue(app)?;
    if queue.is_empty() {
        return Ok(ProbeCycleOutcome::Skipped);
    }

    let all_providers = db.get_all_providers(app)?;
    let original_provider_id = crate::settings::get_effective_current_provider(db, app_type)?
        .or_else(|| db.get_current_provider(app).ok().flatten());
    let Some(original_provider_id) = original_provider_id else {
        return Ok(ProbeCycleOutcome::Skipped);
    };

    let original_provider = all_providers.get(&original_provider_id).cloned();

    if !probe_full_queue {
        if let Some(provider) = original_provider.as_ref() {
            let result = probe_provider(db, app_handle, provider_router, app_type, provider).await;
            if result.success {
                return Ok(ProbeCycleOutcome::Healthy {
                    provider_id: provider.id.clone(),
                });
            }
        }
    }

    let ordered_candidates = ordered_failover_candidates(
        queue.into_iter().map(|item| item.provider_id).collect(),
        &original_provider_id,
        probe_full_queue,
    );

    for provider_id in ordered_candidates {
        let Some(provider) = all_providers.get(&provider_id).cloned() else {
            continue;
        };
        if switch_to_provider(failover_manager, app_handle, app, &provider).await {
            tokio::time::sleep(CANDIDATE_SWITCH_SETTLE_DELAY).await;
        }

        let result = probe_provider(db, app_handle, provider_router, app_type, &provider).await;
        if result.success {
            return Ok(ProbeCycleOutcome::Healthy {
                provider_id: provider.id,
            });
        }
    }

    if let Some(original_provider) = original_provider.as_ref() {
        let _ = switch_to_provider(failover_manager, app_handle, app, original_provider).await;
    }

    Ok(ProbeCycleOutcome::AllFailed)
}

#[derive(Debug)]
struct ProbeProviderResult {
    success: bool,
}

async fn probe_provider(
    db: &Arc<Database>,
    app_handle: Option<&tauri::AppHandle>,
    provider_router: &ProviderRouter,
    app_type: &AppType,
    provider: &Provider,
) -> ProbeProviderResult {
    let app = app_type.as_str();
    let result = crate::commands::model_test::run_model_test_for_provider(
        db, app_handle, app_type, provider,
    )
    .await;

    match result {
        Ok(result) => {
            let _ = db.save_stream_check_log(&provider.id, &provider.name, app, &result);
            let _ = crate::tunnel::model_health::record_failover_probe_result_for_provider(
                db,
                app_type,
                provider,
                result.clone(),
            )
            .await;
            if result.success {
                provider_router
                    .reset_provider_breaker(&provider.id, app)
                    .await;
                let _ = db.reset_provider_health(&provider.id, app).await;
            } else {
                let _ = provider_router
                    .record_result(
                        &provider.id,
                        app,
                        false,
                        false,
                        Some(format!("failover probe failed: {}", result.message)),
                    )
                    .await;
            }
            ProbeProviderResult {
                success: result.success,
            }
        }
        Err(err) => {
            let _ = provider_router
                .record_result(
                    &provider.id,
                    app,
                    false,
                    false,
                    Some(format!("failover probe error: {err}")),
                )
                .await;
            ProbeProviderResult { success: false }
        }
    }
}

async fn switch_to_provider(
    failover_manager: &FailoverSwitchManager,
    app_handle: Option<&tauri::AppHandle>,
    app_type: &str,
    provider: &Provider,
) -> bool {
    match failover_manager
        .try_switch(app_handle, app_type, &provider.id, &provider.name)
        .await
    {
        Ok(switched) => switched,
        Err(err) => {
            log::warn!(
                "[FailoverHealth] failed to switch {} to {} ({}): {err}",
                app_type,
                provider.name,
                provider.id
            );
            false
        }
    }
}

fn ordered_failover_candidates(
    queue: Vec<String>,
    current_provider_id: &str,
    include_current: bool,
) -> Vec<String> {
    if include_current {
        return queue;
    }

    let Some(index) = queue.iter().position(|id| id == current_provider_id) else {
        return queue;
    };

    queue
        .iter()
        .skip(index + 1)
        .chain(queue.iter().take(index))
        .filter(|id| id.as_str() != current_provider_id)
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_order_starts_after_current_and_wraps() {
        let order = ordered_failover_candidates(
            vec!["a".to_string(), "b".to_string(), "c".to_string()],
            "b",
            false,
        );
        assert_eq!(order, vec!["c".to_string(), "a".to_string()]);
    }

    #[test]
    fn full_queue_probe_keeps_priority_order() {
        let order = ordered_failover_candidates(
            vec!["a".to_string(), "b".to_string(), "c".to_string()],
            "b",
            true,
        );
        assert_eq!(
            order,
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
    }

    #[test]
    fn all_failed_backoff_caps_at_one_hour() {
        let mut state = AppProbeState::default();
        state.mark_all_failed();
        assert_eq!(state.remaining_delay, Duration::from_secs(5 * 60));
        state.mark_all_failed();
        assert_eq!(state.remaining_delay, Duration::from_secs(10 * 60));
        state.mark_all_failed();
        assert_eq!(state.remaining_delay, Duration::from_secs(20 * 60));
        state.mark_all_failed();
        assert_eq!(state.remaining_delay, Duration::from_secs(40 * 60));
        state.mark_all_failed();
        assert_eq!(state.remaining_delay, Duration::from_secs(60 * 60));
        state.mark_all_failed();
        assert_eq!(state.remaining_delay, Duration::from_secs(60 * 60));
    }

    #[test]
    fn success_resets_backoff() {
        let mut state = AppProbeState::default();
        state.mark_all_failed();
        state.mark_all_failed();
        state.mark_success();
        assert_eq!(state.remaining_delay, NORMAL_INTERVAL);
        assert_eq!(state.all_failed_delay, NORMAL_INTERVAL);
        assert!(!state.all_failed);
    }

    #[test]
    fn tick_only_becomes_due_after_remaining_delay_elapses() {
        let mut state = AppProbeState::default();
        assert!(!state.tick(Duration::from_secs(60)));
        assert_eq!(state.remaining_delay, Duration::from_secs(4 * 60));
        assert!(state.tick(Duration::from_secs(4 * 60)));
        assert_eq!(state.remaining_delay, Duration::ZERO);
    }
}
