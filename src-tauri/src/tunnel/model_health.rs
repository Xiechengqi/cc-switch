use crate::app_config::AppType;
use crate::database::Database;
use crate::error::AppError;
use crate::services::share::ShareService;
use crate::services::stream_check::StreamCheckResult;
use crate::settings;
use crate::tunnel::config::{ShareModelHealthResult, ShareModelHealthSummary};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

const FIRST_HEALTH_CHECK_DELAY: Duration = Duration::from_secs(120);
const HEALTH_CHECK_INTERVAL: Duration = Duration::from_secs(30 * 60);
const RECENT_RESULT_LIMIT: usize = 3;

type HealthMap = HashMap<String, HealthEntry>;

static SHARE_MODEL_HEALTH: OnceLock<RwLock<HealthMap>> = OnceLock::new();

#[derive(Debug, Clone)]
struct HealthEntry {
    result: ShareModelHealthResult,
    recent_results: VecDeque<String>,
}

pub fn spawn_share_model_health_scheduler(db: Arc<Database>, app_handle: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_HEALTH_CHECK_DELAY).await;
        loop {
            if let Err(err) = run_share_model_health_cycle(&db, &app_handle).await {
                log::warn!("[ShareModelHealth] model health cycle failed: {err}");
            }
            tokio::time::sleep(HEALTH_CHECK_INTERVAL).await;
        }
    });
}

pub async fn current_share_model_health_summary() -> ShareModelHealthSummary {
    let store = health_store().read().await;
    let mut summary = ShareModelHealthSummary::default();
    for entry in store.values() {
        let mut result = entry.result.clone();
        result.recent_results = entry.recent_results.iter().cloned().collect();
        match result.app_type.as_str() {
            "claude" => summary.claude.push(result),
            "codex" => summary.codex.push(result),
            "gemini" => summary.gemini.push(result),
            _ => {}
        }
    }
    summary
        .claude
        .sort_by(|a, b| b.checked_at.cmp(&a.checked_at));
    summary
        .codex
        .sort_by(|a, b| b.checked_at.cmp(&a.checked_at));
    summary
        .gemini
        .sort_by(|a, b| b.checked_at.cmp(&a.checked_at));
    summary
}

async fn run_share_model_health_cycle(
    db: &Arc<Database>,
    app_handle: &tauri::AppHandle,
) -> Result<(), AppError> {
    let has_active_share = ShareService::list(db)?
        .into_iter()
        .any(|share| share.status == "active");
    if !has_active_share {
        return Ok(());
    }

    let support = crate::tunnel::sync::query_share_support(db).await;
    let apps = [
        (support.claude, AppType::Claude),
        (support.codex, AppType::Codex),
        (support.gemini, AppType::Gemini),
    ];
    for (enabled, app_type) in apps {
        if !enabled {
            continue;
        }
        if let Err(err) = check_app(db, app_handle, app_type).await {
            log::warn!("[ShareModelHealth] check failed: {err}");
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Ok(())
}

async fn check_app(
    db: &Arc<Database>,
    app_handle: &tauri::AppHandle,
    app_type: AppType,
) -> Result<(), AppError> {
    let provider_id = match settings::get_effective_current_provider(db, &app_type)? {
        Some(provider_id) => provider_id,
        None => return Ok(()),
    };
    let provider = match db.get_provider_by_id(&provider_id, app_type.as_str())? {
        Some(provider) => provider,
        None => return Ok(()),
    };

    let started = Instant::now();
    let result = crate::commands::run_stream_check_for_provider(
        db.as_ref(),
        Some(app_handle),
        &app_type,
        &provider,
    )
    .await;
    let latency_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    let entry = match result {
        Ok(result) => {
            result_to_health_entry(app_type.as_str(), &provider.id, &provider.name, result)
        }
        Err(err) => ShareModelHealthResult {
            app_type: app_type.as_str().to_string(),
            requested_model: app_type.as_str().to_string(),
            actual_model: app_type.as_str().to_string(),
            status: "failed".to_string(),
            recent_results: Vec::new(),
            status_code: None,
            latency_ms,
            error_message: Some(err.to_string()),
            checked_at: chrono::Utc::now().timestamp(),
            source: "cc-switch-scheduled".to_string(),
            provider_id: Some(provider.id),
            provider_name: Some(provider.name),
        },
    };
    record_health_result(entry).await;
    Ok(())
}

fn result_to_health_entry(
    app_type: &str,
    provider_id: &str,
    provider_name: &str,
    result: StreamCheckResult,
) -> ShareModelHealthResult {
    let status = if result.success { "success" } else { "failed" };
    ShareModelHealthResult {
        app_type: app_type.to_string(),
        requested_model: result.model_used.clone(),
        actual_model: result.model_used,
        status: status.to_string(),
        recent_results: Vec::new(),
        status_code: result.http_status,
        latency_ms: result.response_time_ms.unwrap_or(0),
        error_message: if result.success {
            None
        } else {
            Some(result.message)
        },
        checked_at: result.tested_at,
        source: "cc-switch-scheduled".to_string(),
        provider_id: Some(provider_id.to_string()),
        provider_name: Some(provider_name.to_string()),
    }
}

async fn record_health_result(result: ShareModelHealthResult) {
    let key = result.app_type.clone();
    let mut store = health_store().write().await;
    let entry = store.entry(key).or_insert_with(|| HealthEntry {
        result: result.clone(),
        recent_results: VecDeque::new(),
    });
    entry.recent_results.push_front(result.status.clone());
    while entry.recent_results.len() > RECENT_RESULT_LIMIT {
        entry.recent_results.pop_back();
    }
    entry.result = result;
}

fn health_store() -> &'static RwLock<HealthMap> {
    SHARE_MODEL_HEALTH.get_or_init(|| RwLock::new(HashMap::new()))
}
