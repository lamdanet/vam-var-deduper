//! Where the database file lives. By default in the app's data folder; the
//! user can move it to another folder (Settings → Database) and back.
//!
//! The choice is kept in its own small file, `db_location.json` in the app's
//! config folder, read before the database opens — not in the main config,
//! which the Settings page rewrites as a whole.
//!
//! Moving copies the database with `VACUUM INTO` (a consistent, compacted
//! copy) while holding the writer, flushes the copy to disk, checks it,
//! records the new place and restarts the app — still holding the writer, so
//! nothing is written to the old file after the copy. The old file stays
//! until the new one has opened twice (or Settings' "Delete the old copy"),
//! and only ever when the new one is what opened, never after a fallback.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
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
#[serde(default)]
struct Location {
    /// The folder holding the database; none for the default.
    dir: Option<String>,
    /// A database file left behind by a move, deleted once the move's copy
    /// (`cleanup_target`) has opened twice.
    cleanup: Option<String>,
    cleanup_target: Option<String>,
    /// How many times `cleanup_target` has opened since the move.
    cleanup_opens: u32,
}

/// The test overrides (a test build keeps away from your database and
/// settings). Debug builds only: a stray variable never moves a real install.
fn test_override(name: &str) -> Option<PathBuf> {
    if !cfg!(debug_assertions) {
        return None;
    }
    std::env::var_os(name).filter(|d| !d.is_empty()).map(PathBuf::from)
}

/// VVD_DATA_DIR: the folder a test build uses for its data and settings.
pub(crate) fn test_data_dir() -> Option<PathBuf> {
    test_override("VVD_DATA_DIR")
}

fn test_db_path() -> Option<PathBuf> {
    test_override("VVD_DB_PATH")
}

fn location_file(app: &tauri::AppHandle) -> Option<PathBuf> {
    if let Some(dir) = test_data_dir() {
        return Some(dir.join(LOCATION_FILE));
    }
    app.path().app_config_dir().ok().map(|d| d.join(LOCATION_FILE))
}

fn read_location(app: &tauri::AppHandle) -> Location {
    location_file(app)
        .and_then(|p| crate::utils::read_file_with_backup(&p, |t| serde_json::from_str(t).ok()))
        .unwrap_or_default()
}

fn write_location(app: &tauri::AppHandle, loc: &Location) -> Result<()> {
    let path = location_file(app).context("no app config folder")?;
    crate::utils::write_file_atomic(&path, &serde_json::to_vec_pretty(loc)?)
}

/// The default folder: the app's data folder (VVD_DATA_DIR stands in for it
/// when testing a move, so the real database is never touched).
pub(crate) fn default_dir(app: &tauri::AppHandle) -> Result<PathBuf> {
    if let Some(dir) = test_data_dir() {
        return Ok(dir);
    }
    app.path().app_data_dir().context("failed to resolve app data dir")
}

/// The database file this run uses (decided once), the open one, and a chosen
/// folder that wasn't there at start.
static RESOLVED: OnceLock<PathBuf> = OnceLock::new();
static CURRENT: OnceLock<PathBuf> = OnceLock::new();
static MISSING: Mutex<Option<String>> = Mutex::new(None);
/// A move is under way: no new task starts (they would die in the restart).
static MOVING: AtomicBool = AtomicBool::new(false);

/// Fails while the database is being moved: tasks call this before starting.
pub(crate) fn ensure_not_moving() -> Result<(), String> {
    if MOVING.load(Ordering::SeqCst) {
        return Err("The database is being moved; the app restarts when it's done.".to_string());
    }
    Ok(())
}

/// A new task's id — refused while a move runs, since the restart would end
/// the task halfway.
pub(crate) fn next_task_id(state: &AppState) -> Result<u64, String> {
    ensure_not_moving()?;
    Ok(state.next_task_id.fetch_add(1, Ordering::SeqCst) + 1)
}

fn missing() -> Option<String> {
    MISSING.lock().ok().and_then(|m| m.clone())
}

/// The database file to open: VVD_DB_PATH (testing), else the chosen folder,
/// else the default. When the chosen folder isn't there (a drive not plugged
/// in), the user is asked: try again, use the default folder for now, or
/// quit — never silently a new, empty database.
pub(crate) fn db_path(app: &tauri::AppHandle) -> Result<PathBuf> {
    if let Some(path) = RESOLVED.get() {
        return Ok(path.clone());
    }
    let path = resolve_db_path(app)?;
    Ok(RESOLVED.get_or_init(|| path).clone())
}

fn resolve_db_path(app: &tauri::AppHandle) -> Result<PathBuf> {
    if let Some(path) = test_db_path() {
        return Ok(path);
    }
    if let Some(dir) = read_location(app).dir.filter(|d| !d.trim().is_empty()) {
        let dir = PathBuf::from(dir);
        loop {
            if dir.is_dir() {
                return Ok(dir.join(DB_FILE_NAME));
            }
            match ask_missing_folder(&dir) {
                MissingChoice::Retry => continue,
                MissingChoice::UseDefault => break,
                MissingChoice::Quit => std::process::exit(0),
            }
        }
        if let Ok(mut m) = MISSING.lock() {
            *m = Some(dir.display().to_string());
        }
    }
    let dir = default_dir(app)?;
    fs::create_dir_all(&dir).context("failed to create app data dir")?;
    Ok(dir.join(DB_FILE_NAME))
}

enum MissingChoice {
    Retry,
    UseDefault,
    Quit,
}

fn ask_missing_folder(dir: &Path) -> MissingChoice {
    use rfd::{MessageButtons, MessageDialog, MessageDialogResult, MessageLevel};
    let text = format!(
        "Your database is in {}, and that folder isn't there (a drive not plugged in?).\n\n\
         Yes: try again (plug the drive in first).\n\
         No: use the database in the app's data folder for now. Settings says so; your database stays where it is.\n\
         Cancel: quit.",
        dir.display()
    );
    let answer = MessageDialog::new()
        .set_level(MessageLevel::Warning)
        .set_title("Database folder not found")
        .set_description(text)
        .set_buttons(MessageButtons::YesNoCancel)
        .show();
    match answer {
        MessageDialogResult::Yes => MissingChoice::Retry,
        MessageDialogResult::No => MissingChoice::UseDefault,
        _ => MissingChoice::Quit,
    }
}

/// After the database at `path` opened: remember it, and deal with the file a
/// move left behind. That file goes only once the move's copy is what opened
/// — never after a fallback to the default, where the old file may be the
/// only good copy — and only at its second start, so a first start that
/// fails still has the original.
pub(crate) fn opened(app: &tauri::AppHandle, path: &Path) {
    let _ = CURRENT.set(path.to_path_buf());
    let mut loc = read_location(app);
    let Some(old) = loc.cleanup.clone().map(PathBuf::from) else { return };
    if same_file(&old, path) {
        // Moved back onto the old file: nothing to delete.
        clear_cleanup(&mut loc);
        let _ = write_location(app, &loc);
        return;
    }
    let target_opened = match &loc.cleanup_target {
        Some(target) => same_file(Path::new(target), path),
        // Written by an earlier version: only when the chosen folder opened.
        None => missing().is_none(),
    };
    if !target_opened {
        return;
    }
    loc.cleanup_opens += 1;
    if loc.cleanup_opens >= 2 && delete_database_files(&old) {
        clear_cleanup(&mut loc);
    }
    let _ = write_location(app, &loc);
}

fn clear_cleanup(loc: &mut Location) {
    loc.cleanup = None;
    loc.cleanup_target = None;
    loc.cleanup_opens = 0;
}

/// Deletes a database file with its -wal and -shm: true when none is left.
fn delete_database_files(path: &Path) -> bool {
    let mut gone = true;
    for suffix in ["", "-wal", "-shm"] {
        let p = PathBuf::from(format!("{}{suffix}", path.display()));
        if p.exists() && fs::remove_file(&p).is_err() {
            gone = false;
        }
    }
    gone
}

fn same_file(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().replace('/', "\\").trim_end_matches('\\').to_ascii_lowercase();
    norm(a) == norm(b)
}

/// The old copy a move left, while it's still waiting to be deleted.
fn pending_old_copy(app: &tauri::AppHandle) -> Option<String> {
    let loc = read_location(app);
    let old = loc.cleanup?;
    let current = CURRENT.get()?;
    let target_opened = loc.cleanup_target.as_deref().is_some_and(|t| same_file(Path::new(t), current));
    (target_opened && Path::new(&old).exists()).then_some(old)
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
    /// The file a move left behind, deleted at the next start.
    pub(crate) old_copy: Option<String>,
    /// VVD_DB_PATH is set: moving is off.
    pub(crate) testing: bool,
}

#[tauri::command(async)]
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
        missing: missing(),
        old_copy: pending_old_copy(&app),
        testing: test_db_path().is_some(),
    })
}

/// Deletes the file a move left behind now, rather than at the next start.
#[tauri::command(async)]
pub(crate) fn delete_old_database_copy(app: tauri::AppHandle) -> Result<(), String> {
    let Some(old) = pending_old_copy(&app) else { return Ok(()) };
    if !delete_database_files(Path::new(&old)) {
        return Err(format!("Couldn't delete {old}: is it open in another program?"));
    }
    let mut loc = read_location(&app);
    clear_cleanup(&mut loc);
    write_location(&app, &loc).map_err(|e| e.to_string())
}

/// Forgets a chosen folder that wasn't there at start: the database in the
/// default folder (already open) becomes the one used. Nothing is moved.
#[tauri::command(async)]
pub(crate) fn forget_database_folder(app: tauri::AppHandle) -> Result<(), String> {
    let mut loc = read_location(&app);
    loc.dir = None;
    write_location(&app, &loc).map_err(|e| e.to_string())?;
    if let Ok(mut m) = MISSING.lock() {
        *m = None;
    }
    Ok(())
}

/// A database file's size, counts and age, for the "already has one" choice.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct DbSummary {
    pub(crate) path: String,
    pub(crate) size_bytes: u64,
    /// Seconds since 1970 of the last change, when known.
    pub(crate) modified: Option<u64>,
    pub(crate) packages: Option<i64>,
    pub(crate) files: Option<i64>,
    /// Why it can't be used (it doesn't open, isn't this app's…).
    pub(crate) problem: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct DbTargetCheck {
    /// Where the database would go.
    pub(crate) path: String,
    /// The database already there, if any.
    pub(crate) existing: Option<DbSummary>,
    /// The open database.
    pub(crate) mine: DbSummary,
    /// Things to know before moving there (a removable drive…).
    pub(crate) warnings: Vec<String>,
}

fn summarize(path: &Path, counts: Result<(i64, i64), String>) -> DbSummary {
    let meta = fs::metadata(path).ok();
    // The -wal holds recent changes too: its time counts as the last change.
    let wal = fs::metadata(format!("{}-wal", path.display())).ok();
    let modified = [meta.as_ref(), wal.as_ref()]
        .into_iter()
        .flatten()
        .filter_map(|m| m.modified().ok())
        .max()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs());
    let (packages, files, problem) = match counts {
        Ok((p, f)) => (Some(p), Some(f), None),
        Err(e) => (None, None, Some(e)),
    };
    DbSummary {
        path: path.display().to_string(),
        size_bytes: meta.map(|m| m.len()).unwrap_or(0),
        modified,
        packages,
        files,
        problem,
    }
}

/// The folder a move would go to (none: the default), normalized.
fn target_folder(app: &tauri::AppHandle, target_dir: Option<&str>) -> Result<PathBuf, String> {
    match target_dir.map(str::trim).filter(|d| !d.is_empty()) {
        // Stored the way Windows writes paths, whichever slashes it came with.
        Some(d) => Ok(PathBuf::from(d.replace('/', "\\"))),
        None => default_dir(app).map_err(|e| e.to_string()),
    }
}

/// Everything that rules a folder out, then what's worth knowing about it.
fn vet_target(dir: &Path, current: &Path, need: u64) -> Result<Vec<String>, String> {
    if same_file(&dir.join(DB_FILE_NAME), current) {
        return Err("The database is already there.".to_string());
    }
    let warnings = drive_checks(dir, need)?;
    fs::create_dir_all(dir).map_err(|e| format!("Couldn't use {}: {e}", dir.display()))?;
    // Can we write there at all?
    let probe = dir.join(".vvd-write-test");
    fs::write(&probe, b"").map_err(|e| format!("Can't write to {}: {e}", dir.display()))?;
    let _ = fs::remove_file(&probe);
    Ok(warnings)
}

/// Folders SQLite can't safely keep a database in are refused: network
/// shares (its locking and WAL need a local disk) and folders a sync client
/// copies (it copies the database and its journal separately). A removable
/// or FAT32 drive gets a warning, and the copy has to fit.
fn drive_checks(dir: &Path, need: u64) -> Result<Vec<String>, String> {
    let text = dir.display().to_string();
    if text.starts_with("\\\\") {
        return Err(format!(
            "{text} is a network folder. The database needs a drive in this PC: SQLite can't safely keep it on a network share."
        ));
    }
    if let Some(client) = synced_by(dir) {
        return Err(format!(
            "{text} is synced by {client}. Sync clients copy the database and its journal separately and can corrupt it: pick a folder they don't sync."
        ));
    }
    let mut warnings = Vec::new();
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows::{
            core::PCWSTR,
            Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetDriveTypeW, GetVolumeInformationW},
        };
        // The DRIVE_* values are plain C #defines the crate doesn't bind.
        const DRIVE_REMOVABLE: u32 = 2;
        const DRIVE_REMOTE: u32 = 4;
        let root = crate::packages::volume_root(dir);
        let mut wide: Vec<u16> = root.as_os_str().encode_wide().collect();
        wide.push(0);
        let root_w = PCWSTR(wide.as_ptr());
        match unsafe { GetDriveTypeW(root_w) } {
            DRIVE_REMOTE => {
                return Err(format!(
                    "{text} is on a network drive. The database needs a drive in this PC: SQLite can't safely keep it on a network share."
                ))
            }
            DRIVE_REMOVABLE => warnings.push(
                "It's on a removable drive: keep it plugged in while the app runs. Without it, the app asks at start what to do.".to_string(),
            ),
            _ => {}
        }
        let mut fs_name = [0u16; 32];
        if unsafe { GetVolumeInformationW(root_w, None, None, None, None, Some(&mut fs_name)) }.is_ok() {
            let name = String::from_utf16_lossy(&fs_name).trim_end_matches('\0').to_ascii_uppercase();
            if name.starts_with("FAT") {
                const FAT_LIMIT: u64 = 4 * 1024 * 1024 * 1024 - 1;
                if need >= FAT_LIMIT {
                    return Err(format!("{text} is on a {name} drive, which can't hold files over 4 GB."));
                }
                warnings.push(format!(
                    "That drive is {name}: it can't hold files over 4 GB, and the database grows with every scan."
                ));
            }
        }
        // The folder may not exist yet: ask about its drive.
        let mut dir_w: Vec<u16> = dir.ancestors().find(|a| a.is_dir()).unwrap_or(&root).as_os_str().encode_wide().collect();
        dir_w.push(0);
        let mut free = 0u64;
        if unsafe { GetDiskFreeSpaceExW(PCWSTR(dir_w.as_ptr()), Some(&mut free), None, None) }.is_ok() {
            let want = need + need / 10 + 64 * 1024 * 1024;
            if free < want {
                return Err(format!(
                    "Not enough space there: the copy needs about {} and {} is free.",
                    human_bytes(want),
                    human_bytes(free)
                ));
            }
        }
    }
    Ok(warnings)
}

/// The sync client keeping `dir` in sync, if any (by its well-known folders).
fn synced_by(dir: &Path) -> Option<&'static str> {
    let lower = dir.display().to_string().replace('/', "\\").to_ascii_lowercase();
    for var in ["OneDrive", "OneDriveConsumer", "OneDriveCommercial"] {
        if let Some(root) = std::env::var_os(var).filter(|v| !v.is_empty()) {
            let root = root.to_string_lossy().replace('/', "\\").trim_end_matches('\\').to_ascii_lowercase();
            if lower == root || lower.starts_with(&format!("{root}\\")) {
                return Some("OneDrive");
            }
        }
    }
    let parts: Vec<&str> = lower.split('\\').collect();
    let has = |f: &dyn Fn(&str) -> bool| parts.iter().any(|p| f(p));
    if has(&|p| p == "onedrive" || p.starts_with("onedrive - ")) {
        Some("OneDrive")
    } else if has(&|p| p == "dropbox" || p.starts_with("dropbox (")) {
        Some("Dropbox")
    } else if has(&|p| p == "google drive" || p == "my drive") {
        Some("Google Drive")
    } else if has(&|p| p == "iclouddrive" || p == "icloud drive") {
        Some("iCloud Drive")
    } else {
        None
    }
}

fn human_bytes(n: u64) -> String {
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    if n as f64 >= GB {
        format!("{:.1} GB", n as f64 / GB)
    } else {
        format!("{:.0} MB", n as f64 / MB)
    }
}

/// What a move to `target_dir` (none: the default folder) would meet: the
/// database already there, with both sides' counts, and any warnings. Fails
/// when the folder can't be used at all.
#[tauri::command(async)]
pub(crate) fn check_database_target(
    target_dir: Option<String>,
    app: tauri::AppHandle,
    db: State<'_, Db>,
) -> Result<DbTargetCheck, String> {
    let current = CURRENT.get().cloned().ok_or("the database isn't open yet")?;
    let dir = target_folder(&app, target_dir.as_deref())?;
    let need = fs::metadata(&current).map(|m| m.len()).unwrap_or(0);
    let warnings = vet_target(&dir, &current, need)?;
    let target = dir.join(DB_FILE_NAME);
    let mine_counts = db
        .read()
        .map_err(|e| e.to_string())
        .and_then(|conn| counts(&conn).map_err(|e| e.to_string()));
    Ok(DbTargetCheck {
        path: target.display().to_string(),
        existing: target.exists().then(|| summarize(&target, check_database(&target))),
        mine: summarize(&current, mine_counts),
        warnings,
    })
}

/// Tasks still running, by what they say they're doing.
fn running_tasks(state: &AppState) -> Vec<String> {
    state
        .tasks
        .lock()
        .map(|tasks| tasks.values().filter(|t| !t.done).map(|t| t.message.clone()).collect())
        .unwrap_or_default()
}

/// Moves the database to `target_dir` (none: back to the default folder) as a
/// background task that ends by restarting the app.
///
/// When the folder already has a database: `use_existing` true switches to it
/// (yours stays where it is), false replaces it with yours (the one there is
/// kept beside it as `….replaced.db`); unset, the call fails with
/// `EXISTS:<path>` so the page can ask.
#[tauri::command(async)]
pub(crate) fn start_move_database_task(
    target_dir: Option<String>,
    use_existing: Option<bool>,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    db: State<'_, Db>,
) -> Result<TaskHandle, String> {
    if test_db_path().is_some() {
        return Err("VVD_DB_PATH is set (a test copy): moving is off.".to_string());
    }
    let current = CURRENT.get().cloned().ok_or("the database isn't open yet")?;
    let default = default_dir(&app).map_err(|e| e.to_string())?;
    let dir = target_folder(&app, target_dir.as_deref())?;
    let need = fs::metadata(&current).map(|m| m.len()).unwrap_or(0);
    vet_target(&dir, &current, need)?;
    let target = dir.join(DB_FILE_NAME);
    if target.exists() && use_existing.is_none() {
        return Err(format!("EXISTS:{}", target.display()));
    }
    // The restart would end whatever runs now, halfway.
    let busy = running_tasks(&state);
    if !busy.is_empty() {
        return Err(format!(
            "Wait for what's running to finish first (the move restarts the app): {}",
            busy.join("; ")
        ));
    }
    let keep_dir = if same_file(&dir, &default) { None } else { Some(dir.display().to_string()) };
    let db = db.inner().clone();
    let (task_id, tasks, _cancel) = crate::packages::begin_task(&state, "db_move_starting", "Preparing")?;
    // From here no other task starts.
    if MOVING.swap(true, Ordering::SeqCst) {
        if let Ok(mut guard) = tasks.lock() {
            guard.remove(&task_id);
        }
        return Err("The database is already being moved.".to_string());
    }
    std::thread::spawn(move || {
        let result = (|| -> Result<String, String> {
            // Nothing is written while the copy is made, nor after, until the
            // restart: the writer stays held to the end.
            let writer = db.conn.lock().map_err(|_| "database connection poisoned".to_string())?;
            if use_existing == Some(true) {
                crate::tasks::set_task_progress(&tasks, task_id, "db_move_check", 0.3, "Checking the database there".to_string());
                check_database(&target)?;
                write_location(&app, &Location { dir: keep_dir, ..Location::default() }).map_err(|e| e.to_string())?;
                finish(&tasks, task_id, &target, "Switching to the database there");
                restart_holding(&app, writer);
            }
            let part = dir.join(format!("{DB_FILE_NAME}.part"));
            let _ = fs::remove_file(&part);
            // The copy's progress: its size against the open file's.
            let total = need.max(1);
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
            // On disk before anything else happens: SQLite doesn't flush a
            // VACUUM INTO, and the old file goes once this one has opened.
            let flushed = copied
                .map_err(|e| format!("Couldn't copy the database there: {e}"))
                .and_then(|_| flush_file(&part).map_err(|e| format!("Couldn't write the copy to disk: {e}")));
            if let Err(e) = flushed {
                let _ = fs::remove_file(&part);
                return Err(e);
            }
            crate::tasks::set_task_progress(&tasks, task_id, "db_move_check", 0.85, "Checking the copy".to_string());
            let (packages, rows) = counts(&writer).map_err(|e| e.to_string())?;
            let copy = check_database(&part);
            if !copy.as_ref().is_ok_and(|c| *c == (packages, rows)) {
                let _ = fs::remove_file(&part);
                return Err(match copy {
                    Err(e) => format!("The copy didn't check out: {e}"),
                    Ok(c) => format!("The copy has {} packages and {} files, not {packages} and {rows}", c.0, c.1),
                });
            }
            // A database already there (the user chose to replace it) is
            // kept beside it, never deleted.
            if target.exists() {
                if let Err(e) = set_aside(&target) {
                    let _ = fs::remove_file(&part);
                    return Err(format!("Couldn't set the database there aside: {e}"));
                }
            }
            if let Err(e) = rename_durably(&part, &target) {
                let _ = fs::remove_file(&part);
                return Err(format!("Couldn't put the copy in place: {e}"));
            }
            let loc = Location {
                dir: keep_dir,
                cleanup: Some(current.display().to_string()),
                cleanup_target: Some(target.display().to_string()),
                cleanup_opens: 0,
            };
            write_location(&app, &loc).map_err(|e| e.to_string())?;
            finish(&tasks, task_id, &target, "Moved: restarting");
            restart_holding(&app, writer);
        })();
        if let Err(e) = result {
            MOVING.store(false, Ordering::SeqCst);
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

fn flush_file(path: &Path) -> std::io::Result<()> {
    fs::OpenOptions::new().write(true).open(path)?.sync_all()
}

/// Renames `from` over `to`, written through to disk before it returns.
fn rename_durably(from: &Path, to: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows::{
            core::PCWSTR,
            Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH},
        };
        let wide = |p: &Path| p.as_os_str().encode_wide().chain(Some(0)).collect::<Vec<u16>>();
        let (a, b) = (wide(from), wide(to));
        unsafe { MoveFileExW(PCWSTR(a.as_ptr()), PCWSTR(b.as_ptr()), MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH) }
            .map_err(std::io::Error::other)
    }
    #[cfg(not(windows))]
    {
        fs::rename(from, to)
    }
}

/// Renames a database (with its -wal and -shm) to `….replaced.db` beside it
/// (`.replaced.2.db`… when taken).
fn set_aside(path: &Path) -> std::io::Result<PathBuf> {
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let mut aside = path.with_file_name(format!("{stem}.replaced.db"));
    let mut n = 2;
    while aside.exists() {
        aside = path.with_file_name(format!("{stem}.replaced.{n}.db"));
        n += 1;
    }
    fs::rename(path, &aside)?;
    for suffix in ["-wal", "-shm"] {
        let from = PathBuf::from(format!("{}{suffix}", path.display()));
        if from.exists() {
            fs::rename(&from, format!("{}{suffix}", aside.display()))?;
        }
    }
    Ok(aside)
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
/// held, so nothing reaches the old file. The process restarts directly:
/// `AppHandle::restart` from this thread only asks the main thread's event
/// loop to exit, which never happens while a command there waits for the
/// writer.
fn restart_holding(app: &tauri::AppHandle, writer: std::sync::MutexGuard<'_, rusqlite::Connection>) -> ! {
    std::thread::sleep(Duration::from_millis(1500));
    let _ = writer.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
    tauri::process::restart(&app.env())
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

#[cfg(test)]
pub(crate) fn synced_by_for_test(dir: &Path) -> Option<&'static str> {
    synced_by(dir)
}
