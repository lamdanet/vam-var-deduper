//! Clean many — one package the user keeps as the source, and the other
//! packages that carry copies of its files.
//!
//! The source is never written. Each package the user ticks is cleaned as
//! Clean VARs would clean it with every choice pointing at the source: the
//! files it uses itself that the source has whole go, its references point
//! at the source, which becomes its dependency. Same rules as one package
//! (`clean_var`): what the package itself keeps (a plugin's path, a preset's
//! picture) stays, and only a whole copy counts. There's no backup folder:
//! each original goes to the Recycle Bin, and a package whose drive has none
//! is left as it is.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    path::{Path, PathBuf},
    sync::{atomic::Ordering, Arc, Mutex},
    thread,
};

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use tauri::State;

use crate::{
    clean_var::{apply_clean_with, clean_candidates_from, dep_families, find_target, CleanResult, Original},
    db::Db,
    models::{AppState, ProgressPayload, RecycleSupport, ScannedData, TaskHandle},
    naming,
    packages::{friendly_trash_error, recycle_support_for},
    scan::load_cached_scan_shared,
    tasks::{new_progress_payload, parse_additional_dirs, set_task_progress},
};

/// A file a package carries that the source has too.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct CleanManyFile {
    pub(crate) key: String,
    /// In the package.
    pub(crate) path: String,
    /// In the source: what its references will point at.
    pub(crate) source_path: String,
    /// With what goes along with it (a .vam's .vaj, .vab, textures).
    pub(crate) size: u64,
    pub(crate) bundle: Vec<String>,
    /// Why it stays in the package: `script`, `picture:<file>` (as Clean
    /// VARs), or `incomplete` (the source lacks part of it: `lacks`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) stays: Option<String>,
    #[serde(default)]
    pub(crate) lacks: Vec<String>,
}

/// A package with copies of the source's files.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct CleanManyPackage {
    pub(crate) package_id: String,
    pub(crate) file_path: String,
    /// On disk.
    pub(crate) size_bytes: u64,
    /// It already lists the source (any version) as a dependency.
    pub(crate) already_dependency: bool,
    /// The source lists it (any version) as a dependency: they'd depend on
    /// each other.
    pub(crate) source_depends_on_it: bool,
    /// Another version of the source.
    pub(crate) same_family: bool,
    /// The files it uses itself that the source has, the biggest first.
    pub(crate) files: Vec<CleanManyFile>,
    /// Copies it has but doesn't use itself: left alone.
    pub(crate) unreferenced_files: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct CleanManyFailure {
    pub(crate) package_id: String,
    pub(crate) file_path: String,
    pub(crate) error: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct CleanManyReport {
    pub(crate) source_package_id: String,
    pub(crate) source_file_path: String,
    /// Those that use at least one of the copies, the most to gain first.
    pub(crate) packages: Vec<CleanManyPackage>,
    /// Packages with copies they don't use themselves: nothing to clean.
    pub(crate) unused_only: u32,
    /// Packages that couldn't be read.
    pub(crate) failed: Vec<CleanManyFailure>,
}

fn can_go(f: &CleanManyFile) -> bool {
    f.stays.is_none()
}

/// The other packages with copies of the source's files, and which of them
/// each one could point at the source.
pub(crate) fn clean_many_candidates(
    source_path: &Path,
    scanned: &ScannedData,
    progress: &dyn Fn(usize, usize, &str),
) -> Result<CleanManyReport> {
    let source = find_target(scanned, source_path).ok_or_else(|| anyhow!("the package isn't in the scan: check it again"))?;
    let source_id = source.package_id.clone();
    let family = naming::package_base(&source_id).to_ascii_lowercase();
    let source_deps = dep_families(source);
    let others: BTreeSet<&str> = scanned
        .duplicate_groups
        .iter()
        .filter(|g| g.refs.iter().any(|r| r.package_id == source_id))
        .flat_map(|g| g.refs.iter().map(|r| r.package_id.as_str()))
        .filter(|id| *id != source_id)
        .collect();
    let mut report = CleanManyReport {
        source_package_id: source_id.clone(),
        source_file_path: source.file_path.to_string_lossy().to_string(),
        ..CleanManyReport::default()
    };
    for (i, id) in others.iter().enumerate() {
        let Some(p) = scanned.packages.get(*id) else {
            continue;
        };
        progress(i, others.len(), id);
        let file_path = p.file_path.to_string_lossy().to_string();
        let found = match clean_candidates_from(&p.file_path, scanned, None, Some(&source_id)) {
            Ok(found) => found,
            Err(err) => {
                report.failed.push(CleanManyFailure {
                    package_id: id.to_string(),
                    file_path,
                    error: err.to_string(),
                });
                continue;
            }
        };
        let files: Vec<CleanManyFile> = found
            .items
            .into_iter()
            .filter_map(|item| {
                let copy = item.copies.into_iter().next()?;
                let lacks = copy.incomplete;
                Some(CleanManyFile {
                    stays: item.stays.or_else(|| (!lacks.is_empty()).then(|| "incomplete".to_string())),
                    key: item.key,
                    path: item.path,
                    source_path: copy.internal_path,
                    size: item.size,
                    bundle: item.bundle,
                    lacks,
                })
            })
            .collect();
        if files.is_empty() {
            report.unused_only += 1;
            continue;
        }
        let base = naming::package_base(id).to_ascii_lowercase();
        report.packages.push(CleanManyPackage {
            package_id: id.to_string(),
            size_bytes: fs::metadata(&p.file_path).map(|m| m.len()).unwrap_or(0),
            file_path,
            already_dependency: dep_families(p).contains(&family),
            source_depends_on_it: source_deps.contains(&base),
            same_family: base == family,
            files,
            unreferenced_files: found.unreferenced_files,
        });
    }
    let gain = |p: &CleanManyPackage| p.files.iter().filter(|f| can_go(f)).map(|f| f.size).sum::<u64>();
    report
        .packages
        .sort_by(|a, b| gain(b).cmp(&gain(a)).then_with(|| a.package_id.to_lowercase().cmp(&b.package_id.to_lowercase())));
    Ok(report)
}

/// Cleans one package so it points at the source for every file the source
/// has whole, its original sent away by `recycle`. Worked out again from the
/// scan, not taken from the page: what the page showed, by the same rules.
pub(crate) fn clean_one_from(
    source_id: &str,
    package_path: &Path,
    scanned: ScannedData,
    recycle: &dyn Fn(&Path) -> Result<(), String>,
    db: Option<&Db>,
) -> Result<CleanResult> {
    let found = clean_candidates_from(package_path, &scanned, None, Some(source_id))?;
    let keep: BTreeMap<String, String> = found
        .items
        .iter()
        .filter(|i| i.stays.is_none())
        .filter_map(|i| {
            let c = i.copies.iter().find(|c| c.incomplete.is_empty())?;
            Some((i.key.clone(), format!("{}:{}", c.package_id, c.internal_path)))
        })
        .collect();
    if keep.is_empty() {
        bail!("nothing in it can point at {source_id} now: check again");
    }
    apply_clean_with(package_path, scanned, &keep, Original::Recycle(recycle), db)
}

/// One package's outcome.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct CleanManyDone {
    pub(crate) package_id: String,
    pub(crate) file_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) result: Option<CleanResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct CleanManyResult {
    pub(crate) source_package_id: String,
    pub(crate) done: Vec<CleanManyDone>,
}

/// Cleans each of `packages` against the source, one after the other; one
/// that fails is left as it was and the rest go on. A package on a drive
/// without a Recycle Bin isn't touched.
pub(crate) fn clean_many(
    source_path: &Path,
    packages: &[PathBuf],
    scanned: &ScannedData,
    recycle_ok: &dyn Fn(&Path) -> bool,
    recycle: &dyn Fn(&Path) -> Result<(), String>,
    db: Option<&Db>,
    progress: &dyn Fn(usize, usize, &str),
) -> Result<CleanManyResult> {
    let source = find_target(scanned, source_path).ok_or_else(|| anyhow!("the package isn't in the scan: check it again"))?;
    let source_id = source.package_id.clone();
    let source_file = source.file_path.clone();
    // Every package first: one the page listed that isn't there any more
    // means the folders changed since the check, and nothing is written.
    let mut todo = Vec::new();
    let mut seen = BTreeSet::new();
    for path in packages {
        let p = find_target(scanned, path).ok_or_else(|| anyhow!("{} isn't in the scan: check it again", path.display()))?;
        if p.package_id == source_id || p.file_path == source_file {
            bail!("{} is the package the others point at: it isn't cleaned", p.package_id);
        }
        if seen.insert(p.package_id.clone()) {
            todo.push((p.package_id.clone(), p.file_path.clone()));
        }
    }
    let mut out = CleanManyResult {
        source_package_id: source_id.clone(),
        done: Vec::new(),
    };
    for (i, (id, path)) in todo.iter().enumerate() {
        progress(i, todo.len(), id);
        let outcome = if !recycle_ok(path) {
            Err("its drive has no Recycle Bin, so it wasn't changed".to_string())
        } else {
            clean_one_from(&source_id, path, scanned.clone(), recycle, db).map_err(|e| e.to_string())
        };
        let (result, error) = match outcome {
            Ok(r) => (Some(r), None),
            Err(e) => (None, Some(e)),
        };
        out.done.push(CleanManyDone {
            package_id: id.clone(),
            file_path: path.to_string_lossy().to_string(),
            result,
            error,
        });
    }
    Ok(out)
}

/// Lists the packages with copies of the source's files. Needs the folder
/// scan with the source as its target (`start_scan_task`) cached.
#[tauri::command]
pub(crate) fn start_clean_many_check_task(
    input_dir: String,
    additional_input_dirs: Option<Vec<String>>,
    source_var_path: String,
    state: State<'_, AppState>,
) -> Result<TaskHandle, String> {
    let task_id = state.next_task_id.fetch_add(1, Ordering::SeqCst) + 1;
    {
        let mut guard = state.tasks.lock().map_err(|_| "task state poisoned".to_string())?;
        guard.insert(task_id, new_progress_payload("clean_many_check", "Reading the packages with its files"));
    }
    let tasks = Arc::clone(&state.tasks);
    let scan_cache = Arc::clone(&state.scan_cache);
    let additional = parse_additional_dirs(&additional_input_dirs.unwrap_or_default());
    thread::spawn(move || {
        let result = (|| -> Result<CleanManyReport, String> {
            let source_path = Path::new(&source_var_path);
            let scanned = load_cached_scan_shared(&scan_cache, Path::new(&input_dir), &additional, Some(source_path))
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "No folder scan yet: check the package again.".to_string())?;
            clean_many_candidates(source_path, &scanned, &|i, n, id| {
                let f = if n == 0 { 0.0 } else { i as f64 / n as f64 };
                set_task_progress(&tasks, task_id, "clean_many_check", f, format!("Reading {id} ({} of {n})", i + 1));
            })
            .map_err(|e| e.to_string())
        })();
        finish(&tasks, task_id, |task| match result {
            Ok(r) => {
                task.message = "Checked".to_string();
                task.clean_many_report = Some(r);
                Ok(())
            }
            Err(e) => Err(e),
        });
    });
    Ok(TaskHandle { id: task_id })
}

/// Cleans the ticked packages against the source (never the source itself);
/// each original goes to the Recycle Bin.
#[allow(clippy::too_many_arguments)] // a Tauri command: one argument per field the UI sends
#[tauri::command]
pub(crate) fn start_clean_many_task(
    input_dir: String,
    additional_input_dirs: Option<Vec<String>>,
    source_var_path: String,
    package_paths: Vec<String>,
    state: State<'_, AppState>,
    db: State<'_, Db>,
) -> Result<TaskHandle, String> {
    let task_id = state.next_task_id.fetch_add(1, Ordering::SeqCst) + 1;
    {
        let mut guard = state.tasks.lock().map_err(|_| "task state poisoned".to_string())?;
        guard.insert(task_id, new_progress_payload("clean_many_starting", "Cleaning the packages"));
    }
    let tasks = Arc::clone(&state.tasks);
    let scan_cache = Arc::clone(&state.scan_cache);
    let recycle_cache = Arc::clone(&state.recycle_support);
    let db = db.inner().clone();
    let additional = parse_additional_dirs(&additional_input_dirs.unwrap_or_default());
    thread::spawn(move || {
        let result = (|| -> Result<CleanManyResult, String> {
            let source_path = Path::new(&source_var_path);
            if !source_path.is_file() {
                return Err(format!("package not found on disk: {}", source_path.display()));
            }
            if package_paths.is_empty() {
                return Err("Tick the packages to clean first.".to_string());
            }
            set_task_progress(&tasks, task_id, "clean_many_loading", 0.02, "Loading the folder scan");
            // Read once, while every package is as it was at the check: each
            // write then changes only its own package.
            let scanned = load_cached_scan_shared(&scan_cache, Path::new(&input_dir), &additional, Some(source_path))
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "Your folders changed since the check: check the package again.".to_string())?;
            let paths: Vec<PathBuf> = package_paths.iter().map(PathBuf::from).collect();
            let recycle_ok = |p: &Path| recycle_support_for(&recycle_cache, p) == RecycleSupport::Supported;
            let recycle = |p: &Path| trash::delete(p).map_err(|e| friendly_trash_error(&e));
            clean_many(source_path, &paths, &scanned, &recycle_ok, &recycle, Some(&db), &|i, n, id| {
                let f = 0.05 + 0.95 * (i as f64 / n.max(1) as f64);
                set_task_progress(&tasks, task_id, "clean_many_writing", f, format!("Cleaning {id} ({} of {n})", i + 1));
            })
            .map_err(|e| e.to_string())
        })();
        finish(&tasks, task_id, |task| match result {
            Ok(r) => {
                task.message = "Cleaned".to_string();
                task.clean_many_result = Some(r);
                Ok(())
            }
            Err(e) => Err(e),
        });
    });
    Ok(TaskHandle { id: task_id })
}

fn finish(
    tasks: &Arc<Mutex<HashMap<u64, ProgressPayload>>>,
    task_id: u64,
    fill: impl FnOnce(&mut ProgressPayload) -> Result<(), String>,
) {
    if let Ok(mut guard) = tasks.lock() {
        if let Some(task) = guard.get_mut(&task_id) {
            task.done = true;
            task.progress = 1.0;
            if let Err(err) = fill(task) {
                task.phase = "clean_many_failed".to_string();
                task.message = "Failed".to_string();
                task.error = Some(err);
            }
        }
    }
}
