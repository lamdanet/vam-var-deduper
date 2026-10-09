//! Clean VARs — make a package smaller by pointing at copies elsewhere.
//!
//! The inverse of Internalize Resources: a file the package carries that
//! another package also has (same contents) can go, its references pointing
//! at that package instead, which becomes a dependency. This lists, for one
//! package, each file it uses itself that has an exact copy elsewhere — in
//! the folders, and, when asked, in packages only the database knows (not
//! installed: the package needs them downloaded) — and applies the choices
//! to the package itself, with a backup. The duplicate groups and the
//! rewrite come from the existing dedupe engine (`scan`, `execute`).

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{atomic::Ordering, Arc, Mutex},
    thread,
};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use tauri::State;

use crate::{
    db::{self, Db},
    dep_usage::{read_refs_many, RefCache},
    execute::{prepare_package_changes, rewrite_package, sum_removed_bytes},
    fix_var::free_backup_path,
    models::{AppState, ProgressPayload, ScannedData, TaskHandle, KEEP_ALL_VALUE, META_PATH},
    naming,
    scan::{compute_missing_siblings, load_cached_scan_with_target},
    tasks::{list_target_var_text_refs, new_progress_payload, parse_additional_dirs, set_task_progress},
};

/// A package that has an exact copy of a file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct CleanSource {
    pub(crate) package_id: String,
    /// The .var on disk; empty for a package only the database knows.
    pub(crate) file_path: String,
    pub(crate) internal_path: String,
    /// In the folders; false: only in the database (needs downloading).
    pub(crate) installed: bool,
    /// The package already lists it as a dependency: no new one.
    pub(crate) already_dependency: bool,
    /// What its copy of a bundle (.vam with .vaj/.vab, .vmi with .vmb) lacks:
    /// unsafe to point at, as on the old Clean VARs page.
    #[serde(default)]
    pub(crate) incomplete: Vec<String>,
}

/// A file the package uses itself that has an exact copy elsewhere.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct CleanItem {
    /// The key the apply step takes: the duplicate group, or a database-only
    /// one (`dbfind|<package>|<path>`).
    pub(crate) key: String,
    pub(crate) path: String,
    /// With what goes along with it (a .vam's .vaj, .vab, textures).
    pub(crate) size: u64,
    pub(crate) bundle: Vec<String>,
    /// Installed packages first.
    pub(crate) copies: Vec<CleanSource>,
    /// Other packages that reference it (or what goes with it) in this
    /// package by path: removing it would break them, so it stays.
    #[serde(default)]
    pub(crate) used_by: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct CleanReport {
    pub(crate) target_package_id: String,
    pub(crate) items: Vec<CleanItem>,
    /// Files with a copy elsewhere that the package doesn't reference itself
    /// (offered as content, not used): left alone.
    pub(crate) unreferenced_files: u32,
    pub(crate) unreferenced_bytes: u64,
    /// The packages read to see who uses its files.
    pub(crate) users_read: u32,
}

/// The same file, however its path is spelled (a package outside the
/// folders is kept under its canonical form, with a long-path prefix). The package to rewrite
/// must be exactly this one, never another copy with the same id.
fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy()),
    }
}

/// A scene is the package itself, not a resource to point elsewhere (the old
/// page's rule: `Saves/scene/` at any depth).
fn is_scene(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.starts_with("saves/scene/") || lower.contains("/saves/scene/")
}

/// The target's files other packages in the folders reference by path (any
/// version of it, `.latest` too): lowercase path -> those packages. Its users are the
/// packages listing it as a dependency; a reference only works while the
/// file is there, so these can't go.
pub(crate) fn used_by_others(
    scanned: &ScannedData,
    target_id: &str,
    cache: &Mutex<RefCache>,
) -> (BTreeMap<String, Vec<String>>, u32) {
    let family = naming::package_base(target_id).to_ascii_lowercase();
    let users: Vec<(&String, PathBuf)> = scanned
        .packages
        .values()
        .filter(|p| p.package_id != target_id)
        .filter(|p| {
            p.dependencies
                .as_object()
                .is_some_and(|m| m.keys().any(|k| naming::package_base(k).eq_ignore_ascii_case(&family)))
        })
        .map(|p| (&p.package_id, p.file_path.clone()))
        .collect();
    let paths: Vec<PathBuf> = users.iter().map(|(_, f)| f.clone()).collect();
    let refs = read_refs_many(&paths, cache, |_, _| {});
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for ((id, _), r) in users.iter().zip(refs) {
        for path in r.get(&family).into_iter().flatten() {
            out.entry(path.to_ascii_lowercase()).or_default().push((*id).clone());
        }
    }
    (out, users.len() as u32)
}

/// What the target uses itself that has exact copies elsewhere.
pub(crate) fn clean_candidates(
    target_path: &Path,
    scanned: &ScannedData,
    db: Option<&Db>,
    cache: &Mutex<RefCache>,
) -> Result<CleanReport> {
    let target_str = target_path.to_string_lossy().to_string();
    let target = scanned
        .packages
        .values()
        .find(|p| same_file(&p.file_path, target_path))
        .ok_or_else(|| anyhow!("the package isn't in the scan: check it again"))?;
    let target_id = target.package_id.clone();

    // What it references itself; a bundle's members come with their parent.
    let used: HashSet<String> = list_target_var_text_refs(target_str.clone())
        .map_err(|e| anyhow!(e))?
        .into_iter()
        .collect();
    let members: HashSet<&String> = target.support_paths.values().flatten().collect();
    let deps: HashSet<String> = target
        .dependencies
        .as_object()
        .map(|m| m.keys().map(|k| naming::package_base(k).to_ascii_lowercase()).collect())
        .unwrap_or_default();
    let is_dep = |pkg: &str| deps.contains(&naming::package_base(pkg).to_ascii_lowercase());
    // The members actually in the package (the map also lists the ones a
    // bundle may have).
    let inside: HashSet<&str> = target.resource_refs.iter().map(|r| r.internal_path.as_str()).collect();
    let bundle_of = |path: &str| -> Vec<String> {
        target
            .support_paths
            .get(path)
            .map(|s| s.iter().filter(|m| inside.contains(m.as_str())).cloned().collect())
            .unwrap_or_default()
    };

    let mut report = CleanReport {
        target_package_id: target_id.clone(),
        ..CleanReport::default()
    };
    let mut by_path: HashMap<String, usize> = HashMap::new();
    // Each package's bundles that lack a member, worked out once per package.
    let mut missing: HashMap<&str, BTreeMap<String, BTreeSet<String>>> = HashMap::new();
    for group in &scanned.duplicate_groups {
        let Some(own) = group.refs.iter().find(|r| r.package_id == target_id) else {
            continue;
        };
        let path = own.internal_path.clone();
        if path == META_PATH || is_scene(&path) || members.contains(&path) {
            continue;
        }
        if !used.contains(&path) {
            report.unreferenced_files += 1;
            report.unreferenced_bytes += own.effective_size.max(own.size);
            continue;
        }
        let mut seen: HashSet<&str> = HashSet::new();
        let mut copies: Vec<CleanSource> = Vec::new();
        for r in &group.refs {
            if r.package_id == target_id || !seen.insert(r.package_id.as_str()) {
                continue;
            }
            let lacks = missing
                .entry(r.package_id.as_str())
                .or_insert_with(|| {
                    scanned
                        .packages
                        .get(&r.package_id)
                        .map(compute_missing_siblings)
                        .unwrap_or_default()
                })
                .get(&r.internal_path)
                .map(|m| m.iter().cloned().collect())
                .unwrap_or_default();
            copies.push(CleanSource {
                package_id: r.package_id.clone(),
                file_path: r.package_file.clone(),
                internal_path: r.internal_path.clone(),
                installed: true,
                already_dependency: is_dep(&r.package_id),
                incomplete: lacks,
            });
        }
        if copies.is_empty() {
            continue;
        }
        by_path.insert(path.clone(), report.items.len());
        report.items.push(CleanItem {
            key: group.key.clone(),
            size: own.effective_size.max(own.size),
            bundle: bundle_of(&path),
            path,
            copies,
            used_by: Vec::new(),
        });
    }

    // Packages only the database knows, by the files' contents (the crc32
    // index): not installed, so choosing one means downloading it.
    if let Some(db) = db {
        let wanted: Vec<&crate::models::ResourceRef> = target
            .resource_refs
            .iter()
            .filter(|r| {
                r.crc32.is_some()
                    && r.size > 0
                    && r.internal_path != META_PATH
                    && !is_scene(&r.internal_path)
                    && !members.contains(&r.internal_path)
                    && used.contains(&r.internal_path)
            })
            .collect();
        let crcs: Vec<u32> = wanted.iter().filter_map(|r| r.crc32).collect::<BTreeSet<_>>().into_iter().collect();
        let exclude: HashSet<String> = std::iter::once(target_id.clone()).collect();
        let conn = db.read()?;
        let found = db::find_crc_matches_bulk(&conn, &crcs, &exclude, |_, _| {})?;
        drop(conn);
        for r in wanted {
            let Some(hits) = r.crc32.and_then(|crc| found.get(&crc)) else {
                continue;
            };
            let mut seen: HashSet<&str> = HashSet::new();
            let extra: Vec<CleanSource> = hits
                .iter()
                .filter(|h| h.size as u64 == r.size && !scanned.packages.contains_key(&h.package_id))
                .filter(|h| seen.insert(h.package_id.as_str()))
                .map(|h| CleanSource {
                    package_id: h.package_id.clone(),
                    file_path: String::new(),
                    internal_path: h.internal_path.clone(),
                    installed: false,
                    already_dependency: is_dep(&h.package_id),
                    incomplete: Vec::new(),
                })
                .collect();
            if extra.is_empty() {
                continue;
            }
            match by_path.get(&r.internal_path) {
                Some(&i) => report.items[i].copies.extend(extra),
                None => {
                    by_path.insert(r.internal_path.clone(), report.items.len());
                    report.items.push(CleanItem {
                        key: format!("dbfind|{}|{}", target_id, r.internal_path),
                        path: r.internal_path.clone(),
                        size: r.effective_size.max(r.size),
                        bundle: bundle_of(&r.internal_path),
                        copies: extra,
                        used_by: Vec::new(),
                    });
                }
            }
        }
    }

    // Who else uses each file (with what goes with it) from this package.
    let (used, read) = used_by_others(scanned, &target_id, cache);
    report.users_read = read;
    for item in &mut report.items {
        let mut by: BTreeSet<String> = BTreeSet::new();
        for path in std::iter::once(&item.path).chain(item.bundle.iter()) {
            by.extend(used.get(&path.to_ascii_lowercase()).into_iter().flatten().cloned());
        }
        item.used_by = by.into_iter().collect();
    }

    report.items.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.path.cmp(&b.path)));
    Ok(report)
}

/// Lists what the package could point at elsewhere. Needs the folder scan
/// (`start_scan_task`, as Fix Missing runs it) cached; `include_db` adds
/// packages only the database knows.
#[tauri::command(async)]
pub(crate) fn clean_var_candidates(
    input_dir: String,
    additional_input_dirs: Option<Vec<String>>,
    target_var_path: String,
    include_db: bool,
    state: State<'_, AppState>,
    db: State<'_, Db>,
) -> Result<CleanReport, String> {
    let target_path = Path::new(&target_var_path);
    if !target_path.is_file() {
        return Err(format!("package not found on disk: {}", target_path.display()));
    }
    let additional = parse_additional_dirs(&additional_input_dirs.unwrap_or_default());
    let scanned = load_cached_scan_with_target(&state.scan_cache, Path::new(&input_dir), &additional, Some(target_path))
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "No folder scan yet: check the package again.".to_string())?;
    clean_candidates(target_path, &scanned, include_db.then_some(&*db), &state.dep_refs_cache).map_err(|e| e.to_string())
}

/// What Clean did to the package.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct CleanResult {
    pub(crate) removed_files: u32,
    pub(crate) removed_bytes: u64,
    pub(crate) dependencies: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) backup_path: Option<String>,
}

/// Applies `keep_map` (item key → `package:path` of the copy to point at)
/// to the package itself, backing it up first when asked.
pub(crate) fn apply_clean(
    target_path: &Path,
    mut scanned: ScannedData,
    keep_map: &BTreeMap<String, String>,
    backup_dir: Option<&Path>,
    db: Option<&Db>,
    cache: &Mutex<RefCache>,
) -> Result<CleanResult> {
    let target_id = scanned
        .packages
        .values()
        .find(|p| same_file(&p.file_path, target_path))
        .map(|p| p.package_id.clone())
        .ok_or_else(|| anyhow!("the package isn't in the scan: check it again"))?;
    // Only what was chosen changes: every other duplicate group stays as it
    // is (the engine would otherwise fall back to a group's first copy,
    // whichever package that is).
    // Never point at a bundle that lacks a member (the page doesn't offer one).
    for value in keep_map.values() {
        let Some((pkg, path)) = value.split_once(':') else {
            continue;
        };
        if let Some(lacks) = scanned.packages.get(pkg).and_then(|p| compute_missing_siblings(p).remove(path)) {
            let list: Vec<String> = lacks.into_iter().collect();
            bail!("{pkg} lacks part of {path} ({}): choose another copy", list.join(", "));
        }
    }
    let mut keep_map = keep_map.clone();
    for group in &scanned.duplicate_groups {
        keep_map.entry(group.key.clone()).or_insert_with(|| KEEP_ALL_VALUE.to_string());
    }
    prepare_package_changes(&mut scanned, &keep_map, Some(&target_id), db)?;
    let package = scanned
        .packages
        .get(&target_id)
        .ok_or_else(|| anyhow!("the package isn't in the scan"))?;
    if package.removed_paths.is_empty() && package.required_dependencies.is_empty() {
        bail!("nothing to change");
    }
    // Never remove a file another package references here (the page doesn't
    // offer one): it would break that package.
    let (used, _) = used_by_others(&scanned, &target_id, cache);
    if let Some((path, users)) = package.removed_paths.iter().find_map(|p| used.get(&p.to_ascii_lowercase()).map(|u| (p, u))) {
        bail!("{} uses {path} from this package: it can't go", users.join(", "));
    }
    let mut result = CleanResult {
        removed_files: package.removed_paths.len() as u32,
        removed_bytes: sum_removed_bytes(package),
        dependencies: package.required_dependencies.iter().cloned().collect(),
        backup_path: None,
    };
    if let Some(dir) = backup_dir {
        fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
        let name = target_path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| anyhow!("invalid file name"))?;
        let backup = free_backup_path(dir, name);
        fs::copy(target_path, &backup).with_context(|| format!("failed to back up to {}", backup.display()))?;
        result.backup_path = Some(backup.to_string_lossy().to_string());
    }
    rewrite_package(&scanned, package, &package.file_path)?;
    Ok(result)
}

/// Runs Clean on a worker thread; the UI polls `get_task_progress` and reads
/// `clean_result`.
#[allow(clippy::too_many_arguments)] // a Tauri command: one argument per field the UI sends
#[tauri::command]
pub(crate) fn start_clean_var_task(
    input_dir: String,
    additional_input_dirs: Option<Vec<String>>,
    target_var_path: String,
    keep_map: BTreeMap<String, String>,
    backup: bool,
    backup_dir: Option<String>,
    state: State<'_, AppState>,
    db: State<'_, Db>,
) -> Result<TaskHandle, String> {
    let task_id = state.next_task_id.fetch_add(1, Ordering::SeqCst) + 1;
    {
        let mut guard = state.tasks.lock().map_err(|_| "task state poisoned".to_string())?;
        guard.insert(task_id, new_progress_payload("clean_starting", "Cleaning the package"));
    }
    let tasks = Arc::clone(&state.tasks);
    let scan_cache = Arc::clone(&state.scan_cache);
    let refs_cache = Arc::clone(&state.dep_refs_cache);
    let db = db.inner().clone();
    let additional = parse_additional_dirs(&additional_input_dirs.unwrap_or_default());
    thread::spawn(move || {
        let result = (|| -> Result<CleanResult, String> {
            let target_path = Path::new(&target_var_path);
            if !target_path.is_file() {
                return Err(format!("package not found on disk: {}", target_path.display()));
            }
            let backup_dir = backup_dir.as_deref().map(str::trim).filter(|s| !s.is_empty());
            if backup && backup_dir.is_none() {
                return Err("Choose a folder for the backups first.".to_string());
            }
            set_task_progress(&tasks, task_id, "clean_loading", 0.1, "Loading the folder scan");
            let scanned = load_cached_scan_with_target(&scan_cache, Path::new(&input_dir), &additional, Some(target_path))
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "No folder scan yet: check the package again.".to_string())?;
            set_task_progress(&tasks, task_id, "clean_writing", 0.3, "Rewriting the package");
            apply_clean(
                target_path,
                scanned,
                &keep_map,
                if backup { backup_dir.map(Path::new) } else { None },
                Some(&db),
                &refs_cache,
            )
            .map_err(|e| e.to_string())
        })();
        finish(&tasks, task_id, result);
    });
    Ok(TaskHandle { id: task_id })
}

fn finish(tasks: &Arc<Mutex<HashMap<u64, ProgressPayload>>>, task_id: u64, result: Result<CleanResult, String>) {
    if let Ok(mut guard) = tasks.lock() {
        if let Some(task) = guard.get_mut(&task_id) {
            task.done = true;
            task.progress = 1.0;
            match result {
                Ok(r) => {
                    task.phase = "clean_complete".to_string();
                    task.message = "Cleaned".to_string();
                    task.clean_result = Some(r);
                }
                Err(err) => {
                    task.phase = "clean_failed".to_string();
                    task.message = "Clean failed".to_string();
                    task.error = Some(err);
                }
            }
        }
    }
}
