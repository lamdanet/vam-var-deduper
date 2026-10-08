//! Full integrity check of `.var` packages, after VaM Backstage's
//! `scanner/integrity.js`: every entry is decompressed and its CRC-32 checked
//! against the zip's central directory (the zip crate verifies it when an
//! entry is read to the end). The library scan only reads `meta.json`, so a
//! truncated or bit-rotted download otherwise goes unnoticed until VaM
//! chokes on it.
//!
//! Results are stored per file with its size and modification time
//! (`var_integrity`), so a re-check skips unchanged files — a library can be
//! tens of gigabytes — and the Library shows a Damaged chip from them.

use std::{
    collections::HashMap,
    io,
    path::Path,
    sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    time::UNIX_EPOCH,
};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use tauri::State;
use zip::ZipArchive;

use crate::{
    db::Db,
    models::{AppState, TaskHandle, VarPackageListItem},
    tasks::set_task_progress,
};

/// Read every entry to the end. `Err` names the first entry that fails.
pub(crate) fn verify_var(path: &Path) -> Result<(), String> {
    let file = std::fs::File::open(path).map_err(|e| format!("can't open it: {e}"))?;
    let mut archive = ZipArchive::new(file).map_err(|e| format!("not a readable zip: {e}"))?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| format!("entry {i}: {e}"))?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_string();
        io::copy(&mut entry, &mut io::sink()).map_err(|e| format!("{name}: {e}"))?;
    }
    Ok(())
}

/// Size and modification time (ns), the key a stored result is valid for.
pub(crate) fn fingerprint(path: &Path) -> Option<(u64, String)> {
    let meta = std::fs::metadata(path).ok()?;
    let ns = meta
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_nanos()
        .to_string();
    Some((meta.len(), ns))
}

/// Bumped when stored results change, so the cached Library listing re-reads
/// them.
static GENERATION: AtomicU64 = AtomicU64::new(1);

pub(crate) fn generation() -> u64 {
    GENERATION.load(Ordering::SeqCst)
}

/// Fill `damaged` on library items from stored results that still match the
/// file on disk.
pub(crate) fn annotate(items: &mut [VarPackageListItem], db: &Db) {
    let stored = crate::db::integrity_all(db).unwrap_or_default();
    for item in items.iter_mut() {
        item.damaged = stored.get(&item.file_path).and_then(|row| {
            let fresh = fingerprint(Path::new(&item.file_path))
                .is_some_and(|(size, ns)| size == row.size && ns == row.modified_ns);
            (fresh && !row.ok).then(|| row.error.clone().unwrap_or_else(|| "damaged".into()))
        });
    }
}

#[derive(Debug, Clone)]
pub(crate) struct IntegrityRow {
    pub(crate) size: u64,
    pub(crate) modified_ns: String,
    pub(crate) ok: bool,
    pub(crate) error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct IntegrityIssue {
    pub(crate) file_path: String,
    pub(crate) package_id: String,
    pub(crate) error: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct IntegrityResponse {
    /// Files read in full this time.
    pub(crate) checked: usize,
    /// Unchanged since an earlier check, so not read again.
    pub(crate) cached: usize,
    pub(crate) damaged: Vec<IntegrityIssue>,
    pub(crate) was_cancelled: bool,
}

fn package_id_of(path: &Path) -> String {
    path.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_string()
}

pub(crate) fn run_verify(
    file_paths: &[String],
    recheck: bool,
    db: &Db,
    cancel: &AtomicBool,
    progress: &(dyn Fn(usize, usize, &str) + Sync),
) -> IntegrityResponse {
    let stored: HashMap<String, IntegrityRow> = crate::db::integrity_all(db).unwrap_or_default();
    let done = AtomicUsize::new(0);
    let total = file_paths.len();
    let cached_hits = AtomicUsize::new(0);
    // Two at a time: disk-bound, and a second reader hides per-file overhead.
    let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build();
    let work = || {
        file_paths
            .par_iter()
            .filter_map(|p| {
                if cancel.load(Ordering::SeqCst) {
                    return None;
                }
                let path = Path::new(p);
                let print = fingerprint(path)?;
                let previous = stored.get(p).filter(|row| row.size == print.0 && row.modified_ns == print.1);
                let (ok, error) = match previous {
                    Some(row) if !recheck => {
                        cached_hits.fetch_add(1, Ordering::SeqCst);
                        (row.ok, row.error.clone())
                    }
                    _ => match verify_var(path) {
                        Ok(()) => (true, None),
                        Err(e) => (false, Some(e)),
                    },
                };
                let n = done.fetch_add(1, Ordering::SeqCst) + 1;
                progress(n, total, &package_id_of(path));
                Some((p.clone(), print, ok, error))
            })
            .collect::<Vec<_>>()
    };
    let results = match pool {
        Ok(pool) => pool.install(work),
        Err(_) => work(),
    };
    let rows: Vec<(String, IntegrityRow)> = results
        .iter()
        .map(|(p, (size, ns), ok, error)| {
            (p.clone(), IntegrityRow { size: *size, modified_ns: ns.clone(), ok: *ok, error: error.clone() })
        })
        .collect();
    let _ = crate::db::integrity_put(db, &rows);
    GENERATION.fetch_add(1, Ordering::SeqCst);
    let cached = cached_hits.load(Ordering::SeqCst);
    IntegrityResponse {
        checked: results.len().saturating_sub(cached),
        cached,
        damaged: results
            .into_iter()
            .filter(|(_, _, ok, _)| !ok)
            .map(|(p, _, _, error)| IntegrityIssue {
                package_id: package_id_of(Path::new(&p)),
                file_path: p,
                error: error.unwrap_or_else(|| "damaged".into()),
            })
            .collect(),
        was_cancelled: cancel.load(Ordering::SeqCst),
    }
}

/// Check packages in the background. `recheck` reads files again even when
/// unchanged since their last check.
#[tauri::command]
pub(crate) fn start_verify_packages_task(
    file_paths: Vec<String>,
    recheck: bool,
    state: State<'_, AppState>,
    db: State<'_, Db>,
) -> Result<TaskHandle, String> {
    let (task_id, tasks, cancel) = crate::packages::begin_task(&state, "verify_starting", "Checking packages")?;
    let db = db.inner().clone();
    std::thread::spawn(move || {
        let report = |n: usize, total: usize, name: &str| {
            let frac = if total == 0 { 1.0 } else { n as f64 / total as f64 };
            set_task_progress(&tasks, task_id, "verify_running", frac, format!("Checking {n} / {total} · {name}"));
        };
        let response = run_verify(&file_paths, recheck, &db, &cancel, &report);
        if let Ok(mut guard) = tasks.lock() {
            if let Some(task) = guard.get_mut(&task_id) {
                task.done = true;
                task.progress = 1.0;
                task.phase = "verify_complete".into();
                task.message = format!(
                    "Checked {} package(s), {} damaged",
                    response.checked + response.cached,
                    response.damaged.len()
                );
                task.verify_result = Some(response);
            }
        }
    });
    Ok(TaskHandle { id: task_id })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn write_var(path: &Path) {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        zip.start_file("meta.json", SimpleFileOptions::default()).unwrap();
        zip.write_all(b"{}").unwrap();
        zip.start_file(
            "Custom/Clothing/x.vam",
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
        )
        .unwrap();
        zip.write_all(&[7u8; 4096]).unwrap();
        zip.finish().unwrap();
    }

    #[test]
    fn a_flipped_byte_is_caught_and_results_are_cached() {
        let dir = std::env::temp_dir().join(format!("vam_integrity_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let good = dir.join("A.Good.1.var");
        let bad = dir.join("A.Bad.1.var");
        write_var(&good);
        write_var(&bad);
        // Flip a byte inside the stored entry's data: the zip still opens and
        // meta.json still reads, only the CRC gives it away.
        let mut bytes = std::fs::read(&bad).unwrap();
        let at = bytes.windows(8).position(|w| w == [7u8; 8]).unwrap() + 100;
        bytes[at] ^= 0xff;
        std::fs::write(&bad, bytes).unwrap();
        assert!(verify_var(&good).is_ok());
        let err = verify_var(&bad).unwrap_err();
        assert!(err.contains("x.vam"), "{err}");

        let db = crate::db::open_in_memory().unwrap();
        let paths = vec![good.display().to_string(), bad.display().to_string()];
        let cancel = AtomicBool::new(false);
        let first = run_verify(&paths, false, &db, &cancel, &|_, _, _| {});
        assert_eq!((first.checked, first.cached, first.damaged.len()), (2, 0, 1));
        assert_eq!(first.damaged[0].package_id, "A.Bad.1");
        let second = run_verify(&paths, false, &db, &cancel, &|_, _, _| {});
        assert_eq!((second.checked, second.cached, second.damaged.len()), (0, 2, 1), "unchanged files aren't re-read");

        let mut items = vec![
            VarPackageListItem { file_path: paths[0].clone(), ..Default::default() },
            VarPackageListItem { file_path: paths[1].clone(), ..Default::default() },
        ];
        annotate(&mut items, &db);
        assert!(items[0].damaged.is_none());
        assert!(items[1].damaged.is_some());
        std::fs::remove_dir_all(&dir).ok();
    }
}
