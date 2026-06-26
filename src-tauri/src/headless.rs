use std::sync::Arc;

use log::{Level, LevelFilter, Log, Metadata, Record};

use crate::{store::AppState, AppError, Database};

static HEADLESS_LOGGER: HeadlessLogger = HeadlessLogger;

struct HeadlessLogger;

impl Log for HeadlessLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= Level::Info
    }

    fn log(&self, record: &Record<'_>) {
        if self.enabled(record.metadata()) {
            eprintln!("[{}] {}", record.level(), record.args());
        }
    }

    fn flush(&self) {}
}

pub fn run() -> Result<(), AppError> {
    init_logging();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|err| AppError::Message(format!("create no-desktop runtime failed: {err}")))?;

    runtime.block_on(run_async())
}

fn init_logging() {
    if log::set_logger(&HEADLESS_LOGGER).is_ok() {
        log::set_max_level(LevelFilter::Info);
    }
}

async fn run_async() -> Result<(), AppError> {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let app_config_dir = crate::config::get_app_config_dir();
    crate::panic_hook::init_app_config_dir(app_config_dir.clone());
    std::fs::create_dir_all(&app_config_dir).map_err(|err| AppError::io(&app_config_dir, err))?;

    log::info!("cc-switch 正在以 no-desktop 模式启动");

    let db = initialize_database(&app_config_dir)?;
    let state = Arc::new(AppState::new(db));

    restore_tunnel_config(state.as_ref()).await;
    initialize_web_password(state.as_ref())?;
    initialize_global_http_client(state.as_ref());

    initialize_startup_data(state.as_ref());
    crate::initialize_common_config_snippets(state.as_ref());
    recover_proxy_takeover_if_needed(state.as_ref()).await;
    crate::restore_proxy_state_on_startup(state.as_ref()).await;
    ensure_proxy_running(state.as_ref()).await?;

    restore_share_tunnels(state.as_ref()).await;
    spawn_background_tasks(&state);

    log::info!("cc-switch no-desktop 启动完成");
    futures::future::pending::<()>().await;
    Ok(())
}

fn initialize_database(app_config_dir: &std::path::Path) -> Result<Arc<Database>, AppError> {
    let db_path = app_config_dir.join("cc-switch.db");
    let json_path = app_config_dir.join("config.json");

    if let Some(version) = Database::stored_user_version_exceeds_supported(&db_path)? {
        return Err(AppError::Message(format!(
            "数据库版本过新（{version}），当前应用仅支持 {}，请升级 cc-switch 后再启动 no-desktop",
            crate::database::SCHEMA_VERSION
        )));
    }

    let migration_config = if !db_path.exists() && json_path.exists() {
        log::info!("检测到旧版配置文件，准备迁移到 SQLite");
        Some(crate::app_config::MultiAppConfig::load()?)
    } else {
        None
    };

    let db = Arc::new(Database::init()?);
    if let Some(config) = migration_config {
        db.migrate_from_json(&config)?;
        let archive_path = json_path.with_extension("json.migrated");
        match std::fs::rename(&json_path, &archive_path) {
            Ok(()) => log::info!("旧版配置已归档为 {}", archive_path.display()),
            Err(err) => log::warn!("归档旧版配置失败: {err}"),
        }
    }

    Ok(db)
}

async fn restore_tunnel_config(state: &AppState) {
    let settings = crate::settings::get_settings();
    let cfg = if let Some(domain) = settings.current_share_router_domain() {
        crate::tunnel::config::TunnelConfig {
            domain: domain.to_string(),
        }
    } else {
        crate::tunnel::config::TunnelConfig::default_public_service()
    };

    state.tunnel_manager.write().await.set_config(cfg);
    log::info!("已恢复 cc-switch-router 隧道配置");
}

fn initialize_web_password(state: &AppState) -> Result<(), AppError> {
    if let Some(token) = crate::local_web_auth::ensure_startup_setup_token(&state.db)? {
        log::warn!("Web 管理密码尚未设置。首次设置需要 setup token: {token}");
        println!("cc-switch web setup token: {token}");
    }
    Ok(())
}

fn initialize_global_http_client(state: &AppState) {
    let proxy_url = state.db.get_global_proxy_url().ok().flatten();
    if let Err(err) = crate::proxy::http_client::init(proxy_url.as_deref()) {
        log::error!("[GlobalProxy] Failed to initialize with saved config: {err}");
        if proxy_url.is_some() {
            if let Err(clear_err) = state.db.set_global_proxy_url(None) {
                log::error!("[GlobalProxy] Failed to clear invalid config: {clear_err}");
            }
        }
        if let Err(fallback_err) = crate::proxy::http_client::init(None) {
            log::error!("[GlobalProxy] Failed to initialize direct connection: {fallback_err}");
        }
    }
}

fn initialize_startup_data(state: &AppState) {
    match state.db.init_default_skill_repos() {
        Ok(count) if count > 0 => {
            log::info!("Initialized {count} default skill repositories");
        }
        Ok(_) => {}
        Err(err) => log::warn!("Failed to initialize default skill repos: {err}"),
    }

    if let Err(err) = state.db.periodic_backup_if_needed() {
        log::warn!("Periodic backup failed on startup: {err}");
    }
}

async fn recover_proxy_takeover_if_needed(state: &AppState) {
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

async fn ensure_proxy_running(state: &AppState) -> Result<(), AppError> {
    if state.proxy_service.is_running().await {
        return Ok(());
    }

    let info = state
        .proxy_service
        .start()
        .await
        .map_err(|err| AppError::Message(format!("启动 Web/代理入口失败: {err}")))?;
    println!(
        "cc-switch no-desktop web/proxy listening on {}:{}",
        info.address, info.port
    );
    log::info!(
        "no-desktop 模式：Web/代理入口已启动于 {}:{}",
        info.address,
        info.port
    );
    Ok(())
}

async fn restore_share_tunnels(state: &AppState) {
    if let Err(err) = crate::commands::share::restore_active_share_tunnel(state).await {
        log::warn!("恢复 active share tunnel 失败: {err}");
    }
    if let Err(err) = crate::commands::share::restore_client_tunnel(state).await {
        log::warn!("恢复 client tunnel 失败: {err}");
    }
}

fn spawn_background_tasks(state: &Arc<AppState>) {
    crate::tunnel::sync::reconcile_share_router_state(state.db.clone());
    crate::tunnel::sync::schedule_pull_pending_share_edits(state.db.clone());
    crate::tunnel::sync::spawn_share_edit_event_listener(state.db.clone());

    spawn_share_tunnel_restore_loop(state);
    spawn_periodic_backup_loop(state);
    spawn_session_usage_sync_loop(state);
}

fn spawn_share_tunnel_restore_loop(state: &Arc<AppState>) {
    let state = Arc::clone(state);
    tokio::spawn(async move {
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

fn spawn_periodic_backup_loop(state: &Arc<AppState>) {
    let db = state.db.clone();
    tokio::spawn(async move {
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

fn spawn_session_usage_sync_loop(state: &Arc<AppState>) {
    let db = state.db.clone();
    tokio::spawn(async move {
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
