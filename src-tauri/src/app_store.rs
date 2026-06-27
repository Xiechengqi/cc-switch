use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};
use tauri_plugin_store::StoreExt;

use crate::error::AppError;

/// Store 中的键名
const STORE_KEY_APP_CONFIG_DIR: &str = "app_config_dir_override";
const STORE_FILE_APP_PATHS: &str = "app_paths.json";
const APP_IDENTIFIER: &str = "com.ccswitch.desktop";
const ENV_APP_CONFIG_DIR: &str = "CC_SWITCH_APP_CONFIG_DIR";

/// 缓存当前的 app_config_dir 覆盖路径，避免存储 AppHandle
static APP_CONFIG_DIR_OVERRIDE: OnceLock<RwLock<Option<PathBuf>>> = OnceLock::new();

fn override_cache() -> &'static RwLock<Option<PathBuf>> {
    APP_CONFIG_DIR_OVERRIDE.get_or_init(|| RwLock::new(None))
}

fn update_cached_override(value: Option<PathBuf>) {
    if let Ok(mut guard) = override_cache().write() {
        *guard = value;
    }
}

/// 获取缓存中的 app_config_dir 覆盖路径
pub fn get_app_config_dir_override() -> Option<PathBuf> {
    override_cache().read().ok()?.clone()
}

fn read_override_from_store(app: &tauri::AppHandle) -> Option<PathBuf> {
    let store = match app.store_builder(STORE_FILE_APP_PATHS).build() {
        Ok(store) => store,
        Err(e) => {
            log::warn!("无法创建 Store: {e}");
            return None;
        }
    };

    match store.get(STORE_KEY_APP_CONFIG_DIR) {
        Some(Value::String(path_str)) => {
            let path_str = path_str.trim();
            if path_str.is_empty() {
                return None;
            }

            let path = resolve_path(path_str);

            if !path.exists() {
                log::warn!(
                    "Store 中配置的 app_config_dir 不存在: {path:?}\n\
                     将使用默认路径。"
                );
                return None;
            }

            log::info!("使用 Store 中的 app_config_dir: {path:?}");
            Some(path)
        }
        Some(_) => {
            log::warn!("Store 中的 {STORE_KEY_APP_CONFIG_DIR} 类型不正确，应为字符串");
            None
        }
        None => None,
    }
}

/// 从 Store 刷新 app_config_dir 覆盖值并更新缓存
pub fn refresh_app_config_dir_override(app: &tauri::AppHandle) -> Option<PathBuf> {
    let value = read_override_from_store(app);
    update_cached_override(value.clone());
    value
}

/// no-desktop 没有 Tauri AppHandle，无法使用 tauri-plugin-store API。
/// 这里读取同一个 Store 文件，确保 desktop / no-desktop 使用相同 app_config_dir。
pub fn refresh_app_config_dir_override_for_headless() -> Option<PathBuf> {
    if let Some(value) = read_override_from_env() {
        update_cached_override(Some(value.clone()));
        return Some(value);
    }

    let value = read_override_from_store_file();
    update_cached_override(value.clone());
    value
}

/// 写入 app_config_dir 到 Tauri Store
pub fn set_app_config_dir_to_store(
    app: &tauri::AppHandle,
    path: Option<&str>,
) -> Result<(), AppError> {
    let store = app
        .store_builder(STORE_FILE_APP_PATHS)
        .build()
        .map_err(|e| AppError::Message(format!("创建 Store 失败: {e}")))?;

    match path {
        Some(p) => {
            let trimmed = p.trim();
            if !trimmed.is_empty() {
                store.set(STORE_KEY_APP_CONFIG_DIR, Value::String(trimmed.to_string()));
                log::info!("已将 app_config_dir 写入 Store: {trimmed}");
            } else {
                store.delete(STORE_KEY_APP_CONFIG_DIR);
                log::info!("已从 Store 中删除 app_config_dir 配置");
            }
        }
        None => {
            store.delete(STORE_KEY_APP_CONFIG_DIR);
            log::info!("已从 Store 中删除 app_config_dir 配置");
        }
    }

    store
        .save()
        .map_err(|e| AppError::Message(format!("保存 Store 失败: {e}")))?;

    refresh_app_config_dir_override(app);
    Ok(())
}

fn read_override_from_env() -> Option<PathBuf> {
    let raw = std::env::var(ENV_APP_CONFIG_DIR).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let path = resolve_path(trimmed);
    log::info!("使用 {ENV_APP_CONFIG_DIR} 指定的 app_config_dir: {path:?}");
    Some(path)
}

fn read_override_from_store_file() -> Option<PathBuf> {
    for path in tauri_store_candidate_paths() {
        let Some(value) = read_override_from_store_file_at(&path) else {
            continue;
        };
        return Some(value);
    }
    None
}

fn read_override_from_store_file_at(path: &Path) -> Option<PathBuf> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
        Err(err) => {
            log::warn!("读取 Store 文件失败 {}: {err}", path.display());
            return None;
        }
    };
    let json: Value = match serde_json::from_str(&text) {
        Ok(json) => json,
        Err(err) => {
            log::warn!("解析 Store 文件失败 {}: {err}", path.display());
            return None;
        }
    };
    let value = match json.get(STORE_KEY_APP_CONFIG_DIR) {
        Some(Value::String(value)) => value.trim(),
        Some(_) => {
            log::warn!(
                "Store 文件 {} 中的 {STORE_KEY_APP_CONFIG_DIR} 类型不正确，应为字符串",
                path.display()
            );
            return None;
        }
        None => return None,
    };
    if value.is_empty() {
        return None;
    }

    let resolved = resolve_path(value);
    if !resolved.exists() {
        log::warn!(
            "Store 文件 {} 中配置的 app_config_dir 不存在: {resolved:?}，将使用默认路径。",
            path.display()
        );
        return None;
    }

    log::info!(
        "no-desktop 使用 Store 中的 app_config_dir: {resolved:?} ({})",
        path.display()
    );
    Some(resolved)
}

fn tauri_store_candidate_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(data_dir) = dirs::data_dir() {
        paths.push(data_dir.join(APP_IDENTIFIER).join(STORE_FILE_APP_PATHS));
    }
    if let Some(data_local_dir) = dirs::data_local_dir() {
        paths.push(
            data_local_dir
                .join(APP_IDENTIFIER)
                .join(STORE_FILE_APP_PATHS),
        );
    }
    paths.dedup();
    paths
}

/// 解析路径，支持 ~ 开头的相对路径
fn resolve_path(raw: &str) -> PathBuf {
    if raw == "~" {
        if let Some(home) = dirs::home_dir() {
            return home;
        }
    } else if let Some(stripped) = raw.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(stripped);
        }
    } else if let Some(stripped) = raw.strip_prefix("~\\") {
        if let Some(home) = dirs::home_dir() {
            return home.join(stripped);
        }
    }

    PathBuf::from(raw)
}

/// 从旧的 settings.json 迁移 app_config_dir 到 Store
pub fn migrate_app_config_dir_from_settings(app: &tauri::AppHandle) -> Result<(), AppError> {
    // app_config_dir 已从 settings.json 移除，此函数保留但不再执行迁移
    // 如果用户在旧版本设置过 app_config_dir，需要在 Store 中手动配置
    log::info!("app_config_dir 迁移功能已移除，请在设置中重新配置");

    let _ = refresh_app_config_dir_override(app);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn reads_headless_override_from_store_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let config_dir = temp.path().join("config");
        fs::create_dir_all(&config_dir).expect("config dir");
        let store_path = temp.path().join(STORE_FILE_APP_PATHS);
        fs::write(
            &store_path,
            serde_json::json!({
                STORE_KEY_APP_CONFIG_DIR: config_dir.to_string_lossy()
            })
            .to_string(),
        )
        .expect("store file");

        let read = read_override_from_store_file_at(&store_path).expect("override");
        assert_eq!(read, config_dir);
    }

    #[test]
    fn headless_env_override_wins_without_existing_directory() {
        let _guard = env_lock().lock().expect("env lock");
        let old = std::env::var_os(ENV_APP_CONFIG_DIR);
        let temp = tempfile::tempdir().expect("tempdir");
        let config_dir = temp.path().join("new-config");
        std::env::set_var(ENV_APP_CONFIG_DIR, &config_dir);

        let read = read_override_from_env().expect("env override");
        assert_eq!(read, config_dir);

        match old {
            Some(value) => std::env::set_var(ENV_APP_CONFIG_DIR, value),
            None => std::env::remove_var(ENV_APP_CONFIG_DIR),
        }
    }
}
