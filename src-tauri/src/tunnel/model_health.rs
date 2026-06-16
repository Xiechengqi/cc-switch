use crate::app_config::AppType;
use crate::database::{Database, ShareRecord};
use crate::error::AppError;
use crate::provider::Provider;
use crate::services::model_test::StreamCheckResult;
use crate::services::share::ShareService;
use crate::tunnel::config::{ShareModelHealthResult, ShareModelHealthSummary};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

const FIRST_HEALTH_CHECK_DELAY: Duration = Duration::from_secs(120);
const HEALTH_CHECK_INTERVAL: Duration = Duration::from_secs(30 * 60);
const QUOTA_BLOCK_HEALTH_REPEAT_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const RECENT_RESULT_LIMIT: usize = 3;

/// `(share_id, app_type)` → 该 share 在该 app slot 上最近一次的 stream check 结果。
///
/// 老实现按 app_type 单字段当 key，意味着"任意 share 探测过 codex"都会污染所有
/// share 的 snapshot；改成 per-share 后这条 share 自己看到的、上推给 router 的
/// model_health 就只跟自己绑了的 app 有关。
type HealthMap = HashMap<(String, String), HealthEntry>;

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

/// 返回属于该 share 的最近一次 model_health 探测结果，按 app 分桶。
///
/// `bound_apps` 是该 share 当前实际绑定的 app_type 集合；store 里残留的、
/// 已经不再绑定的旧条目会被静默丢掉。这一层防御让"删 share 时漏掉 purge"
/// 之类的 bug 也不至于把脏数据上推给 router。
pub async fn current_share_model_health_summary_for_share(
    share_id: &str,
    bound_apps: &HashSet<String>,
) -> ShareModelHealthSummary {
    let store = health_store().read().await;
    let mut summary = ShareModelHealthSummary::default();
    for ((entry_share, entry_app), entry) in store.iter() {
        if entry_share != share_id {
            continue;
        }
        if !bound_apps.contains(entry_app) {
            continue;
        }
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

/// 清空该 share 在 store 里所有 app 的结果。
/// 在 share 删除路径上调用，避免 store 永久膨胀。
pub async fn purge_share(share_id: &str) {
    let mut store = health_store().write().await;
    store.retain(|(entry_share, _), _| entry_share != share_id);
}

/// 清空该 share 在指定 app slot 上的结果。
/// 在 update_provider_binding 改绑/解绑时调用，避免新 provider 还没探测之前
/// 显示的是上一个 provider 的旧结果。
pub async fn purge_share_app(share_id: &str, app_type: &str) {
    let mut store = health_store().write().await;
    store.remove(&(share_id.to_string(), app_type.to_string()));
}

async fn run_share_model_health_cycle(
    db: &Arc<Database>,
    app_handle: &tauri::AppHandle,
) -> Result<(), AppError> {
    let shares = ShareService::list(db)?;
    let mut had_any_probe = false;

    for share in shares {
        if share.status != "active" {
            continue;
        }
        // 顺手做一次 GC：share 不再绑这个 app 了，把 store 里残留的条目顺带删了，
        // 防止"曾经绑过的 app"在 dashboard 上继续显示。
        prune_unbound_entries(&share).await;

        for (app_type_str, provider_id) in &share.bindings {
            let app_type = match normalize_app_type(app_type_str) {
                Some(app) => app,
                None => continue,
            };
            if provider_id.trim().is_empty() {
                continue;
            }
            had_any_probe = true;
            if let Err(err) = check_app(db, app_handle, &share, &app_type, provider_id).await {
                log::warn!(
                    "[ShareModelHealth] check failed (share={}, app={}): {err}",
                    share.id,
                    app_type.as_str()
                );
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    if !had_any_probe {
        log::debug!("[ShareModelHealth] no active share with bindings, skipped cycle");
    }
    Ok(())
}

async fn prune_unbound_entries(share: &ShareRecord) {
    let bound: HashSet<&str> = share
        .bindings
        .iter()
        .filter(|(_, pid)| !pid.trim().is_empty())
        .map(|(app, _)| app.as_str())
        .collect();
    let mut store = health_store().write().await;
    store.retain(|(entry_share, entry_app), _| {
        entry_share != &share.id || bound.contains(entry_app.as_str())
    });
}

fn normalize_app_type(value: &str) -> Option<AppType> {
    match value {
        "claude" => Some(AppType::Claude),
        "codex" => Some(AppType::Codex),
        "gemini" => Some(AppType::Gemini),
        _ => None,
    }
}

async fn check_app(
    db: &Arc<Database>,
    app_handle: &tauri::AppHandle,
    share: &ShareRecord,
    app_type: &AppType,
    provider_id: &str,
) -> Result<(), AppError> {
    let provider = match db.get_provider_by_id(provider_id, app_type.as_str())? {
        Some(provider) => provider,
        None => {
            // provider 已经被删但 binding 还在 — 不阻塞调度，只跳过本次。
            // 路由层 next request 会真正报错；这里跑探测没意义。
            return Ok(());
        }
    };

    if let Some(block) = active_quota_block_for_provider(app_type, &provider).await {
        record_health_result(
            &share.id,
            ShareModelHealthResult {
                app_type: app_type.as_str().to_string(),
                requested_model: app_type.as_str().to_string(),
                actual_model: app_type.as_str().to_string(),
                status: "quota_blocked".to_string(),
                recent_results: Vec::new(),
                status_code: None,
                latency_ms: 0,
                error_message: Some(format!(
                    "{} until {}",
                    block.blocked_reason,
                    block.blocked_until.as_deref().unwrap_or("quota reset")
                )),
                checked_at: chrono::Utc::now().timestamp(),
                source: "cc-switch-quota".to_string(),
                provider_id: Some(provider.id),
                provider_name: Some(provider.name),
            },
        )
        .await;
        return Ok(());
    }

    let started = Instant::now();
    let result = crate::commands::model_test::run_model_test_for_provider(
        db.as_ref(),
        Some(app_handle),
        app_type,
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
    record_health_result(&share.id, entry).await;
    Ok(())
}

async fn active_quota_block_for_provider(
    app_type: &AppType,
    provider: &Provider,
) -> Option<crate::services::oauth_quota::QuotaBlockStatus> {
    let auth_provider = auth_provider_for_model_health(app_type, provider)?;
    let service = crate::services::oauth_quota::global_oauth_quota_service()?;
    let cached = match provider
        .meta
        .as_ref()
        .and_then(|meta| meta.managed_account_id_for(auth_provider))
    {
        Some(account_id) => service.get(auth_provider, &account_id).await?,
        None => service.get_first_for_provider(auth_provider).await?,
    };
    let block = crate::services::oauth_quota::quota_block_status(&cached.quota)?;
    crate::services::oauth_quota::quota_block_is_active(&block).then_some(block)
}

fn auth_provider_for_model_health(app_type: &AppType, provider: &Provider) -> Option<&'static str> {
    let provider_type = provider
        .meta
        .as_ref()
        .and_then(|meta| meta.provider_type.as_deref());
    match app_type {
        AppType::Claude if provider_type == Some("claude_oauth") => Some("claude_oauth"),
        AppType::Claude if provider_type == Some("kiro_oauth") => Some("kiro_oauth"),
        AppType::Claude if provider_type == Some("github_copilot") => Some("github_copilot"),
        AppType::Claude if provider_type == Some("antigravity_oauth") => Some("antigravity_oauth"),
        AppType::Claude if provider_type == Some("cursor_oauth") => Some("cursor_oauth"),
        AppType::Codex
            if provider_type == Some("codex_oauth")
                || provider.is_codex_official_with_managed_auth() =>
        {
            Some("codex_oauth")
        }
        AppType::Codex if provider_type == Some("cursor_oauth") => Some("cursor_oauth"),
        AppType::Gemini
            if provider_type == Some("google_gemini_oauth")
                || provider.is_google_gemini_official_with_managed_auth() =>
        {
            Some("google_gemini_oauth")
        }
        AppType::Gemini if provider_type == Some("antigravity_oauth") => Some("antigravity_oauth"),
        _ => None,
    }
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

async fn record_health_result(share_id: &str, result: ShareModelHealthResult) {
    let key = (share_id.to_string(), result.app_type.clone());
    let mut store = health_store().write().await;
    let status = result.status.clone();
    let entry = match store.get_mut(&key) {
        Some(entry) => {
            if should_throttle_repeated_quota_block(&entry.result, &result) {
                return;
            }
            entry
        }
        None => {
            store.insert(
                key.clone(),
                HealthEntry {
                    result: result.clone(),
                    recent_results: VecDeque::new(),
                },
            );
            store
                .get_mut(&key)
                .expect("inserted health entry must exist")
        }
    };
    entry.recent_results.push_front(status);
    while entry.recent_results.len() > RECENT_RESULT_LIMIT {
        entry.recent_results.pop_back();
    }
    entry.result = result;
}

fn should_throttle_repeated_quota_block(
    previous: &ShareModelHealthResult,
    next: &ShareModelHealthResult,
) -> bool {
    if !is_quota_block_result(previous) || !is_quota_block_result(next) {
        return false;
    }
    if previous.error_message != next.error_message
        || previous.provider_id != next.provider_id
        || previous.requested_model != next.requested_model
        || previous.actual_model != next.actual_model
    {
        return false;
    }
    let min_next_checked_at =
        previous.checked_at + QUOTA_BLOCK_HEALTH_REPEAT_INTERVAL.as_secs() as i64;
    next.checked_at < min_next_checked_at
}

fn is_quota_block_result(result: &ShareModelHealthResult) -> bool {
    result.status == "quota_blocked" && result.source == "cc-switch-quota"
}

fn health_store() -> &'static RwLock<HealthMap> {
    SHARE_MODEL_HEALTH.get_or_init(|| RwLock::new(HashMap::new()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// 这些测试都改全局 store，必须串行，否则会互相串味。
    static STORE_LOCK: Mutex<()> = Mutex::new(());

    fn make_result(app: &str) -> ShareModelHealthResult {
        ShareModelHealthResult {
            app_type: app.to_string(),
            requested_model: app.to_string(),
            actual_model: app.to_string(),
            status: "success".to_string(),
            recent_results: Vec::new(),
            status_code: Some(200),
            latency_ms: 100,
            error_message: None,
            checked_at: 0,
            source: "test".to_string(),
            provider_id: Some("p".to_string()),
            provider_name: Some("P".to_string()),
        }
    }

    fn make_quota_block_result(app: &str, checked_at: i64) -> ShareModelHealthResult {
        ShareModelHealthResult {
            app_type: app.to_string(),
            requested_model: app.to_string(),
            actual_model: app.to_string(),
            status: "quota_blocked".to_string(),
            recent_results: Vec::new(),
            status_code: None,
            latency_ms: 0,
            error_message: Some(
                "long window quota exhausted until 2026-07-08T04:25:06+00:00".to_string(),
            ),
            checked_at,
            source: "cc-switch-quota".to_string(),
            provider_id: Some("p".to_string()),
            provider_name: Some("P".to_string()),
        }
    }

    async fn reset_store() {
        let mut store = health_store().write().await;
        store.clear();
    }

    #[tokio::test]
    async fn summary_filters_by_share_id_and_bound_apps() {
        let _guard = STORE_LOCK.lock().unwrap();
        reset_store().await;

        record_health_result("share-1", make_result("claude")).await;
        record_health_result("share-1", make_result("codex")).await;
        record_health_result("share-2", make_result("gemini")).await;

        let mut bound = HashSet::new();
        bound.insert("claude".to_string());

        let summary = current_share_model_health_summary_for_share("share-1", &bound).await;
        // share-1 的 codex 条目存在于 store，但 bound 集合里没有 codex —— 必须被过滤掉。
        assert_eq!(summary.claude.len(), 1);
        assert!(summary.codex.is_empty(), "codex must be filtered out");
        assert!(
            summary.gemini.is_empty(),
            "gemini belongs to share-2, must not leak"
        );
    }

    #[tokio::test]
    async fn purge_share_removes_only_that_shares_entries() {
        let _guard = STORE_LOCK.lock().unwrap();
        reset_store().await;

        record_health_result("share-1", make_result("claude")).await;
        record_health_result("share-2", make_result("claude")).await;
        purge_share("share-1").await;

        let mut bound = HashSet::new();
        bound.insert("claude".to_string());

        let summary_a = current_share_model_health_summary_for_share("share-1", &bound).await;
        let summary_b = current_share_model_health_summary_for_share("share-2", &bound).await;
        assert!(summary_a.claude.is_empty(), "share-1 should be purged");
        assert_eq!(summary_b.claude.len(), 1, "share-2 must remain intact");
    }

    #[tokio::test]
    async fn purge_share_app_removes_one_slot_only() {
        let _guard = STORE_LOCK.lock().unwrap();
        reset_store().await;

        record_health_result("share-1", make_result("claude")).await;
        record_health_result("share-1", make_result("codex")).await;
        purge_share_app("share-1", "codex").await;

        let mut bound = HashSet::new();
        bound.insert("claude".to_string());
        bound.insert("codex".to_string());

        let summary = current_share_model_health_summary_for_share("share-1", &bound).await;
        assert_eq!(
            summary.claude.len(),
            1,
            "claude must remain after codex purge"
        );
        assert!(summary.codex.is_empty(), "codex slot was just purged");
    }

    #[tokio::test]
    async fn repeated_quota_block_health_is_throttled_until_repeat_interval() {
        let _guard = STORE_LOCK.lock().unwrap();
        reset_store().await;

        record_health_result("share-1", make_quota_block_result("codex", 100)).await;
        record_health_result("share-1", make_quota_block_result("codex", 100 + 30 * 60)).await;

        let mut bound = HashSet::new();
        bound.insert("codex".to_string());
        let summary = current_share_model_health_summary_for_share("share-1", &bound).await;
        let result = summary.codex.first().expect("codex quota block");
        assert_eq!(result.checked_at, 100);
        assert_eq!(result.recent_results.len(), 1);

        record_health_result(
            "share-1",
            make_quota_block_result(
                "codex",
                100 + QUOTA_BLOCK_HEALTH_REPEAT_INTERVAL.as_secs() as i64,
            ),
        )
        .await;

        let summary = current_share_model_health_summary_for_share("share-1", &bound).await;
        let result = summary
            .codex
            .first()
            .expect("codex quota block after interval");
        assert_eq!(
            result.checked_at,
            100 + QUOTA_BLOCK_HEALTH_REPEAT_INTERVAL.as_secs() as i64
        );
        assert_eq!(result.recent_results.len(), 2);
    }
}
