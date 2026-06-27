use std::sync::Arc;

use tauri::Manager;

use crate::{store::AppState, AppError, Database};

pub(crate) async fn run_desktop_post_init(app_handle: tauri::AppHandle) {
    let state_handle = app_handle.clone();
    let state = state_handle.state::<AppState>();

    recover_proxy_takeover_if_needed(&state).await;
    initialize_common_config_snippets(&state);
    restore_proxy_state_on_startup(&state).await;
    restore_share_tunnels(&state).await;

    spawn_desktop_background_tasks(app_handle, state.db.clone());
}

pub(crate) async fn recover_proxy_takeover_if_needed(state: &AppState) {
    let has_backups = match state.db.has_any_live_backup().await {
        Ok(value) => value,
        Err(err) => {
            log::error!("检查 Live 备份失败: {err}");
            false
        }
    };
    let live_taken_over = state.proxy_service.detect_takeover_in_live_configs();

    if has_backups || live_taken_over {
        log::warn!("检测到接管残留，正在恢复 Live 配置");
        if let Err(err) = state.proxy_service.recover_from_crash().await {
            log::error!("恢复 Live 配置失败: {err}");
        } else {
            log::info!("Live 配置已恢复");
        }
    }
}

/// 启动时确保本地路由基础设施可用。
///
/// Claude / Codex / Gemini 的本地路由默认常开：启动代理服务，并确保
/// 三个应用的 Live 配置指向本地代理。分享功能只决定是否对外暴露 share，
/// 不再承担代理/接管开关语义。
pub(crate) async fn restore_proxy_state_on_startup(state: &AppState) {
    if let Ok(mut global_config) = state.db.get_global_proxy_config().await {
        if !global_config.proxy_enabled {
            global_config.proxy_enabled = true;
            if let Err(err) = state.db.update_global_proxy_config(global_config).await {
                log::warn!("修正本地路由全局开关失败: {err}");
            }
        }
    }

    let apps_to_restore = ["claude", "codex", "gemini"];
    for app_type in apps_to_restore {
        match state.db.get_proxy_config_for_app(app_type).await {
            Ok(mut config) => {
                if !config.enabled {
                    config.enabled = true;
                    if let Err(err) = state.db.update_proxy_config_for_app(config).await {
                        log::warn!("修正 {app_type} 本地路由状态失败: {err}");
                    }
                }
            }
            Err(err) => log::warn!("读取 {app_type} 本地路由配置失败: {err}"),
        }
    }

    log::info!("正在确保本地路由常开，应用列表: {apps_to_restore:?}");

    for app_type in apps_to_restore {
        let app_enum = match app_type {
            "claude" => crate::app_config::AppType::Claude,
            "codex" => crate::app_config::AppType::Codex,
            "gemini" => crate::app_config::AppType::Gemini,
            _ => continue,
        };
        let has_current = crate::settings::get_effective_current_provider(&state.db, &app_enum)
            .ok()
            .flatten()
            .and_then(|id| {
                state
                    .db
                    .get_provider_by_id(&id, app_type)
                    .ok()
                    .flatten()
                    .map(|_| ())
            })
            .is_some();
        if !has_current {
            log::info!("[{app_type}] 当前无可用 provider，跳过接管；待添加 provider 后自动接管");
            continue;
        }

        match state
            .proxy_service
            .set_takeover_for_app(app_type, true)
            .await
        {
            Ok(()) => {
                log::info!("✓ 已恢复 {app_type} 的代理接管状态");
            }
            Err(err) => {
                log::error!("✗ 恢复 {app_type} 的代理接管状态失败: {err}");
            }
        }
    }
}

pub(crate) fn initialize_common_config_snippets(state: &AppState) {
    for app_type in crate::app_config::AppType::all() {
        if !state
            .db
            .should_auto_extract_config_snippet(app_type.as_str())
            .unwrap_or(false)
        {
            continue;
        }

        let settings = match crate::services::provider::ProviderService::read_live_settings(
            app_type.clone(),
        ) {
            Ok(settings) => settings,
            Err(_) => continue,
        };

        match crate::services::provider::ProviderService::extract_common_config_snippet_from_settings(
            app_type.clone(),
            &settings,
        ) {
            Ok(snippet) if !snippet.is_empty() && snippet != "{}" => {
                match state.db.set_config_snippet(app_type.as_str(), Some(snippet)) {
                    Ok(()) => {
                        let _ = state.db.set_config_snippet_cleared(app_type.as_str(), false);
                        log::info!(
                            "✓ Auto-extracted common config snippet for {}",
                            app_type.as_str()
                        );
                    }
                    Err(err) => log::warn!(
                        "✗ Failed to save config snippet for {}: {err}",
                        app_type.as_str()
                    ),
                }
            }
            Ok(_) => log::debug!(
                "○ Live config for {} has no extractable common fields",
                app_type.as_str()
            ),
            Err(err) => log::warn!(
                "✗ Failed to extract config snippet for {}: {err}",
                app_type.as_str()
            ),
        }
    }

    let should_run_legacy_migration = state
        .db
        .is_legacy_common_config_migrated()
        .map(|done| !done)
        .unwrap_or(true);

    if should_run_legacy_migration {
        for app_type in [
            crate::app_config::AppType::Claude,
            crate::app_config::AppType::Codex,
            crate::app_config::AppType::Gemini,
        ] {
            if let Err(err) =
                crate::services::provider::ProviderService::migrate_legacy_common_config_usage_if_needed(
                    state,
                    app_type.clone(),
                )
            {
                log::warn!(
                    "✗ Failed to migrate legacy common-config usage for {}: {err}",
                    app_type.as_str()
                );
            }
        }

        if let Err(err) = state.db.set_legacy_common_config_migrated(true) {
            log::warn!("✗ Failed to persist legacy common-config migration flag: {err}");
        }
    }
}

pub(crate) async fn restore_share_tunnels(state: &AppState) {
    if let Err(err) = crate::commands::share::restore_active_share_tunnel(state).await {
        log::warn!("恢复 active share tunnel 失败: {err}");
    }
    if let Err(err) = crate::commands::share::restore_client_tunnel(state).await {
        log::warn!("恢复 client tunnel 失败: {err}");
    }
}

pub(crate) fn spawn_headless_background_tasks(state: Arc<AppState>) {
    let db = state.db.clone();
    spawn_router_sync_tasks(db.clone());
    crate::tunnel::model_health::spawn_share_model_health_scheduler_headless(db.clone());
    crate::services::webdav_auto_sync::start_worker_headless(db.clone());
    crate::services::s3_auto_sync::start_worker_headless(db.clone());

    spawn_headless_share_tunnel_restore_loop(state);
    spawn_periodic_backup_loop(db.clone());
    spawn_session_usage_sync_loop(db);
}

fn spawn_desktop_background_tasks(app_handle: tauri::AppHandle, db: Arc<Database>) {
    spawn_router_sync_tasks(db.clone());
    crate::tunnel::model_health::spawn_share_model_health_scheduler(db.clone(), app_handle.clone());

    if let Err(err) = db.periodic_backup_if_needed() {
        log::warn!("Periodic backup failed on startup: {err}");
    }

    spawn_desktop_share_tunnel_restore_loop(app_handle);
    spawn_periodic_backup_loop(db.clone());
    spawn_session_usage_sync_loop(db);
}

fn spawn_router_sync_tasks(db: Arc<Database>) {
    crate::tunnel::sync::reconcile_share_router_state(db.clone());
    crate::tunnel::sync::schedule_pull_pending_share_edits(db.clone());
    crate::tunnel::sync::spawn_share_edit_event_listener(db);
}

fn spawn_desktop_share_tunnel_restore_loop(app_handle: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        const SHARE_RESTORE_INTERVAL_SECS: u64 = 15;
        let mut interval =
            tokio::time::interval(std::time::Duration::from_secs(SHARE_RESTORE_INTERVAL_SECS));
        interval.tick().await;
        loop {
            interval.tick().await;
            let state = app_handle.state::<AppState>();
            restore_share_tunnels(&state).await;
        }
    });
}

fn spawn_headless_share_tunnel_restore_loop(state: Arc<AppState>) {
    tauri::async_runtime::spawn(async move {
        const SHARE_RESTORE_INTERVAL_SECS: u64 = 15;
        let mut interval =
            tokio::time::interval(std::time::Duration::from_secs(SHARE_RESTORE_INTERVAL_SECS));
        interval.tick().await;
        loop {
            interval.tick().await;
            restore_share_tunnels(state.as_ref()).await;
        }
    });
}

fn spawn_periodic_backup_loop(db: Arc<Database>) {
    tauri::async_runtime::spawn(async move {
        const PERIODIC_MAINTENANCE_INTERVAL_SECS: u64 = 24 * 60 * 60;
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(
            PERIODIC_MAINTENANCE_INTERVAL_SECS,
        ));
        interval.tick().await;
        loop {
            interval.tick().await;
            if let Err(err) = db.periodic_backup_if_needed() {
                log::warn!("Periodic maintenance timer failed: {err}");
            }
        }
    });
}

fn spawn_session_usage_sync_loop(db: Arc<Database>) {
    tauri::async_runtime::spawn(async move {
        const SESSION_SYNC_INTERVAL_SECS: u64 = 60;
        run_session_usage_sync(&db);
        let mut interval =
            tokio::time::interval(std::time::Duration::from_secs(SESSION_SYNC_INTERVAL_SECS));
        interval.tick().await;
        loop {
            interval.tick().await;
            run_session_usage_sync(&db);
        }
    });
}

fn run_session_usage_sync(db: &Database) {
    fn run_step<T>(name: &str, result: Result<T, AppError>) {
        if let Err(err) = result {
            log::warn!("{name} failed: {err}");
        }
    }

    run_step("Usage cost backfill", db.backfill_missing_usage_costs());
    run_step(
        "Session usage sync",
        crate::services::session_usage::sync_claude_session_logs(db),
    );
    run_step(
        "Codex usage sync",
        crate::services::session_usage_codex::sync_codex_usage(db),
    );
    run_step(
        "Gemini usage sync",
        crate::services::session_usage_gemini::sync_gemini_usage(db),
    );
    run_step(
        "OpenCode usage sync",
        crate::services::session_usage_opencode::sync_opencode_usage(db),
    );
}
