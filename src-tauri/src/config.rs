use std::{fs, path::PathBuf};

use anyhow::{Context, Result};
use tauri::Manager;

use crate::models::{AppConfig, CONFIG_FILE_NAME};

fn config_path(app: &tauri::AppHandle) -> Result<PathBuf> {
    let dir = app
        .path()
        .app_config_dir()
        .context("failed to resolve app config dir")?;
    fs::create_dir_all(&dir).context("failed to create app config dir")?;
    Ok(dir.join(CONFIG_FILE_NAME))
}

pub(crate) fn load_config_impl(app: &tauri::AppHandle) -> AppConfig {
    let path = match config_path(app) {
        Ok(path) => path,
        Err(_) => return AppConfig::default(),
    };

    match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str::<AppConfig>(&text).unwrap_or_default(),
        Err(_) => AppConfig::default(),
    }
}

pub(crate) fn save_config_impl(app: &tauri::AppHandle, config: &AppConfig) -> Result<()> {
    let path = config_path(app)?;
    fs::write(path, serde_json::to_vec_pretty(config)?).context("failed to write config")
}
