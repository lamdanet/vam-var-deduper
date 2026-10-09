//! Dependency Usage — is a dependency worth keeping?
//!
//! A package that depends on a big one may use only a few files of it. Whether
//! copying those files in (Internalize Resources) saves space depends on every
//! package that uses it: a shared library (many packages using much of it)
//! should stay, a big package each user barely touches can go. For each
//! dependency of one package, this reads which of its files every package that
//! depends on it references (with what they need: a .vam's .vaj, textures) and
//! adds up the bytes. Read-only: no archive is changed, the database isn't used.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    thread,
    time::SystemTime,
};

use anyhow::{Context, Result};
use tauri::State;
use zip::ZipArchive;

use crate::{
    fix_var::{collect_pkg_refs, is_text_path},
    internalize::expand_bundle_for_ref,
    library::{read_var_pkg_info, DepResolution, LibIndex},
    models::{
        AppState, DepUsage, DepUsageReport, DepUser, ProgressPayload, TaskHandle,
        VarPackageListItem,
    },
    naming,
    tasks::{new_progress_payload, set_task_progress},
    utils::{decode_text, normalize_zip_path},
};

/// What one package references in others: package family (lowercase) ->
/// internal paths.
pub(crate) type RefMap = HashMap<String, BTreeSet<String>>;

/// Each package's references, kept for the session and checked against the
/// file's size and modified time, so a second analysis reads only what changed.
#[derive(Default)]
pub(crate) struct RefCache {
    entries: HashMap<PathBuf, (u64, Option<SystemTime>, Arc<RefMap>)>,
}

fn stamp(path: &Path) -> (u64, Option<SystemTime>) {
    fs::metadata(path)
        .map(|m| (m.len(), m.modified().ok()))
        .unwrap_or((0, None))
}

/// Every `Pkg:/path` reference in a package's text files (scenes, presets,
/// .vaj/.vam/.vap…), by the referenced package's family.
pub(crate) fn read_refs(path: &Path) -> RefMap {
    let mut out: RefMap = HashMap::new();
    let Ok(file) = fs::File::open(path) else {
        return out;
    };
    let Ok(mut archive) = ZipArchive::new(file) else {
        return out;
    };
    for index in 0..archive.len() {
        let Ok(mut entry) = archive.by_index(index) else {
            continue;
        };
        if entry.is_dir() {
            continue;
        }
        let name = normalize_zip_path(entry.name());
        // Huge text files are generated data, not references.
        if !is_text_path(&name) || entry.size() > 128 * 1024 * 1024 {
            continue;
        }
        let mut raw = Vec::new();
        if entry.read_to_end(&mut raw).is_err() {
            continue;
        }
        let Some((text, _)) = decode_text(&raw) else {
            continue;
        };
        for (pkg, ref_path) in collect_pkg_refs(&text) {
            out.entry(naming::package_base(&pkg).to_ascii_lowercase())
                .or_default()
                .insert(normalize_zip_path(&ref_path));
        }
    }
    out
}

fn refs_cached(cache: &Mutex<RefCache>, path: &Path) -> Arc<RefMap> {
    let st = stamp(path);
    if let Ok(guard) = cache.lock() {
        if let Some((len, modified, refs)) = guard.entries.get(path) {
            if (*len, *modified) == st {
                return Arc::clone(refs);
            }
        }
    }
    let refs = Arc::new(read_refs(path));
    if let Ok(mut guard) = cache.lock() {
        guard
            .entries
            .insert(path.to_path_buf(), (st.0, st.1, Arc::clone(&refs)));
    }
    refs
}

/// For each dependency of `target`: who uses it (the target included), how
/// many bytes each of them references, and the totals. `items` is the VAR
/// Packages listing (the folders' packages). `progress(fraction, message)`.
pub(crate) fn analyze_dependency_usage(
    target: &Path,
    items: &[VarPackageListItem],
    cache: &Mutex<RefCache>,
    progress: impl Fn(f64, &str) + Sync,
) -> Result<DepUsageReport> {
    let target_str = target.to_string_lossy().to_string();
    let target_id = target
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string();
    let own = items
        .iter()
        .position(|it| it.file_path.eq_ignore_ascii_case(&target_str));
    let declared: Vec<String> = match own {
        Some(i) => items[i].deps.clone(),
        None => read_var_pkg_info(target).deps,
    };

    // Resolve each declared dependency to a package in the folders (once per
    // package: `.latest` and `.3` of the same one are the same dependency).
    let index = LibIndex::build(items);
    let mut deps: Vec<DepUsage> = Vec::new();
    let mut slot_of: HashMap<usize, usize> = HashMap::new();
    for key in &declared {
        let (hit, other_version) = match index.resolve(key) {
            DepResolution::Found(t) => (Some(t), false),
            DepResolution::OtherVersion(t) => (Some(t), true),
            DepResolution::Missing => (None, false),
        };
        if let Some(t) = hit {
            if Some(t) == own || slot_of.contains_key(&t) {
                continue;
            }
            slot_of.insert(t, deps.len());
            deps.push(DepUsage {
                declared: key.clone(),
                package_id: Some(items[t].package_id.clone()),
                file_path: Some(items[t].file_path.clone()),
                size: items[t].size_bytes,
                other_version,
                ..DepUsage::default()
            });
        } else {
            deps.push(DepUsage {
                declared: key.clone(),
                ..DepUsage::default()
            });
        }
    }

    // Who uses each of them: every package whose dependencies resolve to it.
    let mut users: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, item) in items.iter().enumerate() {
        if Some(i) == own || item.file_path.eq_ignore_ascii_case(&target_str) {
            continue;
        }
        let mut seen: HashSet<usize> = HashSet::new();
        for dep in &item.deps {
            if let DepResolution::Found(t) | DepResolution::OtherVersion(t) = index.resolve(dep) {
                if t != i && slot_of.contains_key(&t) && seen.insert(t) {
                    users.entry(t).or_default().push(i);
                }
            }
        }
    }

    // Read every user's references once (the slow part), several at a time.
    let mut to_read: Vec<PathBuf> = vec![target.to_path_buf()];
    let mut queued: HashSet<usize> = HashSet::new();
    for list in users.values() {
        for &u in list {
            if queued.insert(u) {
                to_read.push(PathBuf::from(&items[u].file_path));
            }
        }
    }
    let total = to_read.len();
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let read: Mutex<HashMap<PathBuf, Arc<RefMap>>> = Mutex::new(HashMap::new());
    let workers = thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(2, 8);
    thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                let Some(path) = to_read.get(i) else {
                    break;
                };
                let refs = refs_cached(cache, path);
                if let Ok(mut guard) = read.lock() {
                    guard.insert(path.clone(), refs);
                }
                let n = done.fetch_add(1, Ordering::SeqCst) + 1;
                if n.is_multiple_of(8) || n == total {
                    progress(0.05 + 0.85 * n as f64 / total as f64, &format!("Reading packages that use them: {n} of {total}"));
                }
            });
        }
    });
    let read = read.into_inner().unwrap_or_default();
    let empty = Arc::new(RefMap::new());
    let refs_of = |p: &str| read.get(Path::new(p)).cloned().unwrap_or_else(|| Arc::clone(&empty));

    // Add up each dependency: open it once, measure what each user references.
    let resolved: Vec<(usize, usize)> = slot_of.iter().map(|(&t, &slot)| (t, slot)).collect();
    for (n, (t, slot)) in resolved.iter().enumerate() {
        progress(0.9 + 0.1 * n as f64 / resolved.len().max(1) as f64, "Adding up what each one uses");
        let dep = &items[*t];
        let family = naming::package_base(&dep.package_id).to_ascii_lowercase();
        let file = fs::File::open(&dep.file_path)
            .with_context(|| format!("failed to open {}", dep.file_path))?;
        let Ok(mut archive) = ZipArchive::new(file) else {
            continue;
        };
        let mut entry_sizes: BTreeMap<String, (u64, u32)> = BTreeMap::new();
        for index in 0..archive.len() {
            if let Ok(e) = archive.by_index(index) {
                if !e.is_dir() {
                    entry_sizes.insert(normalize_zip_path(e.name()), (e.size(), e.crc32()));
                }
            }
        }
        let mut bundles: HashMap<String, Vec<String>> = HashMap::new();
        let mut measure = |paths: Option<&BTreeSet<String>>| -> BTreeSet<String> {
            let mut files = BTreeSet::new();
            for p in paths.into_iter().flatten() {
                if !entry_sizes.contains_key(p) {
                    continue;
                }
                let bundle = bundles
                    .entry(p.clone())
                    .or_insert_with(|| expand_bundle_for_ref(p, &entry_sizes, &mut archive));
                files.extend(bundle.iter().cloned());
            }
            files
        };
        let mut union: BTreeSet<String> = BTreeSet::new();
        let mut list: Vec<DepUser> = Vec::new();
        let mut who: Vec<(String, String, bool)> = vec![(target_id.clone(), target_str.clone(), true)];
        for &u in users.get(t).map(Vec::as_slice).unwrap_or(&[]) {
            who.push((items[u].package_id.clone(), items[u].file_path.clone(), false));
        }
        for (id, path, is_target) in who {
            let files = measure(refs_of(&path).get(&family));
            let bytes: u64 = files.iter().filter_map(|f| entry_sizes.get(f)).map(|(s, _)| *s).sum();
            union.extend(files.iter().cloned());
            list.push(DepUser {
                package_id: id,
                file_path: path,
                bytes,
                files: files.len() as u32,
                is_target,
            });
        }
        let usage = &mut deps[*slot];
        usage.union_bytes = union.iter().filter_map(|f| entry_sizes.get(f)).map(|(s, _)| *s).sum();
        usage.sum_bytes = list.iter().map(|u| u.bytes).sum();
        if let Some(me) = list.iter().find(|u| u.is_target) {
            usage.target_bytes = me.bytes;
            usage.target_files = me.files;
        }
        list.sort_by(|a, b| b.is_target.cmp(&a.is_target).then(b.bytes.cmp(&a.bytes)));
        usage.users = list;
    }

    Ok(DepUsageReport {
        target_package_id: target_id,
        packages_read: total as u32,
        deps,
    })
}

/// Runs the analysis for one package on a worker thread; the UI polls
/// `get_task_progress` and reads `dependency_usage_result`. Needs the VAR
/// Packages listing (`list_var_packages`) loaded.
#[tauri::command]
pub(crate) fn start_dependency_usage_task(
    target_var_path: String,
    state: State<'_, AppState>,
) -> Result<TaskHandle, String> {
    let items = state
        .var_packages_folder_cache
        .lock()
        .map_err(|_| "var packages folder cache poisoned".to_string())?
        .as_ref()
        .map(|c| c.items.clone())
        .ok_or_else(|| "The package list isn't loaded yet: open VAR Packages once, then try again.".to_string())?;
    let task_id = state.next_task_id.fetch_add(1, Ordering::SeqCst) + 1;
    {
        let mut guard = state.tasks.lock().map_err(|_| "task state poisoned".to_string())?;
        guard.insert(task_id, new_progress_payload("dep_usage_starting", "Reading its dependencies"));
    }
    let tasks = Arc::clone(&state.tasks);
    let cache = Arc::clone(&state.dep_refs_cache);
    thread::spawn(move || {
        let result = analyze_dependency_usage(Path::new(&target_var_path), &items, &cache, |f, msg| {
            set_task_progress(&tasks, task_id, "dep_usage_reading", f, msg);
        });
        finish(&tasks, task_id, result);
    });
    Ok(TaskHandle { id: task_id })
}

fn finish(tasks: &Arc<Mutex<HashMap<u64, ProgressPayload>>>, task_id: u64, result: Result<DepUsageReport>) {
    if let Ok(mut guard) = tasks.lock() {
        if let Some(task) = guard.get_mut(&task_id) {
            task.done = true;
            task.progress = 1.0;
            match result {
                Ok(report) => {
                    task.phase = "dep_usage_complete".to_string();
                    task.message = "Done".to_string();
                    task.dependency_usage_result = Some(report);
                }
                Err(err) => {
                    task.phase = "dep_usage_failed".to_string();
                    task.message = "Failed".to_string();
                    task.error = Some(err.to_string());
                }
            }
        }
    }
}
