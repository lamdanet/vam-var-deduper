//! VaM's own per-item hide / favorite flags, after VaM Backstage's
//! `vam-prefs.js`: an empty file
//! `<VaM>/AddonPackagesFilePrefs/<package stem>/<path in package>.hide` (or
//! `.fav`) hides (or stars) that item in VaM's own content browser. Nothing in
//! the package changes.
//!
//! Auto-hide dependency content hides everything in packages that are only
//! dependencies (`roles`), so VaM's browser shows what you chose. Unlike
//! Backstage, the hides it makes are recorded (`auto_hidden`): turning it off,
//! or a package becoming "installed", removes only those, never a hide you
//! made yourself — and hiding or showing an item by hand takes it over.
//!
//! When a package updates, its flags are copied to the new version (VaM keys
//! them by the versioned file name), for the items the new version still has.

use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

use rayon::prelude::*;
use serde::Serialize;
use tauri::State;

use crate::{db::Db, models::AppState, naming::{package_base, package_version}};

const PREFS_DIR: &str = "AddonPackagesFilePrefs";

fn prefs_root(vam_dir: &Path) -> PathBuf {
    vam_dir.join(PREFS_DIR)
}

fn flag_path(vam_dir: &Path, package_id: &str, internal: &str, kind: &str) -> PathBuf {
    let mut p = prefs_root(vam_dir).join(package_id);
    for part in internal.split('/').filter(|s| !s.is_empty()) {
        p.push(part);
    }
    let mut name = p.into_os_string();
    name.push(format!(".{kind}"));
    PathBuf::from(name)
}

fn check_kind(kind: &str) -> Result<&str, String> {
    match kind {
        "hide" | "fav" => Ok(kind),
        _ => Err(format!("unknown flag {kind}")),
    }
}

/// Every `.hide` / `.fav` under one package's prefs folder, as in-package paths.
fn read_flags(vam_dir: &Path, package_id: &str) -> (Vec<String>, Vec<String>) {
    let root = prefs_root(vam_dir).join(package_id);
    let (mut hidden, mut favorite) = (Vec::new(), Vec::new());
    for entry in walkdir::WalkDir::new(&root).into_iter().filter_map(Result::ok) {
        if !entry.file_type().is_file() {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(&root) else { continue };
        let rel = rel.to_string_lossy().replace('\\', "/");
        if let Some(p) = rel.strip_suffix(".hide") {
            hidden.push(p.to_string());
        } else if let Some(p) = rel.strip_suffix(".fav") {
            favorite.push(p.to_string());
        }
    }
    (hidden, favorite)
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PackagePrefs {
    pub(crate) hidden: Vec<String>,
    pub(crate) favorite: Vec<String>,
    /// The hidden ones auto-hide made (shown as such; switching it off undoes
    /// only these).
    pub(crate) auto: Vec<String>,
}

#[tauri::command]
pub(crate) fn vam_prefs_get(vam_dir: String, package_id: String, db: State<'_, Db>) -> PackagePrefs {
    let (hidden, favorite) = read_flags(Path::new(vam_dir.trim()), &package_id);
    let auto = crate::db::auto_hidden_for(&db, &package_id).unwrap_or_default();
    PackagePrefs { hidden, favorite, auto }
}

fn set_flag(vam_dir: &Path, package_id: &str, internal: &str, kind: &str, on: bool) -> Result<bool, String> {
    let path = flag_path(vam_dir, package_id, internal, kind);
    match (on, path.exists()) {
        (true, false) => {
            if let Some(dir) = path.parent() {
                fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            }
            fs::write(&path, b"").map(|()| true).map_err(|e| e.to_string())
        }
        (false, true) => fs::remove_file(&path).map(|()| true).map_err(|e| e.to_string()),
        _ => Ok(false),
    }
}

/// Hide / show (`kind` "hide") or favorite / unfavorite ("fav") items of one
/// package in VaM. A hide set or cleared here is the user's from now on.
#[tauri::command]
pub(crate) fn vam_prefs_set(
    vam_dir: String,
    package_id: String,
    paths: Vec<String>,
    kind: String,
    on: bool,
    db: State<'_, Db>,
) -> Result<usize, String> {
    let kind = check_kind(&kind)?;
    let vam_dir = PathBuf::from(vam_dir.trim());
    if !vam_dir.join("AddonPackages").is_dir() {
        return Err("Set your VaM folder in Settings first.".into());
    }
    let mut changed = 0;
    for p in &paths {
        if set_flag(&vam_dir, &package_id, p, kind, on)? {
            changed += 1;
        }
    }
    if kind == "hide" {
        let _ = crate::db::auto_hidden_forget(&db, &package_id, &paths);
    }
    Ok(changed)
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AutoHideResult {
    pub(crate) hidden: usize,
    pub(crate) unhidden: usize,
    pub(crate) packages: usize,
}

/// Make the auto-hides match the setting: with it on, every item of every
/// dependency package (not installed, not offloaded) is hidden; hides it made
/// for anything else are removed. Hides the user made are never touched.
#[tauri::command]
pub(crate) fn auto_hide_sync(
    vam_dir: String,
    enabled: bool,
    state: State<'_, AppState>,
    db: State<'_, Db>,
) -> Result<AutoHideResult, String> {
    let vam_dir = PathBuf::from(vam_dir.trim());
    if !vam_dir.join("AddonPackages").is_dir() {
        return Err("Set your VaM folder in Settings first.".into());
    }
    let deps: Vec<(String, String)> = if enabled {
        let cache = state.var_packages_folder_cache.lock().map_err(|_| "library cache poisoned")?;
        let items = &cache.as_ref().ok_or("Scan the library first.")?.items;
        items
            .iter()
            .filter(|it| !it.installed && !it.offloaded)
            .map(|it| (it.package_id.clone(), it.file_path.clone()))
            .collect()
    } else {
        Vec::new()
    };
    let desired: HashSet<(String, String)> = deps
        .par_iter()
        .flat_map_iter(|(id, path)| {
            crate::library::content_paths(Path::new(path)).into_iter().map(move |p| (id.clone(), p))
        })
        .collect();
    let existing: HashSet<(String, String)> = crate::db::auto_hidden_all(&db).unwrap_or_default().into_iter().collect();

    let mut result = AutoHideResult { packages: deps.len(), ..Default::default() };
    let mut added: Vec<(String, String)> = Vec::new();
    for (id, p) in desired.difference(&existing) {
        // Already hidden by hand: leave it the user's.
        if flag_path(&vam_dir, id, p, "hide").exists() {
            continue;
        }
        if set_flag(&vam_dir, id, p, "hide", true).unwrap_or(false) {
            added.push((id.clone(), p.clone()));
        }
    }
    let mut removed: Vec<(String, String)> = Vec::new();
    for (id, p) in existing.difference(&desired) {
        let _ = set_flag(&vam_dir, id, p, "hide", false);
        removed.push((id.clone(), p.clone()));
    }
    result.hidden = added.len();
    result.unhidden = removed.len();
    crate::db::auto_hidden_update(&db, &added, &removed).map_err(|e| e.to_string())?;
    Ok(result)
}

/// Copy flags to new versions: for each package family whose newest version
/// in the library has no prefs folder while an older version does, copy that
/// version's `.hide` / `.fav` for the items the new one still has. Returns how
/// many flags were copied.
#[tauri::command]
pub(crate) fn vam_prefs_carry_over(vam_dir: String, state: State<'_, AppState>, db: State<'_, Db>) -> Result<usize, String> {
    let vam_dir = PathBuf::from(vam_dir.trim());
    let root = prefs_root(&vam_dir);
    let Ok(dirs) = fs::read_dir(&root) else { return Ok(0) };
    // Stems with flags, newest version per family.
    let mut with_prefs: HashMap<String, Vec<(u64, String)>> = HashMap::new();
    for d in dirs.filter_map(Result::ok).filter(|d| d.path().is_dir()) {
        let stem = d.file_name().to_string_lossy().to_string();
        if let Some(v) = package_version(&stem) {
            with_prefs.entry(package_base(&stem).to_ascii_lowercase()).or_default().push((v, stem));
        }
    }
    let newest: Vec<(String, String, String)> = {
        let cache = state.var_packages_folder_cache.lock().map_err(|_| "library cache poisoned")?;
        let Some(c) = cache.as_ref() else { return Ok(0) };
        let mut best: HashMap<String, (u64, String, String)> = HashMap::new();
        for it in &c.items {
            let Some(v) = package_version(&it.package_id) else { continue };
            let fam = package_base(&it.package_id).to_ascii_lowercase();
            if best.get(&fam).is_none_or(|(bv, _, _)| v > *bv) {
                best.insert(fam, (v, it.package_id.clone(), it.file_path.clone()));
            }
        }
        best.into_iter()
            .filter(|(fam, _)| with_prefs.contains_key(fam))
            .map(|(fam, (_, id, path))| (fam, id, path))
            .collect()
    };
    let auto_rows: HashSet<(String, String)> = crate::db::auto_hidden_all(&db).unwrap_or_default().into_iter().collect();
    let mut copied = 0;
    let mut auto_added = Vec::new();
    for (fam, id, path) in newest {
        if root.join(&id).is_dir() {
            continue;
        }
        let Some(donor) = with_prefs
            .get(&fam)
            .and_then(|v| v.iter().filter(|(_, s)| *s != id).max_by_key(|(v, _)| *v))
            .map(|(_, s)| s.clone())
        else {
            continue;
        };
        let (hidden, favorite) = read_flags(&vam_dir, &donor);
        let present: HashSet<String> = crate::library::content_paths(Path::new(&path)).into_iter().collect();
        for (kind, list) in [("hide", hidden), ("fav", favorite)] {
            for p in list.into_iter().filter(|p| present.contains(p)) {
                if set_flag(&vam_dir, &id, &p, kind, true).unwrap_or(false) {
                    copied += 1;
                    if kind == "hide" && auto_rows.contains(&(donor.clone(), p.clone())) {
                        auto_added.push((id.clone(), p));
                    }
                }
            }
        }
    }
    let _ = crate::db::auto_hidden_update(&db, &auto_added, &[]);
    Ok(copied)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_use_vams_layout() {
        let dir = std::env::temp_dir().join(format!("vam_prefs_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let item = "Custom/Clothing/Female/A/Top/Top.vam";
        assert!(set_flag(&dir, "A.Pack.3", item, "hide", true).unwrap());
        let expected = dir.join("AddonPackagesFilePrefs/A.Pack.3/Custom/Clothing/Female/A/Top/Top.vam.hide");
        assert!(expected.is_file());
        assert_eq!(fs::metadata(&expected).unwrap().len(), 0);
        assert!(set_flag(&dir, "A.Pack.3", item, "fav", true).unwrap());
        let (hidden, favorite) = read_flags(&dir, "A.Pack.3");
        assert_eq!(hidden, [item]);
        assert_eq!(favorite, [item]);
        assert!(set_flag(&dir, "A.Pack.3", item, "hide", false).unwrap());
        assert!(!expected.exists());
        fs::remove_dir_all(&dir).ok();
    }
}
