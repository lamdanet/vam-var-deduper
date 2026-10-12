//! Saving a download source also indexes its package when the database
//! doesn't know it yet: the `.var` (a zip) is listed through its saved link
//! — only its central directory, a few small range requests, never the whole
//! file — and its files (path, CRC32, size, category) go into the database as
//! a scan would store them. So Fix Missing, Clean VARs and the Resource List
//! can use a package before it's downloaded.
//!
//! Not from the listing: what a `.vaj` says it loads (the scan reads that from
//! the file itself), so a clothing item's effective size can be smaller until
//! the package is downloaded and scanned.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::db::Db;
use crate::models::{AppState, ResourceRef, TaskHandle};

/// What indexing the newly saved sources did.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct SourceIndexReport {
    /// Packages added to the database, with their file count.
    pub(crate) added: Vec<(String, usize)>,
    /// Already in the database: left as they are.
    pub(crate) known: usize,
    /// Couldn't be indexed, and why (a link inside a zip, a host that won't
    /// serve parts of files, not a package…).
    pub(crate) skipped: Vec<(String, String)>,
    pub(crate) was_cancelled: bool,
}

/// The `.var` names the database doesn't know yet, among `filenames`.
pub(crate) fn new_packages(db: &Db, filenames: &[String]) -> (Vec<String>, usize) {
    let mut seen = HashSet::new();
    let mut fresh = Vec::new();
    let mut known = 0;
    let Ok(conn) = db.read() else { return (fresh, known) };
    let Ok(mut stmt) = conn.prepare_cached("SELECT 1 FROM packages WHERE lower(package_id) = lower(?1)") else {
        return (fresh, known);
    };
    for name in filenames {
        let id = name.trim().trim_end_matches(".var").trim_end_matches(".VAR").to_string();
        if id.is_empty() || !seen.insert(id.to_ascii_lowercase()) {
            continue;
        }
        if stmt.exists([&id]).unwrap_or(false) {
            known += 1;
        } else {
            fresh.push(id);
        }
    }
    (fresh, known)
}

/// A package's rows from its listing, as the scan makes them (folders and
/// meta.json left out), with the effective sizes its file names imply.
pub(crate) fn rows_from_listing(package_id: &str, files: &[crate::archives::RemoteFile]) -> Result<Vec<ResourceRef>, String> {
    let meta = crate::models::META_PATH;
    if !files.iter().any(|f| crate::utils::normalize_zip_path(&f.name) == meta) {
        // Mirror lists often link a zip holding the .var (and a picture):
        // its own file list is inside, compressed.
        if files.iter().any(|f| f.name.to_ascii_lowercase().ends_with(".var")) {
            return Err("the link is a zip holding the .var: it's added once downloaded".to_string());
        }
        return Err("not a package (no meta.json inside)".to_string());
    }
    let mut rows: Vec<ResourceRef> = files
        .iter()
        .filter(|f| !f.name.ends_with('/'))
        .map(|f| (crate::utils::normalize_zip_path(&f.name), f))
        .filter(|(path, _)| !path.is_empty() && path != meta)
        .map(|(internal_path, f)| ResourceRef {
            package_id: package_id.to_string(),
            package_file: String::new(),
            internal_path,
            crc32: Some(f.crc32),
            size: f.size,
            effective_size: f.size,
        })
        .collect();
    let mut support: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for r in &rows {
        support.extend(crate::scan::support_paths_by_name(&r.internal_path));
    }
    crate::scan::apply_effective_resource_sizes(&mut rows, &support);
    Ok(rows)
}

/// Lists one package through its saved links (best first, those inside a
/// zip skipped) and writes it. Returns its file count.
fn index_one(db: &Db, package_id: &str) -> Result<usize, String> {
    let filename = format!("{package_id}.var");
    let links = crate::db::links_for_file(db, &filename);
    let direct: Vec<_> = links.iter().filter(|l| l.archive_entry.is_none()).collect();
    if direct.is_empty() {
        return Err(if links.is_empty() {
            "no saved link".to_string()
        } else {
            "only inside a zip: indexed once it's downloaded".to_string()
        });
    }
    let mut last_error = String::new();
    for link in direct {
        match crate::archives::remote_zip_listing(&link.url) {
            Ok(listing) => {
                let rows = rows_from_listing(package_id, &listing.files)?;
                return write_package(db, package_id, listing.len, &rows).map_err(|e| e.to_string());
            }
            Err(e) => last_error = e,
        }
    }
    Err(last_error)
}

/// The package row (no file on disk: `source:<id>` stands for its path, as
/// a manifest import uses `manifest:<id>`) and its rows, in one transaction.
/// Returns how many rows were stored (a path a zip lists twice is one row).
pub(crate) fn write_package(db: &Db, package_id: &str, size: u64, rows: &[ResourceRef]) -> anyhow::Result<usize> {
    let mut conn = db.conn.lock().map_err(|_| anyhow::anyhow!("database connection poisoned"))?;
    let category_ids = db.categories.clone();
    let tx = conn.transaction()?;
    let creator_id = match crate::naming::creator_from_package_id(package_id) {
        Some(name) => Some(crate::db::ensure_creator_id(&tx, name)?),
        None => None,
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    tx.execute(
        "INSERT OR IGNORE INTO packages
            (package_id, file_path, size_bytes, modified_ns, scene_image_path, last_scanned_at, creator_id)
         VALUES (?1, ?2, ?3, '0', NULL, ?4, ?5)",
        rusqlite::params![package_id, format!("source:{package_id}"), size as i64, now, creator_id],
    )?;
    let mut stored = 0;
    {
        let mut writer = crate::db::ResourceWriter::insert_or_ignore(&tx)?;
        for r in rows {
            let category = crate::db::category_for_path(&category_ids, &r.internal_path);
            stored += writer.write(package_id, &r.internal_path, r.crc32.map(i64::from), r.size as i64, r.effective_size as i64, category)?;
        }
    }
    tx.commit()?;
    Ok(stored)
}

/// Indexes the packages of `filenames` (just saved as sources) that the
/// database doesn't know, four at a time, as a background task.
#[tauri::command]
pub(crate) fn start_index_sources_task(
    filenames: Vec<String>,
    state: State<'_, AppState>,
    db: State<'_, Db>,
) -> Result<TaskHandle, String> {
    let db = db.inner().clone();
    let (task_id, tasks, cancel) = crate::packages::begin_task(&state, "source_index_starting", "Looking for new packages")?;
    std::thread::spawn(move || {
        use rayon::prelude::*;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let (fresh, known) = new_packages(&db, &filenames);
        let mut report = SourceIndexReport { known, ..Default::default() };
        let done = AtomicUsize::new(0);
        let total = fresh.len().max(1);
        let one = |id: &String| -> Result<(String, usize), (String, String)> {
            if cancel.load(Ordering::SeqCst) {
                return Err((id.clone(), "cancelled".to_string()));
            }
            let out = index_one(&db, id).map(|n| (id.clone(), n)).map_err(|e| (id.clone(), e));
            let n = done.fetch_add(1, Ordering::SeqCst) + 1;
            crate::tasks::set_task_progress(&tasks, task_id, "source_index", n as f64 / total as f64, format!("Read {n} of {} new packages", fresh.len()));
            out
        };
        // Hosts answer slowly but independently: four packages at a time.
        let results: Vec<_> = match rayon::ThreadPoolBuilder::new().num_threads(4).build() {
            Ok(pool) => pool.install(|| fresh.par_iter().map(one).collect()),
            Err(_) => fresh.iter().map(one).collect(),
        };
        for r in results {
            match r {
                Ok(added) => report.added.push(added),
                Err((id, why)) if why == "cancelled" => {
                    report.was_cancelled = true;
                    report.skipped.push((id, why));
                }
                Err(skipped) => report.skipped.push(skipped),
            }
        }
        if let Ok(mut guard) = tasks.lock() {
            if let Some(task) = guard.get_mut(&task_id) {
                task.done = true;
                task.progress = 1.0;
                task.phase = "source_index_complete".to_string();
                task.message = format!("{} new package(s) added to the database", report.added.len());
                task.source_index_result = Some(report);
            }
        }
    });
    Ok(TaskHandle { id: task_id })
}
