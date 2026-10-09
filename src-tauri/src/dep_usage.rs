//! Dependency Usage — is a package worth keeping as a dependency?
//!
//! Packages that depend on a big one may each use only a few files of it.
//! Whether it should stay depends on all of them: a shared library (many
//! packages using much of it) earns its place, a big package its users barely
//! touch can go once each of them has its own copy of what it uses (Internalize
//! Resources). This reads, for one package, every package in the folders that
//! depends on it: which of its files each one references (with what they need:
//! a .vam's .vaj, textures), and adds up the bytes. Read-only: no archive is
//! changed, the database isn't used.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
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
    library::{DepResolution, LibIndex},
    models::{
        AppState, PackageUsageReport, PackageUser, ProgressPayload, TaskHandle, UsedFile,
        VarPackageListItem, META_PATH,
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

/// Reads several packages' references, several at a time (cached), in the
/// order given. `progress(done, total)`.
pub(crate) fn read_refs_many(
    paths: &[PathBuf],
    cache: &Mutex<RefCache>,
    progress: impl Fn(usize, usize) + Sync,
) -> Vec<Arc<RefMap>> {
    let total = paths.len();
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let read: Mutex<HashMap<usize, Arc<RefMap>>> = Mutex::new(HashMap::new());
    let workers = thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(2, 8);
    thread::scope(|s| {
        for _ in 0..workers.min(total.max(1)) {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                let Some(path) = paths.get(i) else {
                    break;
                };
                let refs = refs_cached(cache, path);
                if let Ok(mut guard) = read.lock() {
                    guard.insert(i, refs);
                }
                progress(done.fetch_add(1, Ordering::SeqCst) + 1, total);
            });
        }
    });
    let mut read = read.into_inner().unwrap_or_default();
    (0..total).map(|i| read.remove(&i).unwrap_or_default()).collect()
}

/// The most used files the report lists; the rest are only counted.
const USED_FILES_CAP: usize = 400;

/// Who uses `package` as a dependency and how much of it, from the VAR
/// Packages listing `items` (the folders' packages). `progress(fraction,
/// message)`.
pub(crate) fn analyze_package_usage(
    package: &Path,
    items: &[VarPackageListItem],
    cache: &Mutex<RefCache>,
    progress: impl Fn(f64, &str) + Sync,
) -> Result<PackageUsageReport> {
    let package_str = package.to_string_lossy().to_string();
    let package_id = package
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string();
    let family = naming::package_base(&package_id).to_ascii_lowercase();
    let own = items
        .iter()
        .position(|it| it.file_path.eq_ignore_ascii_case(&package_str));

    // Its users: every package whose dependencies resolve to it (any listed
    // version that lands on this file), or name its family when it isn't in
    // the listing.
    let index = LibIndex::build(items);
    let users: Vec<usize> = items
        .iter()
        .enumerate()
        .filter(|(i, item)| {
            Some(*i) != own
                && !item.file_path.eq_ignore_ascii_case(&package_str)
                && item.deps.iter().any(|dep| match own {
                    Some(t) => matches!(
                        index.resolve(dep),
                        DepResolution::Found(x) | DepResolution::OtherVersion(x) if x == t
                    ),
                    None => naming::package_base(dep).eq_ignore_ascii_case(&family),
                })
        })
        .map(|(i, _)| i)
        .collect();

    // Read every user's references (the slow part), several at a time.
    let total = users.len();
    let paths: Vec<PathBuf> = users.iter().map(|&u| PathBuf::from(&items[u].file_path)).collect();
    let read: HashMap<usize, Arc<RefMap>> = users
        .iter()
        .copied()
        .zip(read_refs_many(&paths, cache, |n, total| {
            if n.is_multiple_of(4) || n == total {
                progress(0.05 + 0.85 * n as f64 / total as f64, &format!("Reading the packages that use it: {n} of {total}"));
            }
        }))
        .collect();
    progress(0.92, "Adding up what each one uses");

    // Measure what each user references in it.
    let file = fs::File::open(package).with_context(|| format!("failed to open {package_str}"))?;
    let mut archive = ZipArchive::new(file).with_context(|| format!("not a readable package: {package_str}"))?;
    let mut entry_sizes: BTreeMap<String, (u64, u32)> = BTreeMap::new();
    for index in 0..archive.len() {
        if let Ok(e) = archive.by_index(index) {
            let name = normalize_zip_path(e.name());
            // meta.json describes the package; it isn't something to use.
            if !e.is_dir() && name != META_PATH {
                entry_sizes.insert(name, (e.size(), e.crc32()));
            }
        }
    }
    let size_of = |files: &BTreeSet<String>| -> u64 {
        files.iter().filter_map(|f| entry_sizes.get(f)).map(|(s, _)| *s).sum()
    };
    let mut bundles: HashMap<String, Vec<String>> = HashMap::new();
    let mut file_users: HashMap<String, u32> = HashMap::new();
    let mut list: Vec<PackageUser> = Vec::new();
    for &u in &users {
        let mut paths: Vec<String> = Vec::new();
        let mut files: BTreeSet<String> = BTreeSet::new();
        if let Some(refs) = read.get(&u).and_then(|r| r.get(&family)) {
            for p in refs {
                if !entry_sizes.contains_key(p) {
                    continue;
                }
                paths.push(p.clone());
                let bundle = bundles
                    .entry(p.clone())
                    .or_insert_with(|| expand_bundle_for_ref(p, &entry_sizes, &mut archive));
                files.extend(bundle.iter().cloned());
            }
        }
        for f in &files {
            *file_users.entry(f.clone()).or_default() += 1;
        }
        list.push(PackageUser {
            package_id: items[u].package_id.clone(),
            file_path: items[u].file_path.clone(),
            bytes: size_of(&files),
            files: files.len() as u32,
            paths,
        });
    }
    list.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.package_id.to_lowercase().cmp(&b.package_id.to_lowercase())));

    let content_bytes: u64 = entry_sizes.values().map(|(s, _)| *s).sum();
    let union_bytes: u64 = file_users
        .keys()
        .filter_map(|f| entry_sizes.get(f))
        .map(|(s, _)| *s)
        .sum();
    let mut used_files: Vec<UsedFile> = file_users
        .iter()
        .map(|(path, n)| UsedFile {
            path: path.clone(),
            size: entry_sizes.get(path).map(|(s, _)| *s).unwrap_or(0),
            users: *n,
        })
        .collect();
    used_files.sort_by(|a, b| b.users.cmp(&a.users).then(b.size.cmp(&a.size)).then_with(|| a.path.cmp(&b.path)));
    let used_count = used_files.len() as u32;
    used_files.truncate(USED_FILES_CAP);

    Ok(PackageUsageReport {
        package_id,
        file_path: package_str,
        size: fs::metadata(package).map(|m| m.len()).unwrap_or(0),
        file_count: entry_sizes.len() as u32,
        content_bytes,
        sum_bytes: list.iter().map(|u| u.bytes).sum(),
        union_bytes,
        used_count,
        unused_bytes: content_bytes.saturating_sub(union_bytes),
        used_files,
        users: list,
        packages_read: total as u32,
    })
}

/// Runs the analysis for one package on a worker thread; the UI polls
/// `get_task_progress` and reads `package_usage_result`. Needs the VAR
/// Packages listing (`list_var_packages`) loaded.
#[tauri::command]
pub(crate) fn start_package_usage_task(
    package_path: String,
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
        guard.insert(task_id, new_progress_payload("package_usage_starting", "Finding the packages that use it"));
    }
    let tasks = Arc::clone(&state.tasks);
    let cache = Arc::clone(&state.dep_refs_cache);
    thread::spawn(move || {
        let result = analyze_package_usage(Path::new(&package_path), &items, &cache, |f, msg| {
            set_task_progress(&tasks, task_id, "package_usage_reading", f, msg);
        });
        finish(&tasks, task_id, result);
    });
    Ok(TaskHandle { id: task_id })
}

fn finish(tasks: &Arc<Mutex<HashMap<u64, ProgressPayload>>>, task_id: u64, result: Result<PackageUsageReport>) {
    if let Ok(mut guard) = tasks.lock() {
        if let Some(task) = guard.get_mut(&task_id) {
            task.done = true;
            task.progress = 1.0;
            match result {
                Ok(report) => {
                    task.phase = "package_usage_complete".to_string();
                    task.message = "Done".to_string();
                    task.package_usage_result = Some(report);
                }
                Err(err) => {
                    task.phase = "package_usage_failed".to_string();
                    task.message = "Failed".to_string();
                    task.error = Some(err.to_string());
                }
            }
        }
    }
}
