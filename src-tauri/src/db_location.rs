//! Where the database file lives. By default in the app's data folder; the
//! user can move it to another folder (Settings → Database) and back.
//!
//! The choice is kept in its own small file, `db_location.json` in the app's
//! config folder, read before the database opens — not in the main config,
//! which the Settings page rewrites as a whole.
//!
//! Moving copies the database with `VACUUM INTO` (a consistent, compacted
//! copy) while holding the writer, checks the copy, records the new place and
//! restarts the app — still holding the writer, so nothing is written to the
//! old file after the copy. The old file is deleted at the next start, once
//! the new one has opened.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, OnceLock,
    },
    time::Duration,
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tauri::{Manager, State};

use crate::db::{Db, DB_FILE_NAME};
use crate::models::{AppState, TaskHandle};

const LOCATION_FILE: &str = "db_location.json";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Location {
    /// The folder holding the database; none for the default.
    #[serde(default)]
    dir: Option<String>,
    /// A database file left behind by a move, deleted once the new one opens.
    #[serde(default)]
    cleanup: Option<String>,
}

fn location_file(app: &tauri::AppHandle) -> Option<PathBuf> {
    app.path().app_config_dir().ok().map(|d| d.join(LOCATION_FILE))
}

fn read_location(app: &tauri::AppHandle) -> Location {
    location_file(app)
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn write_location(app: &tauri::AppHandle, loc: &Location) -> Result<()> {
    let path = location_file(app).context("no app config folder")?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(&path, serde_json::to_vec_pretty(loc)?).with_context(|| format!("failed to write {}", path.display()))
}

/// The default folder: the app's data folder.
pub(crate) fn default_dir(app: &tauri::AppHandle) -> Result<PathBuf> {
    app.path().app_data_dir().context("failed to resolve app data dir")
}

/// The open database's file, and a chosen folder that wasn't there at start.
static CURRENT: OnceLock<PathBuf> = OnceLock::new();
static MISSING: OnceLock<String> = OnceLock::new();

/// The database file to open: VVD_DB_PATH (testing), else the chosen folder,
/// else the default. A chosen folder that isn't there (a drive not plugged
/// in) falls back to the default, and Settings says so.
pub(crate) fn db_path(app: &tauri::AppHandle) -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("VVD_DB_PATH").filter(|p| !p.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    if let Some(dir) = read_location(app).dir.filter(|d| !d.trim().is_empty()) {
        let dir = PathBuf::from(dir);
        if dir.is_dir() {
            return Ok(dir.join(DB_FILE_NAME));
        }
        let _ = MISSING.set(dir.display().to_string());
    }
    let dir = default_dir(app)?;
    fs::create_dir_all(&dir).context("failed to create app data dir")?;
    Ok(dir.join(DB_FILE_NAME))
}

/// After the database at `path` opened: remember it, and delete the file a
/// move left behind (with its -wal and -shm).
pub(crate) fn opened(app: &tauri::AppHandle, path: &Path) {
    let _ = CURRENT.set(path.to_path_buf());
    let mut loc = read_location(app);
    let Some(old) = loc.cleanup.take() else { return };
    let old = PathBuf::from(old);
    if !same_file(&old, path) {
        for suffix in ["", "-wal", "-shm"] {
            let p = PathBuf::from(format!("{}{suffix}", old.display()));
            let _ = fs::remove_file(p);
        }
    }
    let _ = write_location(app, &loc);
}

fn same_file(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().replace('/', "\\").trim_end_matches('\\').to_ascii_lowercase();
    norm(a) == norm(b)
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct DbLocationInfo {
    /// The open database file and its folder.
    pub(crate) path: String,
    pub(crate) dir: String,
    pub(crate) default_dir: String,
    pub(crate) is_default: bool,
    pub(crate) size_bytes: u64,
    /// A chosen folder that wasn't there at start (the default was used).
    pub(crate) missing: Option<String>,
    /// VVD_DB_PATH is set: moving is off.
    pub(crate) testing: bool,
}

#[tauri::command]
pub(crate) fn db_location_info(app: tauri::AppHandle) -> Result<DbLocationInfo, String> {
    let path = CURRENT.get().cloned().map(Ok).unwrap_or_else(|| db_path(&app)).map_err(|e| e.to_string())?;
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let default = default_dir(&app).map_err(|e| e.to_string())?;
    Ok(DbLocationInfo {
        path: path.display().to_string(),
        dir: dir.display().to_string(),
        default_dir: default.display().to_string(),
        is_default: same_file(&dir, &default),
        size_bytes: fs::metadata(&path).map(|m| m.len()).unwrap_or(0),
        missing: MISSING.get().cloned(),
        testing: std::env::var_os("VVD_DB_PATH").is_some_and(|p| !p.is_empty()),
    })
}

/// Moves the database to `target_dir` (none: back to the default folder) as a
/// background task that ends by restarting the app.
///
/// When the folder already has a database: `use_existing` true switches to it
/// (yours stays where it is), false replaces it with yours; unset, the call
/// fails with `EXISTS:<path>` so the page can ask.
#[tauri::command]
pub(crate) fn start_move_database_task(
    target_dir: Option<String>,
    use_existing: Option<bool>,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    db: State<'_, Db>,
) -> Result<TaskHandle, String> {
    if std::env::var_os("VVD_DB_PATH").is_some_and(|p| !p.is_empty()) {
        return Err("VVD_DB_PATH is set (a test copy): moving is off.".to_string());
    }
    let current = CURRENT.get().cloned().ok_or("the database isn't open yet")?;
    let default = default_dir(&app).map_err(|e| e.to_string())?;
    let dir = match target_dir.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
        Some(d) => PathBuf::from(d),
        None => default.clone(),
    };
    fs::create_dir_all(&dir).map_err(|e| format!("Couldn't use {}: {e}", dir.display()))?;
    let target = dir.join(DB_FILE_NAME);
    if same_file(&target, &current) {
        return Err("The database is already there.".to_string());
    }
    // Can we write there at all?
    let probe = dir.join(".vvd-write-test");
    fs::write(&probe, b"").map_err(|e| format!("Can't write to {}: {e}", dir.display()))?;
    let _ = fs::remove_file(&probe);
    if target.exists() && use_existing.is_none() {
        return Err(format!("EXISTS:{}", target.display()));
    }
    let keep_dir = if same_file(&dir, &default) { None } else { Some(dir.display().to_string()) };
    let db = db.inner().clone();
    let (task_id, tasks, _cancel) = crate::packages::begin_task(&state, "db_move_starting", "Preparing")?;
    std::thread::spawn(move || {
        let result = (|| -> Result<String, String> {
            // Nothing is written while the copy is made, nor after, until the
            // restart: the writer stays held to the end.
            let writer = db.conn.lock().map_err(|_| "database connection poisoned".to_string())?;
            if use_existing == Some(true) {
                crate::tasks::set_task_progress(&tasks, task_id, "db_move_check", 0.3, "Checking the database there".to_string());
                check_database(&target)?;
                write_location(&app, &Location { dir: keep_dir, cleanup: None }).map_err(|e| e.to_string())?;
                finish(&tasks, task_id, &target, "Switching to the database there");
                restart_holding(&app, writer);
            }
            let part = dir.join(format!("{DB_FILE_NAME}.part"));
            let _ = fs::remove_file(&part);
            // The copy's progress: its size against the open file's.
            let total = fs::metadata(&current).map(|m| m.len()).unwrap_or(1).max(1);
            let copying = Arc::new(AtomicBool::new(true));
            let watcher = {
                let (copying, part, tasks) = (Arc::clone(&copying), part.clone(), Arc::clone(&tasks));
                std::thread::spawn(move || {
                    while copying.load(Ordering::SeqCst) {
                        let done = fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
                        let f = (done as f64 / total as f64).min(1.0) * 0.8;
                        crate::tasks::set_task_progress(&tasks, task_id, "db_move_copy", f, "Copying your database".to_string());
                        std::thread::sleep(Duration::from_millis(300));
                    }
                })
            };
            let copied = writer.execute("VACUUM INTO ?1", [part.display().to_string()]);
            copying.store(false, Ordering::SeqCst);
            let _ = watcher.join();
            if let Err(e) = copied {
                let _ = fs::remove_file(&part);
                return Err(format!("Couldn't copy the database there: {e}"));
            }
            crate::tasks::set_task_progress(&tasks, task_id, "db_move_check", 0.85, "Checking the copy".to_string());
            let (packages, rows) = counts(&writer).map_err(|e| e.to_string())?;
            let copy = check_database(&part);
            let same = copy.as_ref().is_ok_and(|c| *c == (packages, rows));
            if !same {
                let _ = fs::remove_file(&part);
                return Err(match copy {
                    Err(e) => format!("The copy didn't check out: {e}"),
                    Ok(c) => format!("The copy has {} packages and {} files, not {packages} and {rows}", c.0, c.1),
                });
            }
            for suffix in ["", "-wal", "-shm"] {
                let _ = fs::remove_file(format!("{}{suffix}", target.display()));
            }
            fs::rename(&part, &target).map_err(|e| format!("Couldn't put the copy in place: {e}"))?;
            write_location(&app, &Location { dir: keep_dir, cleanup: Some(current.display().to_string()) })
                .map_err(|e| e.to_string())?;
            finish(&tasks, task_id, &target, "Moved: restarting");
            restart_holding(&app, writer);
        })();
        if let Err(e) = result {
            if let Ok(mut guard) = tasks.lock() {
                if let Some(task) = guard.get_mut(&task_id) {
                    task.done = true;
                    task.phase = "db_move_failed".to_string();
                    task.message = "The database wasn't moved".to_string();
                    task.error = Some(e);
                }
            }
        }
    });
    Ok(TaskHandle { id: task_id })
}

fn finish(tasks: &crate::models::TaskMap, task_id: u64, target: &Path, message: &str) {
    if let Ok(mut guard) = tasks.lock() {
        if let Some(task) = guard.get_mut(&task_id) {
            task.done = true;
            task.progress = 1.0;
            task.phase = "db_move_complete".to_string();
            task.message = format!("{message}: {}", target.display());
        }
    }
}

/// Gives the page a moment to say so, then restarts — with the writer still
/// held, so nothing reaches the old file.
fn restart_holding(app: &tauri::AppHandle, writer: std::sync::MutexGuard<'_, rusqlite::Connection>) -> ! {
    std::thread::sleep(Duration::from_millis(1500));
    let _ = writer.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
    app.restart()
}

fn counts(conn: &rusqlite::Connection) -> rusqlite::Result<(i64, i64)> {
    conn.query_row("SELECT (SELECT COUNT(*) FROM packages), (SELECT COUNT(*) FROM res_rows)", [], |r| {
        Ok((r.get(0)?, r.get(1)?))
    })
}

/// A database file that opens, passes SQLite's quick check, and is this
/// app's (it has its tables): its package and file counts.
fn check_database(path: &Path) -> Result<(i64, i64), String> {
    let conn = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("it doesn't open: {e}"))?;
    let check: String = conn.query_row("PRAGMA quick_check", [], |r| r.get(0)).map_err(|e| e.to_string())?;
    if check != "ok" {
        return Err(format!("SQLite's check says: {check}"));
    }
    let has_rows: bool = conn
        .query_row("SELECT 1 FROM sqlite_master WHERE name IN ('res_rows', 'resources')", [], |_| Ok(true))
        .unwrap_or(false);
    if !has_rows {
        return Err("it isn't this app's database".to_string());
    }
    let packages: i64 = conn.query_row("SELECT COUNT(*) FROM packages", [], |r| r.get(0)).map_err(|e| e.to_string())?;
    let rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM res_rows", [], |r| r.get(0))
        .or_else(|_| conn.query_row("SELECT COUNT(*) FROM resources", [], |r| r.get(0)))
        .map_err(|e| e.to_string())?;
    Ok((packages, rows))
}

#[cfg(test)]
pub(crate) fn check_database_for_test(path: &Path) -> Result<(i64, i64), String> {
    check_database(path)
}
