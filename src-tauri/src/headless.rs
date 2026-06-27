use std::sync::{Arc, OnceLock};

use log::{Level, LevelFilter, Log, Metadata, Record};

use crate::{store::AppState, AppError, Database};

static HEADLESS_LOGGER: HeadlessLogger = HeadlessLogger;
static HEADLESS_APP_STATE: OnceLock<Arc<AppState>> = OnceLock::new();

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

    crate::app_store::refresh_app_config_dir_override_for_headless();
    let app_config_dir = crate::config::get_app_config_dir();
    crate::panic_hook::init_app_config_dir(app_config_dir.clone());
    std::fs::create_dir_all(&app_config_dir).map_err(|err| AppError::io(&app_config_dir, err))?;

    log::info!(
        "cc-switch 正在以 no-desktop 模式启动，配置目录: {}",
        app_config_dir.display()
    );

    let db = initialize_database(&app_config_dir)?;
    let state = Arc::new(AppState::new(db));
    let _ = HEADLESS_APP_STATE.set(state.clone());
    apply_log_config(state.as_ref());
    initialize_headless_auth_globals();

    restore_tunnel_config(state.as_ref()).await;
    initialize_web_password(state.as_ref())?;
    initialize_global_http_client(state.as_ref());
    log_web_dist_status();

    initialize_startup_data(state.as_ref()).await;
    crate::local_ext::startup::initialize_common_config_snippets(state.as_ref());
    crate::local_ext::startup::recover_proxy_takeover_if_needed(state.as_ref()).await;
    crate::local_ext::startup::restore_proxy_state_on_startup(state.as_ref()).await;
    ensure_proxy_running(state.as_ref()).await?;

    crate::local_ext::startup::restore_share_tunnels(state.as_ref()).await;
    crate::local_ext::startup::spawn_headless_background_tasks(state.clone());

    log::info!("cc-switch no-desktop 启动完成");
    futures::future::pending::<()>().await;
    Ok(())
}

pub(crate) fn app_state() -> Option<Arc<AppState>> {
    HEADLESS_APP_STATE.get().cloned()
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
    let cfg = crate::tunnel::config::TunnelConfig::from_settings_or_default();

    state.tunnel_manager.write().await.set_config(cfg);
    log::info!("已恢复 cc-switch-router 隧道配置");
}

fn log_web_dist_status() {
    if let Some(root) = crate::web::handlers::resolve_web_dist_root(None) {
        log::info!("Web 静态资源目录: {}", root.display());
        return;
    }

    let candidates = crate::web::handlers::web_dist_candidate_paths(None)
        .into_iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    log::warn!(
        "未找到 Web 静态资源目录。请将 dist 放到可执行文件同级，或设置 CC_SWITCH_WEB_DIST_DIR。候选路径: {candidates}"
    );
}

fn initialize_web_password(state: &AppState) -> Result<(), AppError> {
    if !crate::local_web_auth::is_password_configured(&state.db)? {
        log::warn!("Web 管理密码尚未设置。首次通过 Web 访问时请直接设置密码");
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

fn apply_log_config(state: &AppState) {
    match state.db.get_log_config() {
        Ok(config) => {
            log::set_max_level(config.to_level_filter());
            log::info!(
                "已加载日志配置: enabled={}, level={}",
                config.enabled,
                config.level
            );
        }
        Err(err) => log::warn!("读取日志配置失败，继续使用 no-desktop 默认日志级别: {err}"),
    }
}

fn initialize_headless_auth_globals() {
    use tokio::sync::RwLock;

    let app_config_dir = crate::config::get_app_config_dir();
    let codex_manager = Arc::new(RwLock::new(
        crate::proxy::providers::codex_oauth_auth::CodexOAuthManager::new(app_config_dir.clone()),
    ));
    crate::proxy::providers::codex_oauth_auth::set_global_codex_oauth_manager(codex_manager);

    let claude_manager = Arc::new(RwLock::new(
        crate::proxy::providers::claude_oauth_auth::ClaudeOAuthManager::new(app_config_dir.clone()),
    ));
    crate::proxy::providers::claude_oauth_auth::set_global_claude_oauth_manager(claude_manager);

    let gemini_manager = Arc::new(RwLock::new(
        crate::proxy::providers::gemini_oauth_auth::GeminiOAuthManager::new(app_config_dir.clone()),
    ));
    crate::proxy::providers::gemini_oauth_auth::set_global_gemini_oauth_manager(gemini_manager);

    let antigravity_manager = Arc::new(RwLock::new(
        crate::proxy::providers::antigravity_oauth_auth::AntigravityOAuthManager::new(
            app_config_dir,
        ),
    ));
    crate::proxy::providers::antigravity_oauth_auth::set_global_antigravity_oauth_manager(
        antigravity_manager,
    );

    let quota_service = Arc::new(crate::services::OauthQuotaService::new());
    crate::services::oauth_quota::set_global_oauth_quota_service(quota_service);
    log::info!("no-desktop auth globals initialized");
}

async fn initialize_startup_data(state: &AppState) {
    match state.db.init_default_skill_repos() {
        Ok(count) if count > 0 => {
            log::info!("Initialized {count} default skill repositories");
        }
        Ok(_) => {}
        Err(err) => log::warn!("Failed to initialize default skill repos: {err}"),
    }

    match state.db.get_setting("skills_ssot_migration_pending") {
        Ok(Some(flag)) if flag == "true" || flag == "1" => {
            let has_existing = state
                .db
                .get_all_installed_skills()
                .map(|skills| !skills.is_empty())
                .unwrap_or(false);
            if has_existing {
                log::info!(
                    "Detected skills_ssot_migration_pending but skills table not empty; skipping auto import."
                );
                let _ = state
                    .db
                    .set_setting("skills_ssot_migration_pending", "false");
            } else {
                match crate::services::skill::migrate_skills_to_ssot(&state.db) {
                    Ok(count) => {
                        log::info!("Auto imported {count} skill(s) into SSOT");
                        let _ = state
                            .db
                            .set_setting("skills_ssot_migration_pending", "false");
                    }
                    Err(err) => log::warn!("Failed to auto import legacy skills to SSOT: {err}"),
                }
            }
        }
        Ok(_) => {}
        Err(err) => log::warn!("Failed to read skills migration flag: {err}"),
    }

    for app_type in crate::app_config::AppType::all().filter(|t| !t.is_additive_mode()) {
        if !crate::services::provider::should_import_default_config_on_startup(state, &app_type)
            .unwrap_or(false)
        {
            continue;
        }
        match crate::services::provider::import_default_config(state, app_type.clone()) {
            Ok(true) => log::info!(
                "Imported live config for {} as default provider",
                app_type.as_str()
            ),
            Ok(false) => {}
            Err(err) => log::debug!("No live config to import for {}: {err}", app_type.as_str()),
        }
    }

    {
        let db = state.db.clone();
        tokio::task::spawn_blocking(move || {
            if let Err(err) =
                crate::codex_history_migration::maybe_migrate_codex_third_party_history_provider_bucket(
                    &db,
                )
            {
                log::warn!("Codex history provider bucket migration failed: {err}");
            }
            if let Err(err) =
                crate::codex_history_migration::maybe_migrate_codex_provider_template_bucket(&db)
            {
                log::warn!("Codex provider template bucket migration failed: {err}");
            }
            if let Err(err) =
                crate::codex_history_migration::maybe_migrate_codex_official_history_to_unified_bucket()
            {
                log::warn!("Codex official history unify migration failed: {err}");
            }
        });
    }

    match crate::services::provider::import_opencode_providers_from_live(state) {
        Ok(count) if count > 0 => {
            log::info!("Imported {count} OpenCode provider(s) from live config")
        }
        Ok(_) => {}
        Err(err) => log::warn!("Failed to import OpenCode providers: {err}"),
    }
    match crate::services::provider::import_openclaw_providers_from_live(state) {
        Ok(count) if count > 0 => {
            log::info!("Imported {count} OpenClaw provider(s) from live config")
        }
        Ok(_) => {}
        Err(err) => log::warn!("Failed to import OpenClaw providers: {err}"),
    }
    match crate::services::provider::import_hermes_providers_from_live(state) {
        Ok(count) if count > 0 => {
            log::info!("Imported {count} Hermes provider(s) from live config")
        }
        Ok(_) => {}
        Err(err) => log::warn!("Failed to import Hermes providers: {err}"),
    }

    let already_cleared_official = state
        .db
        .get_bool_flag("official_providers_seeded_v2_cleared")
        .unwrap_or(false);
    let was_seeded_official = state
        .db
        .get_bool_flag("official_providers_seeded")
        .unwrap_or(false);
    if was_seeded_official && !already_cleared_official {
        for (app_type, seed_id) in [
            ("claude", "claude-official"),
            ("codex", "codex-official"),
            ("gemini", "gemini-official"),
        ] {
            if let Err(err) = state.db.delete_provider(app_type, seed_id) {
                log::warn!("Failed to delete legacy seed {seed_id}: {err}");
            }
        }
        if let Err(err) = state
            .db
            .set_setting("official_providers_seeded_v2_cleared", "true")
        {
            log::warn!("Failed to set official cleanup flag: {err}");
        } else {
            log::info!("Cleared legacy official seed providers (claude/codex/gemini)");
        }
    }

    match state.db.ensure_openai_official_oauth_display_name() {
        Ok(count) if count > 0 => {
            log::info!("Renamed {count} OpenAI Official provider(s) to OAuth display name");
        }
        Ok(_) => {}
        Err(err) => log::warn!("Failed to rename OpenAI Official provider: {err}"),
    }
    match state.db.prune_legacy_provider_catalog() {
        Ok(count) if count > 0 => log::info!("Pruned {count} legacy provider(s)"),
        Ok(_) => {}
        Err(err) => log::warn!("Failed to prune legacy provider catalog: {err}"),
    }
    match state.db.ensure_codex_openai_official_default_model() {
        Ok(count) if count > 0 => {
            log::info!("Updated {count} Codex OpenAI Official (OAuth) provider(s) to gpt-5.5");
        }
        Ok(_) => {}
        Err(err) => log::warn!("Failed to update Codex OpenAI Official (OAuth) model: {err}"),
    }

    let proxy_defaults_already_applied = state
        .db
        .get_bool_flag("proxy_defaults_v2_applied")
        .unwrap_or(false);
    if !proxy_defaults_already_applied {
        let mut next = crate::settings::get_settings();
        next.enable_local_proxy = true;
        next.enable_failover_toggle = true;
        if let Err(err) = crate::settings::update_settings(next) {
            log::warn!("Failed to reset settings defaults: {err}");
        }
        match state.db.get_global_proxy_config().await {
            Ok(mut cfg) => {
                cfg.proxy_enabled = true;
                if let Err(err) = state.db.update_global_proxy_config(cfg).await {
                    log::warn!("Failed to reset global proxy_enabled: {err}");
                }
            }
            Err(err) => log::warn!("Failed to read global proxy config: {err}"),
        }
        if let Err(err) = state.db.set_setting("proxy_defaults_v2_applied", "true") {
            log::warn!("Failed to set proxy defaults flag: {err}");
        }
    }

    import_omo_configs(state);
    import_mcp_configs(state);
    import_prompt_configs(state);

    if let Err(err) = state.db.periodic_backup_if_needed() {
        log::warn!("Periodic backup failed on startup: {err}");
    }
}

fn import_omo_configs(state: &AppState) {
    let has_omo = state
        .db
        .get_all_providers("opencode")
        .map(|providers| {
            providers
                .values()
                .any(|p| p.category.as_deref() == Some("omo"))
        })
        .unwrap_or(false);
    if !has_omo {
        match crate::services::OmoService::import_from_local(state, &crate::services::omo::STANDARD)
        {
            Ok(provider) => {
                log::info!(
                    "Imported OMO config from local as provider '{}'",
                    provider.name
                );
            }
            Err(AppError::OmoConfigNotFound) => {}
            Err(err) => log::warn!("Failed to import OMO config from local: {err}"),
        }
    }

    let has_omo_slim = state
        .db
        .get_all_providers("opencode")
        .map(|providers| {
            providers
                .values()
                .any(|p| p.category.as_deref() == Some("omo-slim"))
        })
        .unwrap_or(false);
    if !has_omo_slim {
        match crate::services::OmoService::import_from_local(state, &crate::services::omo::SLIM) {
            Ok(provider) => {
                log::info!(
                    "Imported OMO Slim config from local as provider '{}'",
                    provider.name
                );
            }
            Err(AppError::OmoConfigNotFound) => {}
            Err(err) => log::warn!("Failed to import OMO Slim config from local: {err}"),
        }
    }
}

fn import_mcp_configs(state: &AppState) {
    if !state.db.is_mcp_table_empty().unwrap_or(false) {
        return;
    }
    log::info!("MCP table empty, importing from live configurations");
    for (label, result) in [
        (
            "Claude",
            crate::services::mcp::McpService::import_from_claude(state),
        ),
        (
            "Codex",
            crate::services::mcp::McpService::import_from_codex(state),
        ),
        (
            "Gemini",
            crate::services::mcp::McpService::import_from_gemini(state),
        ),
        (
            "OpenCode",
            crate::services::mcp::McpService::import_from_opencode(state),
        ),
        (
            "Hermes",
            crate::services::mcp::McpService::import_from_hermes(state),
        ),
    ] {
        match result {
            Ok(count) if count > 0 => log::info!("Imported {count} MCP server(s) from {label}"),
            Ok(_) => {}
            Err(err) => log::warn!("Failed to import {label} MCP: {err}"),
        }
    }
}

fn import_prompt_configs(state: &AppState) {
    if !state.db.is_prompts_table_empty().unwrap_or(false) {
        return;
    }
    log::info!("Prompts table empty, importing from live configurations");
    for app in [
        crate::app_config::AppType::Claude,
        crate::app_config::AppType::Codex,
        crate::app_config::AppType::Gemini,
        crate::app_config::AppType::OpenCode,
        crate::app_config::AppType::OpenClaw,
        crate::app_config::AppType::Hermes,
    ] {
        match crate::services::prompt::PromptService::import_from_file_on_first_launch(
            state,
            app.clone(),
        ) {
            Ok(count) if count > 0 => {
                log::info!("Imported {count} prompt(s) for {}", app.as_str());
            }
            Ok(_) => {}
            Err(err) => log::warn!("Failed to import prompt for {}: {err}", app.as_str()),
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
