//! Adding packages by dropping them on the window, and finding the VaM folder
//! on first run. After VaM Backstage's `downloads/import.js` (drop import) and
//! `ipc/scanner.js` `detectVamDir`.
//!
//! Dropped `.var` files (and `.var.zip`, a `.var` some sites rename), or the
//! `.var` files inside dropped folders, are checked — a real
//! `Creator.Name.Version` name and a zip whose every entry passes its
//! checksum — then copied (or moved) into AddonPackages, in creator folders
//! when that setting is on. A package already in the library is skipped.
//! Imported packages count as installed.

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use serde::Serialize;
use tauri::State;

use crate::{db::Db, library::VamDirInfo, models::AppState, naming::package_version};

/// Ancestors of the working folder and of the app's own folder (5 levels),
/// where VaM folders are usually found for a portable tool. The first with an
/// AddonPackages folder wins.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DetectedVamDir {
    pub(crate) info: VamDirInfo,
    pub(crate) source: String,
}

#[tauri::command]
pub(crate) fn detect_vam_dir() -> Option<DetectedVamDir> {
    let mut roots: Vec<(PathBuf, &str)> = Vec::new();
    if let Ok(dir) = std::env::current_dir() {
        roots.push((dir, "the folder the app was started from"));
    }
    if let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)) {
        roots.push((dir, "the app's folder"));
    }
    let mut seen = HashSet::new();
    for (root, source) in roots {
        for dir in root.ancestors().take(5) {
            if !seen.insert(dir.to_path_buf()) {
                continue;
            }
            if dir.join("AddonPackages").is_dir() {
                let info = crate::library::inspect_vam_dir(dir.display().to_string());
                if info.valid {
                    return Some(DetectedVamDir { info, source: source.to_string() });
                }
            }
        }
    }
    None
}

/// `Creator.Name.Version.var` (or `.var.zip`) → the `.var` file name.
fn var_name(path: &Path) -> Option<String> {
    let file = path.file_name()?.to_str()?;
    let lc = file.to_ascii_lowercase();
    let name = if lc.ends_with(".var.zip") {
        &file[..file.len() - 4]
    } else if lc.ends_with(".var") {
        file
    } else {
        return None;
    };
    Some(name.to_string())
}

fn valid_package_name(var_file: &str) -> bool {
    let stem = &var_file[..var_file.len() - 4];
    stem.split('.').count() >= 3 && package_version(stem).is_some()
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ImportItem {
    pub(crate) source: String,
    pub(crate) name: String,
    /// Where it went (or would go).
    pub(crate) dest: String,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ImportResult {
    /// Imported — or, on a dry run, about to be.
    pub(crate) imported: Vec<ImportItem>,
    /// Already in the library: skipped.
    pub(crate) existing: Vec<String>,
    /// Not packages, or damaged: (file, why).
    pub(crate) invalid: Vec<(String, String)>,
    /// Dropped files that aren't .var at all.
    pub(crate) other_files: usize,
    pub(crate) moved: bool,
}

/// Every .var / .var.zip among the dropped paths (folders walked).
fn collect(paths: &[String]) -> (Vec<PathBuf>, usize) {
    let mut files = Vec::new();
    let mut other = 0;
    for p in paths {
        let path = PathBuf::from(p);
        if path.is_dir() {
            for entry in walkdir::WalkDir::new(&path).into_iter().filter_map(Result::ok) {
                if entry.file_type().is_file() {
                    if var_name(entry.path()).is_some() {
                        files.push(entry.into_path());
                    } else {
                        other += 1;
                    }
                }
            }
        } else if var_name(&path).is_some() {
            files.push(path);
        } else {
            other += 1;
        }
    }
    (files, other)
}

fn place(source: &Path, dest: &Path, move_file: bool) -> Result<(), String> {
    if let Some(dir) = dest.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("can't create {}: {e}", dir.display()))?;
    }
    if move_file && fs::rename(source, dest).is_ok() {
        return Ok(());
    }
    // Copy through a temporary name so a half-copied file never looks like a
    // package; a move across drives is a copy plus deleting the original.
    let tmp = dest.with_extension("var.import-tmp");
    fs::copy(source, &tmp).map_err(|e| format!("copy failed: {e}"))?;
    fs::rename(&tmp, dest).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("couldn't put it in place: {e}")
    })?;
    if move_file {
        let _ = fs::remove_file(source);
    }
    Ok(())
}

/// Check, then (unless `dry_run`) copy or move the dropped packages into
/// AddonPackages.
#[tauri::command(async)]
pub(crate) fn import_var_files(
    paths: Vec<String>,
    addon_dir: String,
    by_creator: bool,
    move_files: bool,
    dry_run: bool,
    state: State<'_, AppState>,
    db: State<'_, Db>,
) -> Result<ImportResult, String> {
    let addon = PathBuf::from(addon_dir.trim());
    if !addon.is_dir() {
        return Err("Set your VaM folder in Settings first.".into());
    }
    let library: HashSet<String> = state
        .var_packages_folder_cache
        .lock()
        .ok()
        .and_then(|c| c.as_ref().map(|c| c.items.iter().map(|i| i.file_name.to_ascii_lowercase()).collect()))
        .unwrap_or_default();
    let (files, other_files) = collect(&paths);
    let mut result = ImportResult { other_files, moved: move_files, ..Default::default() };
    let mut taken: HashSet<String> = HashSet::new();
    for source in files {
        let Some(name) = var_name(&source) else { continue };
        let shown = source.display().to_string();
        if !valid_package_name(&name) {
            result.invalid.push((shown, "not a package name (Creator.Name.Version.var)".into()));
            continue;
        }
        let lc = name.to_ascii_lowercase();
        let creator = name.split('.').next().unwrap_or_default();
        let dir = match (by_creator, crate::naming::sanitize_creator_folder(creator)) {
            (true, Some(folder)) => addon.join(folder),
            _ => addon.clone(),
        };
        let dest = dir.join(&name);
        if library.contains(&lc) || dest.exists() || !taken.insert(lc) {
            result.existing.push(name);
            continue;
        }
        if source.starts_with(&addon) && source.file_name().and_then(|n| n.to_str()) == Some(name.as_str()) {
            // Already inside AddonPackages: nothing to do.
            result.existing.push(name);
            continue;
        }
        if !crate::hub::is_valid_var(&source) {
            result.invalid.push((shown, "not a valid .var".into()));
            continue;
        }
        if !dry_run {
            if let Err(e) = crate::integrity::verify_var(&source) {
                result.invalid.push((shown, format!("damaged ({e})")));
                continue;
            }
            if let Err(e) = place(&source, &dest, move_files) {
                result.invalid.push((shown, e));
                continue;
            }
        }
        result.imported.push(ImportItem { source: shown, name, dest: dest.display().to_string() });
    }
    if !dry_run && !result.imported.is_empty() {
        let families: Vec<String> = result
            .imported
            .iter()
            .map(|i| crate::naming::package_base(i.name.trim_end_matches(".var")).to_ascii_lowercase())
            .collect();
        let _ = crate::db::roles_set(&db, &families, true);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_zip_renames() {
        assert_eq!(var_name(Path::new("C:/x/A.Pkg.3.var")).as_deref(), Some("A.Pkg.3.var"));
        assert_eq!(var_name(Path::new("C:/x/A.Pkg.3.var.zip")).as_deref(), Some("A.Pkg.3.var"));
        assert_eq!(var_name(Path::new("C:/x/readme.txt")), None);
        assert!(valid_package_name("A.Pkg.3.var"));
        assert!(!valid_package_name("Pkg.3.var"), "needs a creator");
        assert!(!valid_package_name("A.Pkg.latest.var"));
    }
}
