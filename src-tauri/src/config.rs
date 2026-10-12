use std::{fs, path::PathBuf};

use anyhow::{Context, Result};
use tauri::Manager;

use crate::models::{AppConfig, CONFIG_FILE_NAME};

fn config_path(app: &tauri::AppHandle) -> Result<PathBuf> {
    // A test build can keep its settings apart from yours (VVD_DATA_DIR).
    if let Some(dir) = crate::db_location::test_data_dir() {
        fs::create_dir_all(&dir).context("failed to create the test data dir")?;
        return Ok(dir.join(CONFIG_FILE_NAME));
    }
    let dir = app
        .path()
        .app_config_dir()
        .context("failed to resolve app config dir")?;
    fs::create_dir_all(&dir).context("failed to create app config dir")?;
    Ok(dir.join(CONFIG_FILE_NAME))
}

pub(crate) fn load_config_impl(app: &tauri::AppHandle) -> AppConfig {
    let Ok(path) = config_path(app) else { return AppConfig::default() };
    // A broken or half-written file falls back to its last good copy, not
    // silently to the defaults (which would lose the VaM directory).
    crate::utils::read_file_with_backup(&path, |text| serde_json::from_str::<AppConfig>(text).ok()).unwrap_or_default()
}

pub(crate) fn save_config_impl(app: &tauri::AppHandle, config: &AppConfig) -> Result<()> {
    let path = config_path(app)?;
    crate::utils::write_file_atomic(&path, &serde_json::to_vec_pretty(config)?).context("failed to write config")
}
