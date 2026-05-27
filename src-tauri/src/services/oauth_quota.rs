use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::sync::{broadcast, RwLock};

use crate::app_config::AppType;
use crate::commands::{
    AntigravityOAuthState, ClaudeOAuthState, CodexOAuthState, CopilotAuthState, CursorOAuthState,
    GeminiOAuthState, KiroOAuthState,
};
use crate::database::Database;
use crate::provider::Provider;
use crate::proxy::providers::antigravity_oauth_auth::AntigravityOAuthManager;
use crate::proxy::providers::claude_oauth_auth::ClaudeOAuthManager;
use crate::proxy::providers::codex_oauth_auth::CodexOAuthManager;
use crate::proxy::providers::copilot_auth::{CopilotAuthManager, CopilotUsageResponse};
use crate::proxy::providers::cursor_oauth_auth::CursorOAuthManager;
use crate::proxy::providers::gemini_oauth_auth::GeminiOAuthManager;
use crate::proxy::providers::kiro_oauth_auth::{KiroOAuthManager, KiroUsageLimitsResponse};
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
                .refresh_target(app, managers, target, source, None, false)
                .await
            {
                log::debug!("[OauthQuota] refresh selected target skipped/failed: {err}");
            }
        }
    }

    pub async fn refresh_all_targets(
        &self,
        app: Option<&AppHandle>,
        db: &Arc<Database>,
        managers: &OauthQuotaManagers,
        source: &str,
    ) {
        let targets = self.discover_all_oauth_targets(db, managers).await;
        for target in dedupe_targets(targets) {
            if let Err(err) = self
                .refresh_target(app, managers, target, source, None, false)
                .await
            {
                log::debug!("[OauthQuota] refresh all target skipped/failed: {err}");
            }
        }
    }

    pub async fn force_refresh(
        &self,
        app: Option<&AppHandle>,
        managers: &OauthQuotaManagers,
        auth_provider: &str,
        account_id: &str,
    ) -> Result<CachedOauthQuota, String> {
        let target = OauthQuotaTarget {
            app_type: String::new(),
            provider_id: String::new(),
            provider_name: String::new(),
            auth_provider: auth_provider.to_string(),
            account_id: account_id.to_string(),
        };
        self.refresh_target(app, managers, target, "manual", None, true)
            .await
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

    async fn discover_all_oauth_targets(
        &self,
        db: &Arc<Database>,
        managers: &OauthQuotaManagers,
    ) -> Vec<OauthQuotaTarget> {
        let mut targets = Vec::new();
        for app_type in [AppType::Claude, AppType::Codex, AppType::Gemini] {
            let providers = match db.get_all_providers(app_type.as_str()) {
                Ok(map) => map,
                Err(err) => {
                    log::debug!(
                        "[OauthQuota] failed to list providers for {}: {err}",
                        app_type.as_str()
                    );
                    continue;
                }
            };
            for (provider_id, provider) in &providers {
                if let Some(target) = self
                    .target_from_provider(&app_type, provider_id, provider, managers)
                    .await
                {
                    targets.push(target);
                }
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
        force: bool,
    ) -> Result<CachedOauthQuota, String> {
        let key = OauthQuotaKey {
            auth_provider: target.auth_provider.clone(),
            account_id: target.account_id.clone(),
        };
        if !force {
            if let Some(cached) = self
                .cache_hit_for_cooldown(&key, source, interval_secs_override)
                .await
            {
                return Ok(cached);
            }
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
            "kiro_oauth" => refresh_kiro_quota(managers, &target.account_id).await,
            "antigravity_oauth" => refresh_antigravity_quota(managers, &target.account_id).await,
            "cursor_oauth" => refresh_cursor_quota(managers, &target.account_id).await,
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
    pub kiro: Arc<RwLock<KiroOAuthManager>>,
    pub antigravity: Arc<RwLock<AntigravityOAuthManager>>,
    pub cursor: Arc<RwLock<CursorOAuthManager>>,
}

impl OauthQuotaManagers {
    pub fn from_states(
        codex: &CodexOAuthState,
        claude: &ClaudeOAuthState,
        gemini: &GeminiOAuthState,
        copilot: &CopilotAuthState,
        kiro: &KiroOAuthState,
        antigravity: &AntigravityOAuthState,
        cursor: &CursorOAuthState,
    ) -> Self {
        Self {
            codex: Arc::clone(&codex.0),
            claude: Arc::clone(&claude.0),
            gemini: Arc::clone(&gemini.0),
            copilot: Arc::clone(&copilot.0),
            kiro: Arc::clone(&kiro.0),
            antigravity: Arc::clone(&antigravity.0),
            cursor: Arc::clone(&cursor.0),
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
                .refresh_all_targets(Some(&app), &db, &managers, "background")
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
    if matches!(app_type, AppType::Claude) && provider_type == Some("kiro_oauth") {
        return Some("kiro_oauth".to_string());
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
    if matches!(app_type, AppType::Gemini)
        && (provider_type == Some("google_gemini_oauth")
            || provider.is_google_gemini_official_with_managed_auth())
    {
        return Some("google_gemini_oauth".to_string());
    }
    if matches!(app_type, AppType::Claude | AppType::Gemini)
        && provider_type == Some("antigravity_oauth")
    {
        return Some("antigravity_oauth".to_string());
    }
    if matches!(app_type, AppType::Claude | AppType::Codex) && provider_type == Some("cursor_oauth")
    {
        return Some("cursor_oauth".to_string());
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
        "kiro_oauth" => managers.kiro.read().await.default_account_id().await,
        "google_gemini_oauth" => managers.gemini.read().await.default_account_id().await,
        "antigravity_oauth" => managers.antigravity.read().await.default_account_id().await,
        "cursor_oauth" => managers.cursor.read().await.default_account_id().await,
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
        "kiro_oauth" => managers.kiro.read().await.default_account_id().await,
        "google_gemini_oauth" => managers.gemini.read().await.default_account_id().await,
        "antigravity_oauth" => managers.antigravity.read().await.default_account_id().await,
        "cursor_oauth" => managers.cursor.read().await.default_account_id().await,
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

async fn refresh_kiro_quota(managers: &OauthQuotaManagers, account_id: &str) -> SubscriptionQuota {
    let manager = managers.kiro.read().await;
    match manager.get_usage_limits_for_account(account_id).await {
        Ok(usage) => kiro_usage_to_subscription_quota(usage),
        Err(err) => SubscriptionQuota::error(
            "kiro_oauth",
            CredentialStatus::Expired,
            format!("Kiro OAuth usage limits unavailable: {err}"),
        ),
    }
}

async fn refresh_antigravity_quota(
    managers: &OauthQuotaManagers,
    account_id: &str,
) -> SubscriptionQuota {
    let manager = managers.antigravity.read().await;
    let token = match manager.get_valid_token_for_account(account_id).await {
        Ok(t) => t,
        Err(err) => {
            return SubscriptionQuota::error(
                "antigravity_oauth",
                CredentialStatus::Expired,
                format!("Antigravity OAuth token unavailable: {err}"),
            )
        }
    };
    let project_id = manager.project_id_for_account(account_id).await.ok();
    drop(manager);
    crate::services::subscription::query_antigravity_quota_with_token(
        &token,
        project_id.as_deref(),
        "antigravity_oauth",
    )
    .await
}

async fn refresh_cursor_quota(
    managers: &OauthQuotaManagers,
    account_id: &str,
) -> SubscriptionQuota {
    let manager = managers.cursor.read().await;
    let token = match manager.get_valid_token_for_account(account_id).await {
        Ok(t) => t,
        Err(err) => {
            return SubscriptionQuota::error(
                "cursor_oauth",
                CredentialStatus::Expired,
                format!("Cursor OAuth token unavailable: {err}"),
            )
        }
    };
    drop(manager);
    crate::services::subscription::query_cursor_quota(&token, account_id).await
}

fn kiro_usage_to_subscription_quota(usage: KiroUsageLimitsResponse) -> SubscriptionQuota {
    let current_usage = usage.current_usage();
    let usage_limit = usage.usage_limit();
    let utilization = if usage_limit > 0.0 {
        (current_usage / usage_limit) * 100.0
    } else {
        0.0
    };
    let resets_at = usage.next_reset_timestamp().and_then(timestamp_to_rfc3339);
    let credential_message = usage
        .subscription_title()
        .map(str::to_string)
        .or_else(|| Some("Kiro OAuth".to_string()));
    let extra_usage =
        usage
            .overage_enabled()
            .map(|enabled| crate::services::subscription::ExtraUsage {
                is_enabled: enabled,
                monthly_limit: None,
                used_credits: None,
                utilization: None,
                currency: None,
            });

    SubscriptionQuota {
        tool: "kiro_oauth".to_string(),
        credential_status: CredentialStatus::Valid,
        credential_message,
        success: true,
        tiers: vec![QuotaTier {
            name: "kiro_agentic_requests".to_string(),
            utilization,
            resets_at,
            used: Some(current_usage),
            limit: Some(usage_limit),
            unit: Some("credits".to_string()),
        }],
        extra_usage,
        error: None,
        queried_at: Some(now_millis()),
        failure: None,
    }
}

fn timestamp_to_rfc3339(value: f64) -> Option<String> {
    if !value.is_finite() || value <= 0.0 {
        return None;
    }
    let millis = if value > 1_000_000_000_000.0 {
        value.round() as i64
    } else {
        (value * 1000.0).round() as i64
    };
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(millis).map(|dt| dt.to_rfc3339())
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
            used: None,
            limit: None,
            unit: None,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::providers::kiro_oauth_auth::{
        KiroBonus, KiroFreeTrialInfo, KiroOverageConfiguration, KiroSubscriptionInfo,
        KiroUsageBreakdown, KiroUsageLimitsResponse,
    };

    #[test]
    fn kiro_usage_maps_to_agentic_requests_tier() {
        let quota = kiro_usage_to_subscription_quota(KiroUsageLimitsResponse {
            email: None,
            account_email: None,
            user_email: None,
            next_date_reset: Some(1_774_000_000.0),
            subscription_info: Some(KiroSubscriptionInfo {
                subscription_title: Some("KIRO PRO+".to_string()),
                email: None,
                account_email: None,
                user_email: None,
                overage_capability: Some("OVERAGE_CAPABLE".to_string()),
                extra: std::collections::HashMap::new(),
            }),
            usage_breakdown_list: vec![KiroUsageBreakdown {
                current_usage_with_precision: 40.0,
                bonuses: vec![
                    KiroBonus {
                        current_usage: 5.0,
                        usage_limit: 10.0,
                        status: Some("ACTIVE".to_string()),
                    },
                    KiroBonus {
                        current_usage: 100.0,
                        usage_limit: 100.0,
                        status: Some("EXPIRED".to_string()),
                    },
                ],
                free_trial_info: Some(KiroFreeTrialInfo {
                    current_usage_with_precision: 5.0,
                    free_trial_status: Some("ACTIVE".to_string()),
                    usage_limit_with_precision: 10.0,
                }),
                next_date_reset: None,
                usage_limit_with_precision: 80.0,
            }],
            overage_configuration: Some(KiroOverageConfiguration {
                overage_enabled: Some(true),
                overage_status: None,
            }),
            extra: std::collections::HashMap::new(),
        });

        assert!(quota.success);
        assert_eq!(quota.tool, "kiro_oauth");
        assert_eq!(quota.credential_message.as_deref(), Some("KIRO PRO+"));
        assert_eq!(quota.tiers.len(), 1);
        assert_eq!(quota.tiers[0].name, "kiro_agentic_requests");
        assert_eq!(quota.tiers[0].utilization, 50.0);
        assert_eq!(quota.tiers[0].used, Some(50.0));
        assert_eq!(quota.tiers[0].limit, Some(100.0));
        assert_eq!(quota.tiers[0].unit.as_deref(), Some("credits"));
        assert!(quota.tiers[0].resets_at.is_some());
        assert_eq!(
            quota.extra_usage.as_ref().map(|item| item.is_enabled),
            Some(true)
        );
    }

    #[test]
    fn timestamp_to_rfc3339_accepts_seconds_and_millis() {
        assert_eq!(
            timestamp_to_rfc3339(1_774_000_000.0),
            timestamp_to_rfc3339(1_774_000_000_000.0)
        );
        assert!(timestamp_to_rfc3339(0.0).is_none());
    }
}
