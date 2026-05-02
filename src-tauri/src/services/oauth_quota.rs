use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::sync::{broadcast, RwLock};

use crate::app_config::AppType;
use crate::commands::{ClaudeOAuthState, CodexOAuthState, CopilotAuthState, GeminiOAuthState};
use crate::database::Database;
use crate::provider::Provider;
use crate::proxy::providers::claude_oauth_auth::ClaudeOAuthManager;
use crate::proxy::providers::codex_oauth_auth::CodexOAuthManager;
use crate::proxy::providers::copilot_auth::{CopilotAuthManager, CopilotUsageResponse};
use crate::proxy::providers::gemini_oauth_auth::GeminiOAuthManager;
use crate::services::subscription::{
    query_claude_quota_with_token, query_codex_quota, query_gemini_quota_with_token,
    CredentialStatus, QuotaTier, SubscriptionQuota,
};

const STARTUP_REFRESH_DELAY_SECS: u64 = 10;
const SWITCH_REFRESH_COOLDOWN_SECS: i64 = 60;

#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct OauthQuotaKey {
    pub auth_provider: String,
    pub account_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedOauthQuota {
    pub auth_provider: String,
    pub account_id: String,
    pub provider_id: Option<String>,
    pub provider_name: Option<String>,
    pub app_type: Option<String>,
    pub quota: SubscriptionQuota,
    pub refreshed_at: i64,
    pub next_refresh_at: Option<i64>,
    pub source: String,
}

#[derive(Debug, Clone)]
pub struct OauthQuotaTarget {
    pub app_type: String,
    pub provider_id: String,
    pub provider_name: String,
    pub auth_provider: String,
    pub account_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OauthQuotaUpdatedEvent {
    pub auth_provider: String,
    pub account_id: String,
    pub provider_id: Option<String>,
    pub app_type: Option<String>,
    pub refreshed_at: i64,
    pub success: bool,
}

/// Per-key broadcast channel; 缓存未命中时首胜者建立 channel，
/// 竞败者订阅并等待同一次刷新的结果。
type InFlightSender = broadcast::Sender<CachedOauthQuota>;

#[derive(Default)]
pub struct OauthQuotaService {
    cache: RwLock<HashMap<OauthQuotaKey, CachedOauthQuota>>,
    /// 使用 std::sync::Mutex：临界区只做 HashMap get/insert/remove，
    /// 用同步锁才能在 Drop 里安全清理（tokio::sync::Mutex 不支持从 Drop 里取锁）。
    in_flight: StdMutex<HashMap<OauthQuotaKey, InFlightSender>>,
}

/// 确保 leader 即使在刷新过程中 panic 或被 cancel，也能从 in_flight 中清理自己的 key，
/// 让后续请求可以重新进入刷新流程而不是永远拿到陈旧的 Receiver。
struct InFlightGuard<'a> {
    service: &'a OauthQuotaService,
    key: OauthQuotaKey,
}

impl Drop for InFlightGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut map) = self.service.in_flight.lock() {
            // 若 leader 异常退出，remove 掉 sender 会让竞败者的 recv 立刻返回 Err，
            // 然后走 fallback: 读缓存；若缓存也没有，就向上报错而不是永远挂起。
            map.remove(&self.key);
        }
    }
}

static GLOBAL_OAUTH_QUOTA_SERVICE: OnceLock<Arc<OauthQuotaService>> = OnceLock::new();

pub fn set_global_oauth_quota_service(service: Arc<OauthQuotaService>) {
    let _ = GLOBAL_OAUTH_QUOTA_SERVICE.set(service);
}

pub fn global_oauth_quota_service() -> Option<Arc<OauthQuotaService>> {
    GLOBAL_OAUTH_QUOTA_SERVICE.get().cloned()
}

impl OauthQuotaService {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn get(&self, auth_provider: &str, account_id: &str) -> Option<CachedOauthQuota> {
        let cache = self.cache.read().await;
        cache
            .get(&OauthQuotaKey {
                auth_provider: auth_provider.to_string(),
                account_id: account_id.to_string(),
            })
            .cloned()
    }

    pub async fn refresh_selected_targets(
        &self,
        app: Option<&AppHandle>,
        db: &Arc<Database>,
        managers: &OauthQuotaManagers,
        source: &str,
    ) {
        let targets = self.discover_selected_targets(db, managers).await;
        for target in dedupe_targets(targets) {
            if let Err(err) = self
                .refresh_target(app, managers, target, source, None)
                .await
            {
                log::debug!("[OauthQuota] refresh selected target skipped/failed: {err}");
            }
        }
    }

    async fn discover_selected_targets(
        &self,
        db: &Arc<Database>,
        managers: &OauthQuotaManagers,
    ) -> Vec<OauthQuotaTarget> {
        let mut targets = Vec::new();
        for app_type in [AppType::Claude, AppType::Codex, AppType::Gemini] {
            let current_id = match crate::settings::get_effective_current_provider(db, &app_type) {
                Ok(Some(id)) => id,
                Ok(None) => continue,
                Err(err) => {
                    log::debug!(
                        "[OauthQuota] failed to resolve current provider for {}: {err}",
                        app_type.as_str()
                    );
                    continue;
                }
            };
            let provider = match db.get_provider_by_id(&current_id, app_type.as_str()) {
                Ok(Some(provider)) => provider,
                Ok(None) => continue,
                Err(err) => {
                    log::debug!(
                        "[OauthQuota] failed to load current provider {current_id} for {}: {err}",
                        app_type.as_str()
                    );
                    continue;
                }
            };
            if let Some(target) = self
                .target_from_provider(&app_type, &current_id, &provider, managers)
                .await
            {
                targets.push(target);
            }
        }
        targets
    }

    async fn target_from_provider(
        &self,
        app_type: &AppType,
        provider_id: &str,
        provider: &Provider,
        managers: &OauthQuotaManagers,
    ) -> Option<OauthQuotaTarget> {
        let auth_provider = provider_auth_provider(app_type, provider)?;
        let account_id = resolve_provider_account_id(&auth_provider, provider, managers).await?;
        Some(OauthQuotaTarget {
            app_type: app_type.as_str().to_string(),
            provider_id: provider_id.to_string(),
            provider_name: provider.name.clone(),
            auth_provider,
            account_id,
        })
    }

    async fn refresh_target(
        &self,
        app: Option<&AppHandle>,
        managers: &OauthQuotaManagers,
        target: OauthQuotaTarget,
        source: &str,
        interval_secs_override: Option<i64>,
    ) -> Result<CachedOauthQuota, String> {
        let key = OauthQuotaKey {
            auth_provider: target.auth_provider.clone(),
            account_id: target.account_id.clone(),
        };
        if let Some(cached) = self
            .cache_hit_for_cooldown(&key, source, interval_secs_override)
            .await
        {
            return Ok(cached);
        }

        // 选一个角色：
        // - 首胜者（is_leader == true）：持有 broadcast::Sender，真正触发上游刷新。
        // - 竞败者：持有 broadcast::Receiver，等待首胜者广播结果。
        let (is_leader, mut rx_opt) = {
            let mut in_flight = self
                .in_flight
                .lock()
                .map_err(|e| format!("in_flight mutex poisoned: {e}"))?;
            match in_flight.get(&key) {
                Some(sender) => (false, Some(sender.subscribe())),
                None => {
                    let (tx, _) = broadcast::channel::<CachedOauthQuota>(1);
                    in_flight.insert(key.clone(), tx);
                    (true, None)
                }
            }
        };

        if !is_leader {
            let rx = rx_opt.as_mut().expect("non-leader must have a receiver");
            match rx.recv().await {
                Ok(cached) => return Ok(cached),
                Err(e) => {
                    if let Some(cached) = self.get(&key.auth_provider, &key.account_id).await {
                        return Ok(cached);
                    }
                    return Err(format!("quota refresh dropped: {e}"));
                }
            }
        }

        // === leader 执行实际刷新 ===
        // Drop guard 保证：无论下面是否 panic / 提前 return，都能清理 in_flight。
        let _cleanup_guard = InFlightGuard {
            service: self,
            key: key.clone(),
        };

        let quota = match target.auth_provider.as_str() {
            "codex_oauth" => refresh_codex_quota(managers, &target.account_id).await,
            "claude_oauth" => refresh_claude_quota(managers, &target.account_id).await,
            "google_gemini_oauth" => refresh_gemini_quota(managers, &target.account_id).await,
            "github_copilot" => refresh_copilot_quota(managers, &target.account_id).await,
            other => SubscriptionQuota::error(
                other,
                CredentialStatus::NotFound,
                format!("unsupported OAuth quota provider: {other}"),
            ),
        };

        let now = now_millis();
        let interval_ms = interval_secs_override
            .unwrap_or_else(|| read_refresh_interval().as_secs() as i64)
            * 1000;
        let cached = CachedOauthQuota {
            auth_provider: target.auth_provider.clone(),
            account_id: target.account_id.clone(),
            provider_id: Some(target.provider_id.clone()),
            provider_name: Some(target.provider_name.clone()),
            app_type: Some(target.app_type.clone()),
            quota,
            refreshed_at: now,
            next_refresh_at: Some(now + interval_ms),
            source: source.to_string(),
        };
        {
            let mut cache = self.cache.write().await;
            cache.insert(key.clone(), cached.clone());
        }

        // 从 in_flight 里取出 sender 并广播给所有订阅者；
        // 之后 guard 的 drop 就是 no-op（key 已不存在）。
        if let Ok(mut in_flight) = self.in_flight.lock() {
            if let Some(sender) = in_flight.remove(&key) {
                let _ = sender.send(cached.clone());
            }
        }

        if let Some(app) = app {
            let _ = app.emit(
                "oauth-quota-updated",
                OauthQuotaUpdatedEvent {
                    auth_provider: cached.auth_provider.clone(),
                    account_id: cached.account_id.clone(),
                    provider_id: cached.provider_id.clone(),
                    app_type: cached.app_type.clone(),
                    refreshed_at: cached.refreshed_at,
                    success: cached.quota.success,
                },
            );
        }

        Ok(cached)
    }

    async fn cache_hit_for_cooldown(
        &self,
        key: &OauthQuotaKey,
        source: &str,
        interval_secs_override: Option<i64>,
    ) -> Option<CachedOauthQuota> {
        let cached = self.get(&key.auth_provider, &key.account_id).await?;
        let now = now_millis();
        let cooldown_ms = match source {
            "switch" => SWITCH_REFRESH_COOLDOWN_SECS * 1000,
            _ => {
                interval_secs_override.unwrap_or_else(|| read_refresh_interval().as_secs() as i64)
                    * 1000
            }
        };
        if now - cached.refreshed_at < cooldown_ms {
            Some(cached)
        } else {
            None
        }
    }
}

#[derive(Clone)]
pub struct OauthQuotaManagers {
    pub codex: Arc<RwLock<CodexOAuthManager>>,
    pub claude: Arc<RwLock<ClaudeOAuthManager>>,
    pub gemini: Arc<RwLock<GeminiOAuthManager>>,
    pub copilot: Arc<RwLock<CopilotAuthManager>>,
}

impl OauthQuotaManagers {
    pub fn from_states(
        codex: &CodexOAuthState,
        claude: &ClaudeOAuthState,
        gemini: &GeminiOAuthState,
        copilot: &CopilotAuthState,
    ) -> Self {
        Self {
            codex: Arc::clone(&codex.0),
            claude: Arc::clone(&claude.0),
            gemini: Arc::clone(&gemini.0),
            copilot: Arc::clone(&copilot.0),
        }
    }
}

pub fn spawn_oauth_quota_refresher(
    app: AppHandle,
    db: Arc<Database>,
    service: Arc<OauthQuotaService>,
    managers: OauthQuotaManagers,
) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(STARTUP_REFRESH_DELAY_SECS)).await;
        loop {
            service
                .refresh_selected_targets(Some(&app), &db, &managers, "background")
                .await;
            tokio::time::sleep(read_refresh_interval()).await;
        }
    });
}

fn provider_auth_provider(app_type: &AppType, provider: &Provider) -> Option<String> {
    let provider_type = provider
        .meta
        .as_ref()
        .and_then(|meta| meta.provider_type.as_deref());
    if matches!(app_type, AppType::Claude) && provider_type == Some("claude_oauth") {
        return Some("claude_oauth".to_string());
    }
    if matches!(app_type, AppType::Claude)
        && (provider_type == Some("github_copilot")
            || provider
                .meta
                .as_ref()
                .and_then(|meta| meta.usage_script.as_ref())
                .and_then(|script| script.template_type.as_deref())
                == Some("github_copilot"))
    {
        return Some("github_copilot".to_string());
    }
    if matches!(app_type, AppType::Codex)
        && (provider_type == Some("codex_oauth") || provider.is_codex_official_with_managed_auth())
    {
        return Some("codex_oauth".to_string());
    }
    if matches!(app_type, AppType::Gemini) && provider_type == Some("google_gemini_oauth") {
        return Some("google_gemini_oauth".to_string());
    }
    None
}

async fn resolve_provider_account_id(
    auth_provider: &str,
    provider: &Provider,
    managers: &OauthQuotaManagers,
) -> Option<String> {
    if let Some(meta) = provider.meta.as_ref() {
        if let Some(id) = meta.managed_account_id_for(auth_provider) {
            if !id.trim().is_empty() {
                return Some(id);
            }
        }
    }

    match auth_provider {
        "codex_oauth" => managers.codex.read().await.default_account_id().await,
        "claude_oauth" => managers.claude.read().await.default_account_id().await,
        "google_gemini_oauth" => managers.gemini.read().await.default_account_id().await,
        "github_copilot" => managers
            .copilot
            .read()
            .await
            .list_accounts()
            .await
            .first()
            .map(|account| account.id.clone()),
        _ => None,
    }
}

pub async fn resolve_account_id_for_auth_provider(
    auth_provider: &str,
    account_id: Option<String>,
    managers: &OauthQuotaManagers,
) -> Option<String> {
    if let Some(id) = account_id {
        if !id.trim().is_empty() {
            return Some(id);
        }
    }
    match auth_provider {
        "codex_oauth" => managers.codex.read().await.default_account_id().await,
        "claude_oauth" => managers.claude.read().await.default_account_id().await,
        "google_gemini_oauth" => managers.gemini.read().await.default_account_id().await,
        "github_copilot" => managers
            .copilot
            .read()
            .await
            .list_accounts()
            .await
            .first()
            .map(|account| account.id.clone()),
        _ => None,
    }
}

async fn refresh_codex_quota(managers: &OauthQuotaManagers, account_id: &str) -> SubscriptionQuota {
    let manager = managers.codex.read().await;
    match manager.get_valid_token_for_account(account_id).await {
        Ok(token) => {
            query_codex_quota(
                &token,
                Some(account_id),
                "codex_oauth",
                "Codex OAuth access token expired or rejected. Please re-login via cc-switch.",
            )
            .await
        }
        Err(err) => SubscriptionQuota::error(
            "codex_oauth",
            CredentialStatus::Expired,
            format!("Codex OAuth token unavailable: {err}"),
        ),
    }
}

async fn refresh_claude_quota(
    managers: &OauthQuotaManagers,
    account_id: &str,
) -> SubscriptionQuota {
    let manager = managers.claude.read().await;
    match manager.get_valid_token_for_account(account_id).await {
        Ok(token) => query_claude_quota_with_token(&token, "claude_oauth").await,
        Err(err) => SubscriptionQuota::error(
            "claude_oauth",
            CredentialStatus::Expired,
            format!("Claude OAuth token unavailable: {err}"),
        ),
    }
}

async fn refresh_copilot_quota(
    managers: &OauthQuotaManagers,
    account_id: &str,
) -> SubscriptionQuota {
    let manager = managers.copilot.read().await;
    match manager.fetch_usage_for_account(account_id).await {
        Ok(usage) => copilot_usage_to_subscription_quota(usage),
        Err(err) => SubscriptionQuota::error(
            "github_copilot",
            CredentialStatus::Expired,
            format!("Copilot usage unavailable: {err}"),
        ),
    }
}

async fn refresh_gemini_quota(
    managers: &OauthQuotaManagers,
    account_id: &str,
) -> SubscriptionQuota {
    let manager = managers.gemini.read().await;
    match manager.get_valid_token_for_account(account_id).await {
        Ok(token) => query_gemini_quota_with_token(&token, "google_gemini_oauth").await,
        Err(err) => SubscriptionQuota::error(
            "google_gemini_oauth",
            CredentialStatus::Expired,
            format!("Gemini OAuth token unavailable: {err}"),
        ),
    }
}

fn copilot_usage_to_subscription_quota(usage: CopilotUsageResponse) -> SubscriptionQuota {
    let premium = usage.quota_snapshots.premium_interactions;
    let utilization = if premium.entitlement > 0 {
        ((premium.entitlement - premium.remaining) as f64 / premium.entitlement as f64) * 100.0
    } else {
        0.0
    };
    SubscriptionQuota {
        tool: "github_copilot".to_string(),
        credential_status: CredentialStatus::Valid,
        credential_message: Some(usage.copilot_plan),
        success: true,
        tiers: vec![QuotaTier {
            name: "premium".to_string(),
            utilization,
            resets_at: Some(usage.quota_reset_date),
        }],
        extra_usage: None,
        error: None,
        queried_at: Some(now_millis()),
        failure: None,
    }
}

fn dedupe_targets(targets: Vec<OauthQuotaTarget>) -> Vec<OauthQuotaTarget> {
    let mut seen = HashSet::new();
    let mut deduped = Vec::new();
    for target in targets {
        let key = (target.auth_provider.clone(), target.account_id.clone());
        if seen.insert(key) {
            deduped.push(target);
        }
    }
    deduped
}

fn read_refresh_interval() -> Duration {
    let minutes = crate::settings::get_settings()
        .oauth_quota_refresh_interval_minutes
        .max(1);
    Duration::from_secs(minutes as u64 * 60)
}

fn now_millis() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
