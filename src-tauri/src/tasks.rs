use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    thread,
};

use anyhow::Result;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use tauri::State;
use zip::ZipArchive;

use crate::{
    config::{load_config_impl, save_config_impl},
    db::{self, Db, DbStats},
    execute::{execute_with_progress, paired_support_paths},
    import::import_manifest,
    fix_var::{apply_fix_var, scan_target_var_for_broken_refs},
    internalize::{apply_internalize, scan_target_var_for_external_refs},
    models::{
        AnalyzeVarDepsResponse, AppConfig, AppState, BackfillSizesRequest, BackfillSizesResponse,
        BrokenRef, BulkImportRequest, BulkImportResponse, DbFindGroup, DbFindRef, DbFindRequest,
        DbFindResponse, DeleteDependencyItem, DeleteDependencyScanRequest,
        DeleteDependencyScanResponse, DependencyCheckMatch, DependencyCheckRequest,
        DependencyCheckResponse, DependencyDeepRef, DependencyLocalFile, DependencyUser,
        DownloadDepItem, DownloadDepSource, DownloadLinkRow, DownloadVarItem,
        DownloadVarsResponse, ExecuteResponse, ExportScenesResponse, ExternalRefGroup, FixDirective,
        FixReport, ImportLinksResult,
        InternalizeReport, InternalizeSelection, ProgressPayload, ReclaimCandidate,
        SceneImageExportResult,
        ReclaimScanRequest, ReclaimScanResponse, ResourceDuplicateRef, ResourceListFilterOptions,
        ResourceListFilters, ResourceListItem, ResourceListPage, ResourceRef, ScanResponse,
        ScanTaskRequest, TaskHandle, UniqueResource, UniqueResourceSource, UniqueResourcesRequest,
        VarSourceInfo,
        UniqueResourcesResponse, VamPreviewImage, VamPreviewResponse, VarFileStats,
        VarPackageFilterOptions, VarPackageFilters, VarPackageListItem, VarPackagePage,
        VarPackagesFolderCache, VarResourceEntry,
    },
    scan::{
        build_scan_response, cache_scan_result, extract_vaj_support_paths,
        load_cached_scan_with_target, same_stem_texture_paths,
        scan_directory_with_target_with_progress_db,
    },
    utils::{format_bytes, ScanDepth},
};

const DB_FIND_DB_MATCHES_CAP: usize = 256;

fn image_mime_type(path: &str) -> Option<&'static str> {
    Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .and_then(|ext| match ext.as_str() {
            "jpg" | "jpeg" => Some("image/jpeg"),
            "png" => Some("image/png"),
            _ => None,
        })
}

fn preview_candidate_paths(path: &str) -> Vec<String> {
    if image_mime_type(path).is_some() {
        return vec![path.replace('\\', "/")];
    }
    if Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("vam"))
        .unwrap_or(false)
    {
        return same_stem_texture_paths(path).into_iter().collect();
    }
    Vec::new()
}

fn parse_png_dimensions(raw: &[u8]) -> Option<(u32, u32)> {
    if raw.len() < 24 || &raw[..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let width = u32::from_be_bytes(raw.get(16..20)?.try_into().ok()?);
    let height = u32::from_be_bytes(raw.get(20..24)?.try_into().ok()?);
    Some((width, height))
}

fn parse_jpeg_dimensions(raw: &[u8]) -> Option<(u32, u32)> {
    if raw.len() < 4 || raw[0] != 0xFF || raw[1] != 0xD8 {
        return None;
    }

    let mut index = 2usize;
    while index + 9 < raw.len() {
        if raw[index] != 0xFF {
            index += 1;
            continue;
        }
        let marker = raw[index + 1];
        index += 2;

        while index < raw.len() && raw[index] == 0xFF {
            index += 1;
        }

        if marker == 0xD8 || marker == 0xD9 || (0xD0..=0xD7).contains(&marker) {
            continue;
        }

        if index + 2 > raw.len() {
            return None;
        }

        let segment_len = u16::from_be_bytes(raw.get(index..index + 2)?.try_into().ok()?) as usize;
        if segment_len < 2 || index + segment_len > raw.len() {
            return None;
        }

        if matches!(
            marker,
            0xC0 | 0xC1
                | 0xC2
                | 0xC3
                | 0xC5
                | 0xC6
                | 0xC7
                | 0xC9
                | 0xCA
                | 0xCB
                | 0xCD
                | 0xCE
                | 0xCF
        ) {
            if segment_len < 7 {
                return None;
            }
            let height = u16::from_be_bytes(raw.get(index + 3..index + 5)?.try_into().ok()?) as u32;
            let width = u16::from_be_bytes(raw.get(index + 5..index + 7)?.try_into().ok()?) as u32;
            return Some((width, height));
        }

        index += segment_len;
    }

    None
}

fn image_dimensions(mime_type: &str, raw: &[u8]) -> Option<(u32, u32)> {
    match mime_type {
        "image/png" => parse_png_dimensions(raw),
        "image/jpeg" => parse_jpeg_dimensions(raw),
        _ => None,
    }
}

fn is_large_preview(width: Option<u32>, height: Option<u32>) -> bool {
    width.unwrap_or(0) > 2048 || height.unwrap_or(0) > 2048
}

fn load_preview_image_data_impl(
    package_path: &str,
    internal_path: &str,
) -> Result<(String, String), String> {
    let archive_file = fs::File::open(package_path).map_err(|err| err.to_string())?;
    let mut archive = ZipArchive::new(archive_file).map_err(|err| err.to_string())?;
    let Some(mime_type) = image_mime_type(internal_path) else {
        return Err(format!("Unsupported preview image type: {internal_path}"));
    };
    let mut file = archive
        .by_name(internal_path)
        .map_err(|err| err.to_string())?;
    let mut raw = Vec::new();
    file.read_to_end(&mut raw).map_err(|err| err.to_string())?;
    Ok((
        mime_type.to_string(),
        format!("data:{mime_type};base64,{}", BASE64.encode(raw)),
    ))
}

#[tauri::command]
pub(crate) fn get_vam_preview(
    package_id: String,
    package_path: String,
    vam_path: String,
) -> Result<VamPreviewResponse, String> {
    let archive_file = fs::File::open(&package_path).map_err(|err| err.to_string())?;
    let mut archive = ZipArchive::new(archive_file).map_err(|err| err.to_string())?;
    let mut images = Vec::new();

    for candidate in preview_candidate_paths(&vam_path) {
        let Some(mime_type) = image_mime_type(&candidate) else {
            continue;
        };
        let Ok(mut file) = archive.by_name(&candidate) else {
            continue;
        };
        let mut raw = Vec::new();
        file.read_to_end(&mut raw).map_err(|err| err.to_string())?;
        let dimensions = image_dimensions(mime_type, &raw);
        images.push(VamPreviewImage {
            internal_path: candidate,
            mime_type: mime_type.to_string(),
            size: raw.len() as u64,
            width: dimensions.map(|(width, _)| width),
            height: dimensions.map(|(_, height)| height),
            data_url: if is_large_preview(
                dimensions.map(|(width, _)| width),
                dimensions.map(|(_, height)| height),
            ) {
                None
            } else {
                Some(format!("data:{mime_type};base64,{}", BASE64.encode(raw)))
            },
        });
    }

    Ok(VamPreviewResponse {
        package_id,
        vam_path,
        images,
    })
}

// (async) so the decompress + base64 of a multi-MB image runs off the main
// thread. The VAR Packages image grid calls this once per visible tile, and a
// plain #[tauri::command] freezes the window for the duration of each one.
#[tauri::command(async)]
pub(crate) fn load_preview_image_data(
    package_path: String,
    internal_path: String,
) -> Result<String, String> {
    let (_, data_url) = load_preview_image_data_impl(&package_path, &internal_path)?;
    Ok(data_url)
}

pub(crate) fn set_task_progress(
    tasks: &Arc<Mutex<HashMap<u64, ProgressPayload>>>,
    task_id: u64,
    phase: &str,
    progress: f64,
    message: impl Into<String>,
) {
    if let Ok(mut guard) = tasks.lock() {
        if let Some(task) = guard.get_mut(&task_id) {
            task.phase = phase.to_string();
            task.progress = progress.clamp(0.0, 1.0);
            task.message = message.into();
        }
    }
}

fn finish_scan_task(
    tasks: &Arc<Mutex<HashMap<u64, ProgressPayload>>>,
    task_id: u64,
    result: Result<ScanResponse>,
) {
    if let Ok(mut guard) = tasks.lock() {
        if let Some(task) = guard.get_mut(&task_id) {
            task.done = true;
            task.progress = 1.0;
            match result {
                Ok(payload) => {
                    task.phase = "scan_complete".to_string();
                    task.message = "Scan completed".to_string();
                    task.scan_result = Some(payload);
                    task.error = None;
                }
                Err(err) => {
                    task.phase = "scan_failed".to_string();
                    task.message = "Scan failed".to_string();
                    task.error = Some(err.to_string());
                }
            }
        }
    }
}

fn finish_execute_task(
    tasks: &Arc<Mutex<HashMap<u64, ProgressPayload>>>,
    task_id: u64,
    result: Result<ExecuteResponse>,
) {
    if let Ok(mut guard) = tasks.lock() {
        if let Some(task) = guard.get_mut(&task_id) {
            task.done = true;
            task.progress = 1.0;
            match result {
                Ok(payload) => {
                    task.phase = "execute_complete".to_string();
                    task.message = "Dedup completed".to_string();
                    task.execute_result = Some(payload);
                    task.error = None;
                }
                Err(err) => {
                    task.phase = "execute_failed".to_string();
                    task.message = "Dedup failed".to_string();
                    task.error = Some(err.to_string());
                }
            }
        }
    }
}

fn finish_bulk_import_task(
    tasks: &Arc<Mutex<HashMap<u64, ProgressPayload>>>,
    task_id: u64,
    result: Result<BulkImportResponse>,
) {
    if let Ok(mut guard) = tasks.lock() {
        if let Some(task) = guard.get_mut(&task_id) {
            task.done = true;
            task.progress = 1.0;
            match result {
                Ok(payload) => {
                    task.phase = "bulk_import_complete".to_string();
                    task.message = "Manifest import completed".to_string();
                    task.bulk_import_result = Some(payload);
                    task.error = None;
                }
                Err(err) => {
                    task.phase = "bulk_import_failed".to_string();
                    task.message = "Manifest import failed".to_string();
                    task.error = Some(err.to_string());
                }
            }
        }
    }
}

#[tauri::command]
pub(crate) fn load_config(app: tauri::AppHandle) -> AppConfig {
    load_config_impl(&app)
}

#[tauri::command]
pub(crate) fn pick_folder() -> Option<String> {
    rfd::FileDialog::new()
        .pick_folder()
        .map(|path| path.display().to_string())
}

/// Multi-select folder picker. Empty when the dialog is cancelled.
#[tauri::command]
pub(crate) fn pick_folders() -> Vec<String> {
    rfd::FileDialog::new()
        .pick_folders()
        .unwrap_or_default()
        .into_iter()
        .map(|path| path.display().to_string())
        .collect()
}

#[tauri::command]
pub(crate) fn pick_var_file() -> Option<String> {
    rfd::FileDialog::new()
        .add_filter("VAR package", &["var"])
        .pick_file()
        .map(|path| path.display().to_string())
}

#[tauri::command]
pub(crate) fn pick_manifest_file() -> Option<String> {
    rfd::FileDialog::new()
        .add_filter("Manifest text", &["txt"])
        .pick_file()
        .map(|path| path.display().to_string())
}

#[tauri::command]
pub(crate) fn pick_save_file(file_name: String) -> Option<String> {
    rfd::FileDialog::new()
        .set_file_name(&file_name)
        .save_file()
        .map(|path| path.display().to_string())
}

#[tauri::command]
pub(crate) fn path_exists(path: String) -> bool {
    Path::new(&path).exists()
}

#[tauri::command]
pub(crate) fn export_var_resource(
    package_path: String,
    internal_path: String,
    output_path: String,
) -> Result<(), String> {
    let archive_file = fs::File::open(&package_path).map_err(|err| err.to_string())?;
    let mut archive = ZipArchive::new(archive_file).map_err(|err| err.to_string())?;
    let mut file = archive
        .by_name(&internal_path)
        .map_err(|err| err.to_string())?;
    let mut raw = Vec::new();
    file.read_to_end(&mut raw).map_err(|err| err.to_string())?;

    let output = Path::new(&output_path);
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    fs::write(output, raw).map_err(|err| err.to_string())
}

#[tauri::command]
pub(crate) fn export_vam_bundle(
    package_path: String,
    internal_path: String,
    output_dir: String,
) -> Result<usize, String> {
    let archive_file = fs::File::open(&package_path).map_err(|err| err.to_string())?;
    let mut archive = ZipArchive::new(archive_file).map_err(|err| err.to_string())?;

    let base_dir = Path::new(&internal_path).parent().unwrap_or(Path::new(""));

    let mut paths_to_export = vec![internal_path.clone()];
    paths_to_export.extend(paired_support_paths(&internal_path));

    let vaj_paths: Vec<String> = paths_to_export
        .iter()
        .filter(|p| p.to_ascii_lowercase().ends_with(".vaj"))
        .cloned()
        .collect();
    for vaj_path in &vaj_paths {
        if let Ok(mut entry) = archive.by_name(vaj_path) {
            let mut raw = Vec::new();
            if entry.read_to_end(&mut raw).is_ok() {
                for texture_path in extract_vaj_support_paths(vaj_path, &raw) {
                    if !paths_to_export.contains(&texture_path) {
                        paths_to_export.push(texture_path);
                    }
                }
            }
        }
    }

    let folder_name = Path::new(&internal_path)
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("vam-export");
    let out = Path::new(&output_dir).join(folder_name);
    fs::create_dir_all(&out).map_err(|err| err.to_string())?;

    let mut count = 0usize;
    for zip_path in &paths_to_export {
        if let Ok(mut entry) = archive.by_name(zip_path) {
            let rel = Path::new(zip_path)
                .strip_prefix(base_dir)
                .unwrap_or(Path::new(zip_path));
            let dest = out.join(rel);
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent).map_err(|err| err.to_string())?;
            }
            let mut raw = Vec::new();
            entry.read_to_end(&mut raw).map_err(|err| err.to_string())?;
            fs::write(&dest, raw).map_err(|err| err.to_string())?;
            count += 1;
        }
    }
    Ok(count)
}

#[tauri::command]
pub(crate) fn show_in_explorer(path: String) -> Result<(), String> {
    let target = Path::new(&path);
    if !target.exists() {
        return Err(format!("Path does not exist: {path}"));
    }

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        // Two Windows Explorer quirks handled here:
        //  1. `explorer /select,<file>` returns exit code 1 even on success, so
        //     we only treat a spawn failure as an error, not the exit status.
        //  2. Explorer's command-line parser chokes on the quoting Rust's normal
        //     `.arg()` escaping applies to a path with spaces — it drops the
        //     `/select` and opens a default folder. `raw_arg` appends the string
        //     verbatim, so we quote the (backslash-normalized) path ourselves:
        //     `explorer /select,"D:\a folder with spaces\file.var"`.
        let win_path = target.to_string_lossy().replace('/', "\\");
        let spawn_result = if target.is_file() {
            Command::new("explorer")
                .raw_arg(format!("/select,\"{win_path}\""))
                .status()
        } else {
            Command::new("explorer")
                .raw_arg(format!("\"{win_path}\""))
                .status()
        };
        spawn_result.map_err(|err| err.to_string())?;
        Ok(())
    }

    #[cfg(not(target_os = "windows"))]
    {
        let parent = if target.is_dir() {
            target
        } else {
            target.parent().unwrap_or(target)
        };

        let status = if cfg!(target_os = "macos") {
            Command::new("open").arg(parent).status()
        } else {
            Command::new("xdg-open").arg(parent).status()
        }
        .map_err(|err| err.to_string())?;

        if status.success() {
            Ok(())
        } else {
            Err(format!("File manager exited with status {status}"))
        }
    }
}

#[tauri::command]
pub(crate) fn save_config(app: tauri::AppHandle, config: AppConfig) -> Result<(), String> {
    save_config_impl(&app, &config).map_err(|err| err.to_string())
}

/// Trim, drop empties, and convert the UI-supplied additional scan folders to
/// `PathBuf`s. Empty input yields an empty vec, which keeps the cache key and
/// scan roots identical to the historical single-folder behavior.
fn parse_additional_dirs(dirs: &[String]) -> Vec<PathBuf> {
    dirs.iter()
        .map(|dir| dir.trim())
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .collect()
}

#[tauri::command]
pub(crate) fn start_scan_task(
    request: ScanTaskRequest,
    state: State<'_, AppState>,
    db: State<'_, Db>,
) -> Result<TaskHandle, String> {
    let task_id = state.next_task_id.fetch_add(1, Ordering::SeqCst) + 1;
    {
        let mut guard = state
            .tasks
            .lock()
            .map_err(|_| "task state poisoned".to_string())?;
        guard.insert(
            task_id,
            new_progress_payload("scan_starting", "Starting scan"),
        );
    }

    let tasks = Arc::clone(&state.tasks);
    let scan_cache = Arc::clone(&state.scan_cache);
    let db = db.inner().clone();
    let skip_db = request.skip_db;
    thread::spawn(move || {
        let additional_dirs = parse_additional_dirs(&request.additional_input_dirs);
        let result = (|| -> Result<ScanResponse> {
            let target_var_path = request
                .target_var_path
                .as_deref()
                .filter(|path| !path.trim().is_empty())
                .map(Path::new);
            let scanned = scan_directory_with_target_with_progress_db(
                Path::new(&request.input_dir),
                &additional_dirs,
                target_var_path,
                if skip_db { None } else { Some(&db) },
                request.index_only,
                |progress, message| {
                    set_task_progress(&tasks, task_id, "scanning", progress.min(1.0), message);
                },
            )?;
            cache_scan_result(
                &scan_cache,
                Path::new(&request.input_dir),
                &additional_dirs,
                target_var_path,
                &scanned,
            )?;
            Ok(build_scan_response(&scanned))
        })();

        finish_scan_task(&tasks, task_id, result);
    });

    Ok(TaskHandle { id: task_id })
}

#[tauri::command]
pub(crate) fn start_execute_task(
    request: crate::models::ExecuteRequest,
    state: State<'_, AppState>,
    db: State<'_, Db>,
) -> Result<TaskHandle, String> {
    let task_id = state.next_task_id.fetch_add(1, Ordering::SeqCst) + 1;
    {
        let mut guard = state
            .tasks
            .lock()
            .map_err(|_| "task state poisoned".to_string())?;
        guard.insert(
            task_id,
            new_progress_payload("execute_starting", "Starting dedupe"),
        );
    }

    let tasks = Arc::clone(&state.tasks);
    let scan_cache = Arc::clone(&state.scan_cache);
    let db = db.inner().clone();
    thread::spawn(move || {
        let target_var_path = request
            .target_var_path
            .as_deref()
            .filter(|path| !path.trim().is_empty())
            .map(Path::new);
        let additional_dirs = parse_additional_dirs(&request.additional_input_dirs);
        let cached_scan = load_cached_scan_with_target(
            &scan_cache,
            Path::new(&request.input_dir),
            &additional_dirs,
            target_var_path,
        )
        .map_err(|err| err.to_string())
        .ok()
        .flatten();
        let result = execute_with_progress(request, cached_scan, Some(&db), |progress, message| {
            let phase = if progress < 0.50 {
                "execute_scanning"
            } else {
                "execute_writing"
            };
            set_task_progress(&tasks, task_id, phase, progress, message);
        });

        finish_execute_task(&tasks, task_id, result);
    });

    Ok(TaskHandle { id: task_id })
}

#[tauri::command]
pub(crate) fn start_bulk_import_task(
    request: BulkImportRequest,
    state: State<'_, AppState>,
    db: State<'_, Db>,
) -> Result<TaskHandle, String> {
    let task_id = state.next_task_id.fetch_add(1, Ordering::SeqCst) + 1;
    {
        let mut guard = state
            .tasks
            .lock()
            .map_err(|_| "task state poisoned".to_string())?;
        guard.insert(
            task_id,
            new_progress_payload("bulk_import_starting", "Starting manifest import"),
        );
    }

    let tasks = Arc::clone(&state.tasks);
    let db = db.inner().clone();
    thread::spawn(move || {
        let result = import_manifest(
            Path::new(&request.manifest_path),
            &db,
            |progress, message| {
                set_task_progress(&tasks, task_id, "bulk_importing", progress.min(1.0), message);
            },
        );

        finish_bulk_import_task(&tasks, task_id, result);
    });

    Ok(TaskHandle { id: task_id })
}

#[tauri::command]
pub(crate) fn get_task_progress(
    task_id: u64,
    state: State<'_, AppState>,
) -> Result<ProgressPayload, String> {
    let guard = state
        .tasks
        .lock()
        .map_err(|_| "task state poisoned".to_string())?;
    guard
        .get(&task_id)
        .cloned()
        .ok_or_else(|| format!("task {task_id} not found"))
}

#[tauri::command]
pub(crate) fn clear_task(task_id: u64, state: State<'_, AppState>) -> Result<(), String> {
    let mut guard = state
        .tasks
        .lock()
        .map_err(|_| "task state poisoned".to_string())?;
    guard.remove(&task_id);
    drop(guard);
    if let Ok(mut cancels) = state.cancellations.lock() {
        cancels.remove(&task_id);
    }
    if let Ok(mut pauses) = state.pauses.lock() {
        pauses.remove(&task_id);
    }
    Ok(())
}

/// Stop a download but keep its partial file, so starting it again resumes
/// (status `paused`). For tasks without a pause flag it is a plain cancel.
#[tauri::command]
pub(crate) fn pause_task(task_id: u64, state: State<'_, AppState>) -> Result<bool, String> {
    if let Some(flag) = state.pauses.lock().ok().and_then(|p| p.get(&task_id).cloned()) {
        flag.store(true, Ordering::SeqCst);
    }
    cancel_task(task_id, state)
}

/// Flip the cancel flag for `task_id` if one was registered. Tasks that opt
/// in (currently the backfill-sizes task) poll this flag at safe points and
/// bail out gracefully. A no-op for tasks without a cancel flag — those run
/// to completion.
#[tauri::command]
pub(crate) fn cancel_task(task_id: u64, state: State<'_, AppState>) -> Result<bool, String> {
    let cancels = state
        .cancellations
        .lock()
        .map_err(|_| "cancellation state poisoned".to_string())?;
    if let Some(flag) = cancels.get(&task_id) {
        flag.store(true, Ordering::SeqCst);
        Ok(true)
    } else {
        Ok(false)
    }
}

#[tauri::command]
pub(crate) fn format_bytes_command(size: u64) -> String {
    format_bytes(size)
}

/// Settings > About: the version (Cargo.toml is the only place it's set) and
/// the release notes, built into the exe so they match this build.
#[tauri::command]
pub(crate) fn app_info() -> serde_json::Value {
    serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "changelog": include_str!("../../CHANGELOG.md"),
    })
}

const PREVIEW_IMAGE_EXTS: &[&str] = &["jpg", "jpeg", "png"];

/// Lowercased, forward-slashed. Compared against a lowercased path, so it also
/// matches the `Saves/Scene/` casing some VARs ship.
const SCENE_IMAGE_PREFIX: &str = "saves/scene/";

fn is_preview_image(normalized_lower_ext: &str) -> bool {
    PREVIEW_IMAGE_EXTS.contains(&normalized_lower_ext)
}

/// Picks the scene image entry inside a `.var` — the first `.jpg/.jpeg/.png`
/// under `Saves/scene/`, which is what the grid thumbnail shows.
///
/// Deliberately has NO fallback to "the largest embedded image": a clothing or
/// hair package's preview is not a scene image, and exporting one produced a
/// side-car the user never asked for. A VAR without a scene image yields None
/// and nothing is written.
///
/// The prefix is matched case-insensitively — VARs ship both `Saves/scene/` and
/// `Saves/Scene/`, and with the fallback gone a case mismatch would silently
/// export nothing. Returns the raw zip entry name.
fn choose_scene_image(archive: &mut ZipArchive<fs::File>) -> Option<String> {
    for i in 0..archive.len() {
        let entry = match archive.by_index(i) {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_string();
        let normalized = name.replace('\\', "/");
        let ext = normalized.rsplit('.').next().unwrap_or("").to_lowercase();
        if !is_preview_image(&ext) {
            continue;
        }
        if normalized
            .to_ascii_lowercase()
            .starts_with(SCENE_IMAGE_PREFIX)
        {
            return Some(name);
        }
    }
    None
}

/// Writes a VAR's scene image next to it as `<var stem>.<image ext>`.
///
/// Only a real `Saves/scene/` image is ever written — a VAR without one reports
/// `no_image` and nothing lands on disk. Skips (does not overwrite) an existing
/// side-car unless `overwrite`. Never touches the `.var`.
fn export_scene_image_impl(var_path: &Path, overwrite: bool) -> SceneImageExportResult {
    let failed = |detail: String| SceneImageExportResult {
        status: "failed".to_string(),
        output_path: None,
        detail: Some(detail),
    };

    let file = match fs::File::open(var_path) {
        Ok(f) => f,
        Err(e) => return failed(format!("could not open: {e}")),
    };
    let mut archive = match ZipArchive::new(file) {
        Ok(a) => a,
        Err(e) => return failed(format!("not a readable VAR: {e}")),
    };

    let Some(entry_name) = choose_scene_image(&mut archive) else {
        return SceneImageExportResult {
            status: "no_image".to_string(),
            output_path: None,
            detail: None,
        };
    };

    // `<var>.var` -> `<var>.<image ext>` in the same folder. The stem keeps the
    // full `Creator.Package.Version` so the image sorts next to its VAR.
    let ext = entry_name
        .rsplit('.')
        .next()
        .unwrap_or("jpg")
        .to_ascii_lowercase();
    let ext = if ext == "jpeg" { "jpg".to_string() } else { ext };
    let stem = var_path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    if stem.is_empty() {
        return failed("bad .var filename".to_string());
    }
    let out_path = match var_path.parent() {
        Some(dir) => dir.join(format!("{stem}.{ext}")),
        None => return failed("no parent directory".to_string()),
    };

    if out_path.exists() && !overwrite {
        return SceneImageExportResult {
            status: "exists".to_string(),
            output_path: Some(out_path.display().to_string()),
            detail: None,
        };
    }

    let mut buf = Vec::new();
    match archive.by_name(&entry_name) {
        Ok(mut zip_file) => {
            if let Err(e) = zip_file.read_to_end(&mut buf) {
                return failed(format!("could not read image: {e}"));
            }
        }
        Err(e) => return failed(format!("could not read image: {e}")),
    }
    // Write to a temp then rename, so a crash mid-write can't leave a partial
    // image that later looks "already exists".
    let tmp_path = out_path.with_extension(format!("{ext}.vamvd-tmp"));
    if let Err(e) = fs::write(&tmp_path, &buf) {
        return failed(format!("could not write: {e}"));
    }
    if let Err(e) = fs::rename(&tmp_path, &out_path) {
        let _ = fs::remove_file(&tmp_path);
        return failed(format!("could not finalize: {e}"));
    }

    SceneImageExportResult {
        status: "exported".to_string(),
        output_path: Some(out_path.display().to_string()),
        detail: None,
    }
}

/// Extract one VAR's preview image next to it (VAR Packages → per-card action).
#[tauri::command]
pub(crate) fn export_var_scene_image(
    package_path: String,
    overwrite: Option<bool>,
) -> Result<SceneImageExportResult, String> {
    let path = Path::new(&package_path);
    if !path.is_file() {
        return Err("that package is not on disk".to_string());
    }
    Ok(export_scene_image_impl(path, overwrite.unwrap_or(false)))
}

/// Extract preview images for every VAR under a folder (VAR Packages → batch).
/// Respects the page's scan depth so it matches the listed set; skips existing
/// side-cars unless `overwrite`.
#[tauri::command]
pub(crate) fn start_export_scene_images_task(
    input_dir: String,
    additional_input_dirs: Option<Vec<String>>,
    deep_scan: Option<bool>,
    overwrite: Option<bool>,
    state: State<'_, AppState>,
) -> Result<TaskHandle, String> {
    let dir = Path::new(&input_dir);
    if !dir.is_dir() {
        return Err(format!("not a directory: {}", dir.display()));
    }
    let additional_paths: Vec<PathBuf> = additional_input_dirs
        .unwrap_or_default()
        .iter()
        .map(|d| d.trim())
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .collect();
    let roots = crate::scan::scan_roots(dir, &additional_paths);
    let depth = ScanDepth::from_deep(deep_scan.unwrap_or(true));
    let overwrite = overwrite.unwrap_or(false);

    let task_id = state.next_task_id.fetch_add(1, Ordering::SeqCst) + 1;
    {
        let mut guard = state
            .tasks
            .lock()
            .map_err(|_| "task state poisoned".to_string())?;
        guard.insert(
            task_id,
            new_progress_payload("export_scenes_starting", "Preparing"),
        );
    }
    let cancel_flag = Arc::new(AtomicBool::new(false));
    {
        let mut cancels = state
            .cancellations
            .lock()
            .map_err(|_| "cancellation state poisoned".to_string())?;
        cancels.insert(task_id, Arc::clone(&cancel_flag));
    }

    let tasks = Arc::clone(&state.tasks);
    thread::spawn(move || {
        let result = run_export_scene_images(&tasks, task_id, &roots, depth, overwrite, cancel_flag);
        finish_export_scene_images(&tasks, task_id, result);
    });
    Ok(TaskHandle { id: task_id })
}

fn run_export_scene_images(
    tasks: &Arc<Mutex<HashMap<u64, ProgressPayload>>>,
    task_id: u64,
    roots: &[PathBuf],
    depth: ScanDepth,
    overwrite: bool,
    cancel: Arc<AtomicBool>,
) -> Result<ExportScenesResponse> {
    let files = crate::utils::collect_var_files_by_root_with_depth(roots, depth)?;
    let total = files.len().max(1);
    let mut resp = ExportScenesResponse::default();

    for (index, (_root, entry)) in files.iter().enumerate() {
        if cancel.load(Ordering::SeqCst) {
            resp.was_cancelled = true;
            break;
        }
        if index % 20 == 0 || index + 1 == total {
            set_task_progress(
                tasks,
                task_id,
                "export_scenes_running",
                index as f64 / total as f64,
                format!("Exporting images ({}/{})", index + 1, total),
            );
        }
        resp.scanned += 1;
        let outcome = export_scene_image_impl(&entry.path, overwrite);
        match outcome.status.as_str() {
            "exported" => resp.exported += 1,
            "exists" => resp.skipped_exists += 1,
            "no_image" => resp.no_image += 1,
            _ => {
                resp.failed += 1;
                if resp.notes.len() < 20 {
                    let name = entry
                        .path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("");
                    resp.notes.push(format!(
                        "{name}: {}",
                        outcome.detail.unwrap_or_else(|| "failed".to_string())
                    ));
                }
            }
        }
    }
    Ok(resp)
}

fn finish_export_scene_images(
    tasks: &Arc<Mutex<HashMap<u64, ProgressPayload>>>,
    task_id: u64,
    result: Result<ExportScenesResponse>,
) {
    if let Ok(mut guard) = tasks.lock() {
        if let Some(task) = guard.get_mut(&task_id) {
            task.done = true;
            task.progress = 1.0;
            match result {
                Ok(resp) => {
                    task.phase = if resp.was_cancelled {
                        "export_scenes_cancelled".to_string()
                    } else {
                        "export_scenes_complete".to_string()
                    };
                    task.message = format!("Exported {} image(s)", resp.exported);
                    task.error = None;
                    task.export_scenes_result = Some(resp);
                }
                Err(err) => {
                    task.phase = "export_scenes_failed".to_string();
                    task.message = "Failed".to_string();
                    task.error = Some(err.to_string());
                }
            }
        }
    }
}

#[tauri::command]
pub(crate) fn get_var_file_stats(package_path: String) -> Result<VarFileStats, String> {
    let path = Path::new(&package_path);
    let metadata = fs::metadata(path).map_err(|e| e.to_string())?;

    let modified_ms = metadata.modified().ok().and_then(|t| {
        t.duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_millis() as u64)
    });

    let size_bytes = metadata.len();

    let (scene_image_path, scene_image_data) = if let Ok(file) = fs::File::open(path) {
        if let Ok(mut archive) = ZipArchive::new(file) {
            // Thumbnail parity: only the Saves/scene image. The exporter now
            // uses the same predicate, so a card with no thumbnail is exactly a
            // card that exports nothing.
            let img_name = choose_scene_image(&mut archive);
            drop(archive);
            let img_name_opt = img_name;
            let img_path = img_name_opt.as_ref().map(|n| n.replace('\\', "/"));
            let img_data = img_name_opt.and_then(|name| {
                if let Ok(file) = fs::File::open(path) {
                    if let Ok(mut archive) = ZipArchive::new(file) {
                        if let Ok(mut zip_file) = archive.by_name(&name) {
                            let ext = name.rsplit('.').next().unwrap_or("");
                            let mime = if ext.to_lowercase() == "png" {
                                "image/png"
                            } else {
                                "image/jpeg"
                            };
                            let mut buf = Vec::new();
                            if zip_file.read_to_end(&mut buf).is_ok() {
                                return Some(format!("data:{mime};base64,{}", BASE64.encode(&buf)));
                            }
                        }
                    }
                }
                None
            });
            (img_path, img_data)
        } else {
            (None, None)
        }
    } else {
        (None, None)
    };

    Ok(VarFileStats {
        modified_ms,
        size_bytes,
        scene_image_path,
        scene_image_data,
    })
}

// `(async)` runs this off the webview's main thread on Tauri's blocking pool.
// The body is a blocking SQLite read (`SELECT COUNT(*)` over `packages` and the
// potentially multi-million-row `resources` table plus a `lock()` on the shared
// connection); as a plain sync command it executed on the main thread and froze
// the whole UI while it ran — most visibly when opening the Build Database page.
#[tauri::command(async)]
pub(crate) fn get_database_stats(
    app: tauri::AppHandle,
    db: State<'_, Db>,
) -> Result<DbStats, String> {
    db::database_stats(&app, db.inner()).map_err(|err| err.to_string())
}

#[tauri::command]
pub(crate) fn set_creator_flag(
    creator_name: String,
    flag: i32,
    db: State<'_, Db>,
) -> Result<(), String> {
    let conn = db
        .conn
        .lock()
        .map_err(|_| "database connection poisoned".to_string())?;
    db::set_creator_flag(&conn, &creator_name, flag).map_err(|err| err.to_string())
}

#[tauri::command]
pub(crate) fn get_creator_flag(
    creator_name: String,
    db: State<'_, Db>,
) -> Result<i32, String> {
    let conn = db.read().map_err(|err| err.to_string())?;
    db::get_creator_flag(&conn, &creator_name).map_err(|err| err.to_string())
}

/// Returns the full set of blocked creator names so the UI can mirror the
/// backend's scan-time filter immediately (without re-scanning) — e.g. after
/// the user right-clicks a candidate and picks "Disable creator", every other
/// already-rendered row from that creator should also disappear.
#[tauri::command]
pub(crate) fn list_blocked_creators(db: State<'_, Db>) -> Result<Vec<String>, String> {
    let conn = db.read().map_err(|err| err.to_string())?;
    let mut names: Vec<String> = db::get_blocked_creator_names(&conn)
        .map_err(|err| err.to_string())?
        .into_iter()
        .collect();
    names.sort();
    Ok(names)
}

// (async): the write path blocks on the writer mutex, which a Build Database
// persist can hold for minutes — a plain sync command would park the main
// thread and freeze the UI, the same failure get_database_stats hit above.
#[tauri::command(async)]
pub(crate) fn set_package_flag(
    package_id: String,
    flag: i32,
    db: State<'_, Db>,
) -> Result<(), String> {
    let conn = db
        .conn
        .lock()
        .map_err(|_| "database connection poisoned".to_string())?;
    db::set_package_flag(&conn, &package_id, flag).map_err(|err| err.to_string())
}

#[tauri::command(async)]
pub(crate) fn get_package_flag(package_id: String, db: State<'_, Db>) -> Result<i32, String> {
    let conn = db.read().map_err(|err| err.to_string())?;
    db::get_package_flag(&conn, &package_id).map_err(|err| err.to_string())
}

/// Full favorite-package id set so the UI can mirror stars everywhere (list
/// rows, cards, details header, Clean VARs picks) without per-row calls.
#[tauri::command(async)]
pub(crate) fn list_favorite_packages(db: State<'_, Db>) -> Result<Vec<String>, String> {
    let conn = db.read().map_err(|err| err.to_string())?;
    let mut ids: Vec<String> = db::get_favorite_package_ids(&conn)
        .map_err(|err| err.to_string())?
        .into_iter()
        .collect();
    ids.sort();
    Ok(ids)
}

/// Fix Missing: mark a package as a preferred replacement source (1), one to
/// avoid (-1), or neither (0). Async for the same reason as set_package_flag.
#[tauri::command(async)]
pub(crate) fn set_replacement_pref(package_id: String, pref: i32, db: State<'_, Db>) -> Result<(), String> {
    let conn = db
        .conn
        .lock()
        .map_err(|_| "database connection poisoned".to_string())?;
    db::set_replacement_pref(&conn, &package_id, pref).map_err(|err| err.to_string())
}

/// Every replacement preference, as (package id, 1 or -1) pairs.
#[tauri::command(async)]
pub(crate) fn list_replacement_prefs(db: State<'_, Db>) -> Result<Vec<(String, i32)>, String> {
    let conn = db.read().map_err(|err| err.to_string())?;
    db::get_replacement_prefs(&conn).map_err(|err| err.to_string())
}

/// Favorite-creator counterpart to `list_blocked_creators` — until now
/// `get_favorite_creator_names` was only consumed inside the reclaim scan.
#[tauri::command(async)]
pub(crate) fn list_favorite_creators(db: State<'_, Db>) -> Result<Vec<String>, String> {
    let conn = db.read().map_err(|err| err.to_string())?;
    let mut names: Vec<String> = db::get_favorite_creator_names(&conn)
        .map_err(|err| err.to_string())?
        .into_iter()
        .collect();
    names.sort();
    Ok(names)
}

/// Wipe every indexed package / resource / creator and reset the FTS path
/// index. Async + progress-reporting so the Settings UI can show a real
/// progress bar instead of a frozen "Clearing…" line. The actual work runs
/// on a background thread; the UI polls progress via `get_task_progress`.
#[tauri::command]
pub(crate) fn clear_database(
    state: State<'_, AppState>,
    db: State<'_, Db>,
) -> Result<TaskHandle, String> {
    let task_id = state.next_task_id.fetch_add(1, Ordering::SeqCst) + 1;
    {
        let mut guard = state
            .tasks
            .lock()
            .map_err(|_| "task state poisoned".to_string())?;
        guard.insert(
            task_id,
            new_progress_payload("clear_db_starting", "Starting database clear"),
        );
    }

    let tasks = Arc::clone(&state.tasks);
    let db = db.inner().clone();
    thread::spawn(move || {
        let tasks_for_cb = Arc::clone(&tasks);
        let result = db::clear_all(&db, move |phase, progress, message| {
            set_task_progress(&tasks_for_cb, task_id, phase, progress, message);
        })
        .map_err(|err| err.to_string());
        finish_clear_database_task(&tasks, task_id, result);
    });

    Ok(TaskHandle { id: task_id })
}

fn finish_clear_database_task(
    tasks: &Arc<Mutex<HashMap<u64, ProgressPayload>>>,
    task_id: u64,
    result: Result<(), String>,
) {
    if let Ok(mut guard) = tasks.lock() {
        if let Some(task) = guard.get_mut(&task_id) {
            task.done = true;
            task.progress = 1.0;
            match result {
                Ok(()) => {
                    task.phase = "clear_db_complete".to_string();
                    task.message = "Database cleared".to_string();
                    task.error = None;
                }
                Err(err) => {
                    task.phase = "clear_db_failed".to_string();
                    task.message = "Database clear failed".to_string();
                    task.error = Some(err);
                }
            }
        }
    }
}

#[tauri::command]
pub(crate) fn find_resources_by_crc(
    crc32: u32,
    exclude_package_id: Option<String>,
    db: State<'_, Db>,
) -> Result<Vec<DbFindRef>, String> {
    let conn = db.inner().read().map_err(|err| err.to_string())?;
    let exclude: HashSet<String> = exclude_package_id.into_iter().collect();
    let mut result = db::find_crc_matches_bulk(&conn, &[crc32], &exclude, |_, _| {})
        .map_err(|err| err.to_string())?;
    Ok(result.remove(&crc32).unwrap_or_default())
}

/// Bulk variant of [`find_resources_by_crc`] for the Find Duplicates page —
/// the overview needs DB-match counts for every group's CRC32 up front, and
/// the per-CRC tauri round-trip is too slow for VARs with hundreds of
/// resources. Result map is keyed by CRC32 as a string (JSON object keys are
/// always strings); CRCs with no matches are omitted from the map.
#[tauri::command(async)]
pub(crate) fn find_resources_by_crcs_bulk(
    crc32s: Vec<u32>,
    exclude_package_id: Option<String>,
    db: State<'_, Db>,
) -> Result<HashMap<String, Vec<DbFindRef>>, String> {
    let conn = db.inner().read().map_err(|err| err.to_string())?;
    let exclude: HashSet<String> = exclude_package_id.into_iter().collect();
    let result = db::find_crc_matches_bulk(&conn, &crc32s, &exclude, |_, _| {})
        .map_err(|err| err.to_string())?;
    Ok(result
        .into_iter()
        .map(|(crc, refs)| (crc.to_string(), refs))
        .collect())
}

/// Load every indexed resource for `package_id`. Used by the Find Duplicates
/// page when the user picks a DB-sourced row as the keep target — the
/// frontend builds a CRC32 → ResourceRef map from this list and uses it to
/// relocate filtered groups via CRC matching.
#[tauri::command]
pub(crate) fn load_db_package_resources(
    package_id: String,
    db: State<'_, Db>,
) -> Result<Vec<ResourceRef>, String> {
    let conn = db.inner().read().map_err(|err| err.to_string())?;
    let file_path: Option<String> = conn
        .query_row(
            "SELECT file_path FROM packages WHERE package_id = ?1",
            rusqlite::params![package_id],
            |row| row.get::<_, String>(0),
        )
        .map(Some)
        .or_else(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })
        .map_err(|err| err.to_string())?;
    let Some(file_path) = file_path else {
        return Ok(Vec::new());
    };
    db::load_resources_for_package(&conn, &package_id, &file_path).map_err(|err| err.to_string())
}

/// Returns the set of internal paths the target VAR's own text files
/// reference back at itself (via `SELF:/path` or `<target_pkg_id>:/path`).
/// Used by Find Duplicates to gate which non-bundle resources are safe to
/// surface as dedup candidates: a file the user can't redirect from any
/// scene/.vap text isn't safe to remove because the game would silently
/// fail to find it. Bundle parents (.vam/.vmi) are handled separately by
/// the bundle-cascade machinery; this list complements that.
#[tauri::command]
pub(crate) fn list_target_var_text_refs(var_path: String) -> Result<Vec<String>, String> {
    use crate::utils::{decode_text, normalize_zip_path};

    let path = Path::new(&var_path);
    let target_pkg_id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .unwrap_or_default();

    let file = fs::File::open(path).map_err(|err| err.to_string())?;
    let mut archive = ZipArchive::new(file).map_err(|err| err.to_string())?;

    let mut out: HashSet<String> = HashSet::new();
    let self_needle: &[u8] = b"SELF:/";
    // References can carry any version of this package's id — a scene commonly
    // pins `<creator.asset>.latest:/...` (or an older `.N:/...`) while the file
    // on disk is `<creator.asset>.<this version>.var`. Match on the
    // version-stripped base so those still count as "the target references this
    // path" (mirrors VAM's runtime resolution; see fix_var::resolve_pkg_entry).
    let target_base: Option<String> = target_pkg_id
        .rsplit_once('.')
        .map(|(base, _)| base.to_ascii_lowercase());

    for index in 0..archive.len() {
        let mut entry = match archive.by_index(index) {
            Ok(e) => e,
            Err(_) => continue,
        };
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().replace('\\', "/");
        let lower_ext = Path::new(&name)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| format!(".{}", e.to_ascii_lowercase()));
        let is_text = matches!(
            lower_ext.as_deref(),
            Some(".json") | Some(".vam") | Some(".vaj") | Some(".vmi") | Some(".vap")
        );
        if !is_text {
            continue;
        }

        let mut raw = Vec::new();
        if entry.read_to_end(&mut raw).is_err() {
            continue;
        }

        // Implicit references VAM resolves at runtime without an explicit
        // `SELF:/` / `pkgid:/` string in the JSON. Mirror the scan's
        // support-path machinery so DB-mode's text-ref safety filter doesn't
        // hide legitimate duplicates (e.g. a clothing texture that a `.vam`
        // loads by stem, or a `.vaj` references via a relative path).
        match lower_ext.as_deref() {
            // A `.vaj` references its textures via `customTexture_*` values
            // that may be relative (no `SELF:/` prefix) — resolve them.
            Some(".vaj") => {
                out.extend(crate::scan::extract_vaj_support_paths(&name, &raw));
            }
            // A `.vam` implicitly loads the same-stem `.png` / `.jpg` / `.jpeg`
            // sibling with no reference string at all.
            Some(".vam") => {
                out.extend(crate::scan::same_stem_texture_paths(&name));
            }
            _ => {}
        }

        let Some((text, _)) = decode_text(&raw) else {
            continue;
        };
        let bytes = text.as_bytes();

        // `SELF:/path` — the package referencing its own resources. Stops at the
        // surrounding quote/newline to match the JSON string.
        let mut harvest_self = |start: usize| {
            let mut end = start;
            while end < bytes.len() {
                let c = bytes[end];
                if c == b'"' || c == b'\'' || c == b'\r' || c == b'\n' {
                    break;
                }
                end += 1;
            }
            while end > start && (bytes[end - 1] == b' ' || bytes[end - 1] == b'\t') {
                end -= 1;
            }
            if end > start {
                if let Ok(p) = std::str::from_utf8(&bytes[start..end]) {
                    out.insert(normalize_zip_path(p));
                }
            }
            end
        };
        let mut i = 0;
        while i < bytes.len() {
            if i + self_needle.len() <= bytes.len()
                && &bytes[i..i + self_needle.len()] == self_needle
            {
                i = harvest_self(i + self_needle.len());
                continue;
            }
            i += 1;
        }

        // `<package_id>:/path` for any version of this package's own id. Reuse
        // the tolerant, CJK-safe ref walker from fix_var so `.latest` / `.N`
        // prefixes are caught instead of only the exact on-disk stem.
        for (pkg, ref_path) in crate::fix_var::collect_pkg_refs(&text) {
            let matches_target = pkg.eq_ignore_ascii_case(&target_pkg_id)
                || match (target_base.as_deref(), pkg.rsplit_once('.')) {
                    (Some(tb), Some((pb, _))) => pb.eq_ignore_ascii_case(tb),
                    _ => false,
                };
            if matches_target {
                out.insert(normalize_zip_path(&ref_path));
            }
        }
    }

    Ok(out.into_iter().collect())
}

// (async) so opening the archive and walking its central directory runs off the
// main thread — a texture-heavy .var holds tens of thousands of entries, and a
// plain #[tauri::command] freezes the whole window while they are read.
#[tauri::command(async)]
pub(crate) fn list_var_resources(var_path: String) -> Result<Vec<VarResourceEntry>, String> {
    let path = Path::new(&var_path);
    let file = fs::File::open(path).map_err(|err| err.to_string())?;
    let mut archive = ZipArchive::new(file).map_err(|err| err.to_string())?;
    let mut out: Vec<VarResourceEntry> = Vec::with_capacity(archive.len());
    for index in 0..archive.len() {
        let entry = match archive.by_index(index) {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        if entry.is_dir() {
            continue;
        }
        out.push(VarResourceEntry {
            internal_path: entry.name().replace('\\', "/"),
            crc32: entry.crc32(),
            size: entry.size(),
        });
    }
    Ok(out)
}

// (async) so the folder walk and the scene-image id load run off the main
// thread — a plain #[tauri::command] freezes the whole window while they run.
#[allow(clippy::too_many_arguments)] // a Tauri command: one argument per field the UI sends
#[tauri::command(async)]
pub(crate) fn list_var_packages(
    input_dir: String,
    additional_input_dirs: Option<Vec<String>>,
    offset: u64,
    limit: u64,
    search: Option<String>,
    filters: Option<VarPackageFilters>,
    // Sort is a top-level arg rather than a VarPackageFilters field for two
    // reasons: the frontend's "Clear All" resets the filters object and must
    // not clear the sort, and the folder cache key covers roots + depth only,
    // so re-sorting correctly reuses the cache instead of re-walking the disk.
    sort: Option<String>,
    sort_dir: Option<String>,
    force_rescan: Option<bool>,
    deep_scan: Option<bool>,
    // The offload folder. Packages under it are flagged `offloaded`; the caller
    // also lists it among the additional dirs when it should be scanned.
    offload_dir: Option<String>,
    db: State<'_, Db>,
    state: State<'_, AppState>,
) -> Result<VarPackagePage, String> {
    let force = force_rescan.unwrap_or(false);
    let offload_root = offload_dir
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(PathBuf::from);
    let filters = filters.unwrap_or_default();
    let additional = additional_input_dirs.unwrap_or_default();
    let additional_paths: Vec<PathBuf> = additional
        .iter()
        .map(|dir| dir.trim())
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .collect();
    let roots = crate::scan::scan_roots(Path::new(&input_dir), &additional_paths);
    // Absent = deep, matching every other walk in the app and the behavior this
    // page had before the toggle existed.
    let depth = crate::utils::ScanDepth::from_deep(deep_scan.unwrap_or(true));
    // The depth is part of the key, not just the roots: the two modes produce
    // different result sets for the same folders, so sharing a key would serve a
    // deep listing after switching to top-level (and vice versa) until something
    // else forced a rescan.
    let cache_key = format!(
        "{}|{}|{}",
        crate::utils::scan_cache_key_multi(
            &roots.iter().map(PathBuf::as_path).collect::<Vec<_>>(),
            None,
        ),
        depth.cache_token(),
        offload_root
            .as_ref()
            .map(|p| p.display().to_string().to_ascii_lowercase())
            .unwrap_or_default(),
    );

    // Snapshotted BEFORE the walk below, which runs without holding the cache
    // lock. If an apply bumps the generation while we walk, whatever we
    // collected is a torn pre-apply view and must not be cached — the cache is
    // validated by `cache_key` alone, with no fingerprint, so a stale entry
    // under a live key would be served to every later non-forced call
    // (pagination, filters, search) indefinitely.
    let generation_at_start = state.var_packages_cache_generation.load(Ordering::SeqCst);

    // Decide whether the cached scan can be reused. Same roots + no force = hit.
    let need_rescan = {
        let cache = state
            .var_packages_folder_cache
            .lock()
            .map_err(|_| "var packages folder cache poisoned".to_string())?;
        force
            || match cache.as_ref() {
                Some(c) => c.cache_key != cache_key,
                None => true,
            }
    };

    // Set only when the walk raced an apply: serve these for this call, but
    // leave the cache empty so the next call re-walks.
    let mut fresh_items: Option<(Vec<VarPackageListItem>, u64)> = None;

    if need_rescan {
        let dir = Path::new(&input_dir);
        if !dir.is_dir() {
            return Err(format!("not a directory: {}", dir.display()));
        }

        let entries = crate::utils::collect_var_files_multi_with_depth(&roots, depth)
            .map_err(|err| err.to_string())?;

        // Point lookups for just these files: loading every id in a large
        // index (tens of thousands) cost most of a rescan.
        let known_ids = {
            let conn = db.read().map_err(|err| err.to_string())?;
            let stems: Vec<&str> = entries
                .iter()
                .filter_map(|e| e.path.file_stem().and_then(|s| s.to_str()))
                .collect();
            known_package_ids_among(&conn, stems)?
        };

        // Opens only archives that are new or changed since they were last
        // read; everything else comes from the info cache.
        let infos = crate::library::infos_for_entries(&entries, &state, &db);

        let mut scanned: Vec<VarPackageListItem> = Vec::with_capacity(entries.len());
        for (entry, info) in entries.into_iter().zip(infos) {
            let path = &entry.path;
            let file_name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            let package_id = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            let creator = crate::naming::creator_from_package_id(&package_id).map(str::to_string);
            let modified_ms = u64::try_from(entry.fingerprint.modified_ns / 1_000_000).ok();
            let indexed = known_ids.contains(&package_id);

            scanned.push(VarPackageListItem {
                file_path: path.display().to_string(),
                file_name,
                package_id,
                creator,
                size_bytes: entry.fingerprint.size,
                modified_ms,
                indexed,
                pkg_type: info.pkg_type,
                item_count: info.item_count,
                dep_count: info.deps.len() as u32,
                disabled: crate::packages::disabled_sidecar(path).exists(),
                offloaded: offload_root
                    .as_deref()
                    .is_some_and(|root| crate::offload::path_is_under(path, root)),
                has_scene_image: info.has_scene_image,
                license: info.license,
                readable: info.readable,
                deps: info.deps,
                ..VarPackageListItem::default()
            });
        }
        let missing_unique = crate::library::apply_graph(&mut scanned);
        crate::roles::apply(&mut scanned, &db);

        let mut cache = state
            .var_packages_folder_cache
            .lock()
            .map_err(|_| "var packages folder cache poisoned".to_string())?;
        if state.var_packages_cache_generation.load(Ordering::SeqCst) == generation_at_start {
            *cache = Some(VarPackagesFolderCache {
                cache_key: cache_key.clone(),
                items: scanned,
                missing_unique,
                hub_stamp: 0,
                integrity_stamp: 0,
            });
        } else {
            // An apply mutated the library mid-walk. Drop the cache so the next
            // call rescans, and serve this (possibly torn) snapshot only to the
            // caller who asked for it.
            *cache = None;
            fresh_items = Some((scanned, missing_unique));
        }
    }

    // Loaded before taking the cache lock so the DB read never runs while
    // holding the folder-cache mutex. Always loaded (the tables are tiny): the
    // Favorites and replacement-source rows of the status facet need them
    // even when not filtering.
    let marks = {
        let conn = db.read().map_err(|err| err.to_string())?;
        crate::library::PackageMarks {
            favorites: db::get_favorite_package_ids(&conn).map_err(|err| err.to_string())?,
            replacement: db::get_replacement_prefs(&conn)
                .map_err(|err| err.to_string())?
                .into_iter()
                .collect(),
        }
    };

    let mut cache = state
        .var_packages_folder_cache
        .lock()
        .map_err(|_| "var packages folder cache poisoned".to_string())?;
    // Hub fields (updates, Not on Hub) follow the Hub package index, which
    // loads and refreshes on its own schedule.
    let hub_generation = crate::hub_index::generation();
    let integrity_generation = crate::integrity::generation();
    if let Some((fresh, _)) = fresh_items.as_mut() {
        crate::hub_index::annotate(fresh);
        crate::integrity::annotate(fresh, &db);
    }
    if let Some(c) = cache.as_mut() {
        if c.hub_stamp != hub_generation {
            crate::hub_index::annotate(&mut c.items);
            c.hub_stamp = hub_generation;
        }
        if c.integrity_stamp != integrity_generation {
            crate::integrity::annotate(&mut c.items, &db);
            c.integrity_stamp = integrity_generation;
        }
    }
    let (items, missing_unique): (&[VarPackageListItem], u64) = match (fresh_items.as_ref(), cache.as_ref()) {
        (Some((fresh, missing)), _) => (fresh.as_slice(), *missing),
        (None, Some(c)) if c.cache_key == cache_key => (&c.items, c.missing_unique),
        _ => {
            return Ok(VarPackagePage {
                items: Vec::new(),
                total: 0,
                facets: None,
            })
        }
    };

    let query = crate::library::SearchQuery::parse(search.as_deref());

    // `base` passes every filter except the two facets (status, type), so the
    // facet counts can each ignore their own selection.
    let base: Vec<&VarPackageListItem> = items
        .iter()
        .filter(|item| {
            if let Some(q) = query.as_ref() {
                if !q.matches(item) {
                    return false;
                }
            }
            matches_var_package_filters(item, &filters, &marks.favorites)
                && crate::library::location_matches(item, &filters)
        })
        .collect();
    let mut facets = crate::library::compute_facets(items, &base, &filters, &marks, missing_unique);

    let mut filtered: Vec<&VarPackageListItem> = base
        .into_iter()
        .filter(|item| {
            crate::library::library_status_matches(item, &filters, &marks)
                && crate::library::type_matches(item, &filters)
        })
        .collect();
    facets.total_bytes = filtered.iter().map(|i| i.size_bytes).sum();
    facets.total_items = filtered.iter().map(|i| u64::from(i.item_count)).sum();
    facets.total_deps = filtered.iter().filter(|i| i.used_by_count > 0).count() as u64;

    // Sort before slicing so the page reflects the whole matching set, not just
    // its arrival order. compare_var_packages carries the package_id tiebreaker
    // that keeps pagination deterministic and mirrors database-mode ordering.
    let sort_key = VarPackageSort::parse(sort.as_deref());
    let sort_desc = parse_sort_desc(sort_dir.as_deref());
    filtered.sort_by(|a, b| compare_var_packages(a, b, sort_key, sort_desc));

    let total = filtered.len() as u64;
    let start = (offset as usize).min(filtered.len());
    let end = start.saturating_add(limit.clamp(1, 1000) as usize).min(filtered.len());
    let page = filtered[start..end].iter().map(|i| (*i).clone()).collect();

    Ok(VarPackagePage {
        items: page,
        total,
        facets: Some(facets),
    })
}

/// The folder-mode filters that are not facets, served from the in-memory
/// cache without round-tripping the DB. Status and content type are facets and
/// live in `library` so their counts can ignore their own selection.
fn matches_var_package_filters(
    item: &VarPackageListItem,
    filters: &VarPackageFilters,
    favorite_ids: &HashSet<String>,
) -> bool {
    if let Some(scene) = filters
        .scene_image
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        // Read from the archive itself during the scan, so a package the
        // database has never indexed still answers correctly.
        let has = item.has_scene_image;
        match scene {
            "with" if !has => return false,
            "without" if has => return false,
            _ => {}
        }
    }
    if let Some(bucket) = filters
        .size_bucket
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        const MB: u64 = 1024 * 1024;
        const GB: u64 = 1024 * MB;
        let size = item.size_bytes;
        match bucket {
            "sm" if size >= 100 * MB => return false,
            "md" if !(100 * MB..GB).contains(&size) => return false,
            "lg" if size < GB => return false,
            _ => {}
        }
    }
    if let Some(creator) = filters.creator.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        if item.creator.as_deref().unwrap_or("") != creator {
            return false;
        }
    }
    if filters.favorite.unwrap_or(false) && !favorite_ids.contains(&item.package_id) {
        return false;
    }
    true
}

/// Which of `ids` the database knows, by indexed point lookups — for when only
/// a few hundred ids matter and loading all of them would dominate.
pub(crate) fn known_package_ids_among<'a>(
    conn: &rusqlite::Connection,
    ids: impl IntoIterator<Item = &'a str>,
) -> Result<HashSet<String>, String> {
    let mut stmt = conn
        .prepare_cached("SELECT 1 FROM packages WHERE package_id = ?1")
        .map_err(|err| err.to_string())?;
    let mut set = HashSet::new();
    for id in ids {
        if stmt.exists([id]).map_err(|err| err.to_string())? {
            set.insert(id.to_string());
        }
    }
    Ok(set)
}

/// A database package id in the family `base` (compared case-insensitively),
/// found by a range scan of `idx_packages_id_lower` over ids starting `base.`
/// rather than by loading every id.
pub(crate) fn known_package_in_family(
    conn: &rusqlite::Connection,
    base: &str,
) -> Result<Option<String>, String> {
    let mut stmt = conn
        .prepare_cached(
            "SELECT package_id FROM packages WHERE lower(package_id) >= ?1 AND lower(package_id) < ?2",
        )
        .map_err(|err| err.to_string())?;
    // SQLite's lower() folds ASCII only, matching to_ascii_lowercase.
    let lower = base.to_ascii_lowercase();
    // '/' sorts right after '.', so this is exactly the ids prefixed `base.`.
    let rows = stmt
        .query_map([format!("{lower}."), format!("{lower}/")], |row| row.get::<_, String>(0))
        .map_err(|err| err.to_string())?;
    for id in rows.flatten() {
        if crate::naming::package_base(&id).eq_ignore_ascii_case(base) {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

pub(crate) fn load_known_package_ids(conn: &rusqlite::Connection) -> Result<HashSet<String>, String> {
    let mut stmt = conn
        .prepare("SELECT package_id FROM packages")
        .map_err(|err| err.to_string())?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|err| err.to_string())?;
    let mut set = HashSet::new();
    for id in rows.flatten() {
        set.insert(id);
    }
    Ok(set)
}

/// The sort keys the VAR Packages page offers. Parsing user input into this
/// enum is the injection guard: `var_package_order_by_sql` matches on the enum
/// and emits fixed column literals, so a request string never reaches the SQL.
///
/// There is deliberately no `Creator` key. `naming::creator_from_package_id` is
/// just the substring before the first `.`, and `.` (0x2E) sorts below every
/// alphanumeric, so ordering by `package_id` already yields exactly creator
/// order with the package name as the tiebreak — a Creator option would be a
/// menu entry that changes nothing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum VarPackageSort {
    Name,
    Size,
    Modified,
    /// Folder mode only (database mode orders these by name): content type in
    /// filter-panel order, item count, and dependency count.
    Type,
    Items,
    Deps,
}

impl VarPackageSort {
    /// Anything unrecognised — absent, empty, or hostile — falls back to
    /// `Name`, which is the ordering this page had before sorting existed. An
    /// older frontend that sends nothing therefore behaves exactly as before.
    pub(crate) fn parse(raw: Option<&str>) -> Self {
        match raw.map(str::trim) {
            Some("size") => Self::Size,
            Some("modified") => Self::Modified,
            Some("type") => Self::Type,
            Some("items") => Self::Items,
            Some("deps") => Self::Deps,
            _ => Self::Name,
        }
    }
}

/// True only for an explicit "desc" — every other value keeps ascending order.
pub(crate) fn parse_sort_desc(raw: Option<&str>) -> bool {
    matches!(raw.map(str::trim), Some("desc"))
}

/// The ORDER BY body (no `ORDER BY` keyword) for database-mode listing. Both
/// query branches in `list_var_packages_from_db` call this, so they can never
/// drift apart — the same one-source-of-truth discipline as
/// `db::SCENE_IMAGE_PREDICATE_SQL`.
///
/// Two non-obvious requirements are baked in here:
///
/// 1. `modified_ns` is declared TEXT and manifest imports store the literal
///    '0', so a bare column compare would be lexicographic and would order
///    those rows against the scanner's 19-digit epoch-ns values incorrectly.
///    CAST makes it numeric regardless of how the value was stored.
/// 2. Size and Modified append an always-ascending `package_id` tiebreaker.
///    Without it SQLite is free to order ties arbitrarily, and because the
///    caller paginates with LIMIT/OFFSET across separate queries, a row could
///    appear on two pages or on none. Equal sizes are common and equal '0'
///    mtimes are guaranteed on a manifest-imported library.
///
/// Emits NO bound parameters. The caller pushes LIMIT and OFFSET positionally
/// after the WHERE params, so a `?` here would shift them and silently corrupt
/// the query.
pub(crate) fn var_package_order_by_sql(sort: VarPackageSort, desc: bool) -> String {
    let dir = if desc { "DESC" } else { "ASC" };
    match sort {
        // package_id is already unique, so it needs no tiebreaker. The library
        // keys have no database column, so they order by name there.
        VarPackageSort::Name | VarPackageSort::Type | VarPackageSort::Items | VarPackageSort::Deps => {
            format!("p.package_id COLLATE NOCASE {dir}")
        }
        VarPackageSort::Size => {
            format!("p.size_bytes {dir}, p.package_id COLLATE NOCASE ASC")
        }
        VarPackageSort::Modified => {
            format!("CAST(p.modified_ns AS INTEGER) {dir}, p.package_id COLLATE NOCASE ASC")
        }
    }
}

/// Folder-mode counterpart of `var_package_order_by_sql`. Kept next to it so
/// the two orderings are read and changed together — folder mode and database
/// mode must agree on the sequence, including how ties resolve.
pub(crate) fn compare_var_packages(
    a: &VarPackageListItem,
    b: &VarPackageListItem,
    sort: VarPackageSort,
    desc: bool,
) -> std::cmp::Ordering {
    let primary = match sort {
        VarPackageSort::Name => a
            .package_id
            .to_lowercase()
            .cmp(&b.package_id.to_lowercase()),
        VarPackageSort::Size => a.size_bytes.cmp(&b.size_bytes),
        // A file with no mtime sorts oldest rather than floating to an
        // arbitrary position; SQL sees the same thing, since those rows carry
        // modified_ns '0'.
        VarPackageSort::Modified => a.modified_ms.unwrap_or(0).cmp(&b.modified_ms.unwrap_or(0)),
        VarPackageSort::Type => {
            crate::library::type_order(&a.pkg_type).cmp(&crate::library::type_order(&b.pkg_type))
        }
        VarPackageSort::Items => a.item_count.cmp(&b.item_count),
        VarPackageSort::Deps => a.dep_count.cmp(&b.dep_count),
    };
    let primary = if desc { primary.reverse() } else { primary };
    // Tiebreaker stays ASCENDING in both directions, matching the SQL — this is
    // what keeps pagination deterministic. A no-op for Name.
    primary.then_with(|| {
        a.package_id
            .to_lowercase()
            .cmp(&b.package_id.to_lowercase())
    })
}

/// Builds the WHERE-clause fragments and bound parameters that apply the
/// search needle, creator, category, and size-bucket filters to the
/// `packages` table. Used by both the count query and the paginated select
/// query so both stay in sync. Status filter is *not* included here — it
/// requires a per-row filesystem stat and is applied after the SQL fetch.
pub(crate) fn build_db_package_where(
    search: &Option<String>,
    filters: &VarPackageFilters,
) -> (String, Vec<rusqlite::types::Value>) {
    use rusqlite::types::Value;

    let mut clauses: Vec<String> = Vec::new();
    let mut params: Vec<Value> = Vec::new();

    if let Some(needle) = search
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let pattern = format!(
            "%{}%",
            needle
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        clauses.push(
            "(p.package_id LIKE ? ESCAPE '\\' OR p.file_path LIKE ? ESCAPE '\\')".to_string(),
        );
        params.push(Value::Text(pattern.clone()));
        params.push(Value::Text(pattern));
    }

    if let Some(creator) = filters
        .creator
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        clauses.push(
            "EXISTS (SELECT 1 FROM creators c WHERE c.creator_id = p.creator_id AND c.name = ?)"
                .to_string(),
        );
        params.push(Value::Text(creator.to_string()));
    }

    if let Some(bucket) = filters
        .size_bucket
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        const MB: i64 = 1024 * 1024;
        const GB: i64 = 1024 * MB;
        match bucket {
            "sm" => {
                clauses.push("p.size_bytes < ?".to_string());
                params.push(Value::Integer(100 * MB));
            }
            "md" => {
                clauses.push("p.size_bytes >= ? AND p.size_bytes < ?".to_string());
                params.push(Value::Integer(100 * MB));
                params.push(Value::Integer(GB));
            }
            "lg" => {
                clauses.push("p.size_bytes >= ?".to_string());
                params.push(Value::Integer(GB));
            }
            _ => {}
        }
    }

    if filters.favorite.unwrap_or(false) {
        clauses.push(
            "EXISTS (SELECT 1 FROM package_flags pf \
             WHERE pf.package_id = p.package_id AND pf.flag = ?)"
                .to_string(),
        );
        params.push(Value::Integer(db::PACKAGE_FLAG_FAVORITE as i64));
    }

    // Built from the same predicate db::get_scene_image_package_ids uses, so
    // database mode and folder mode can never disagree about a package.
    if let Some(scene) = filters
        .scene_image
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        // IN, not a correlated EXISTS: the planner refuses the v12 partial
        // index inside a correlated subquery and falls back to walking every
        // resource row of every package (measured ~4.5 s per page on a 4.4M-row
        // library). The uncorrelated subquery materializes once off the partial
        // index (~40 ms). NOT IN is NULL-safe here — resources.package_id is
        // NOT NULL. Unqualified `internal_path` binds to the inner `resources`
        // row, so the shared predicate drops in verbatim.
        let id_set = format!(
            "(SELECT package_id FROM resources WHERE {})",
            db::SCENE_IMAGE_PREDICATE_SQL,
        );
        match scene {
            "with" => clauses.push(format!("p.package_id IN {id_set}")),
            "without" => clauses.push(format!("p.package_id NOT IN {id_set}")),
            _ => {}
        }
    }

    let where_sql = if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    };
    (where_sql, params)
}

// (async) for the same reason as list_var_packages: the scene-image EXISTS
// subquery runs against the whole resources table on every page.
#[tauri::command(async)]
pub(crate) fn list_var_packages_from_db(
    offset: u64,
    limit: u64,
    search: Option<String>,
    filters: Option<VarPackageFilters>,
    // Top-level, matching list_var_packages — see the note there.
    sort: Option<String>,
    sort_dir: Option<String>,
    db: State<'_, Db>,
) -> Result<VarPackagePage, String> {
    use rusqlite::types::Value;

    let filters = filters.unwrap_or_default();
    let status_filter = filters
        .status
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let conn = db.read().map_err(|err| err.to_string())?;

    let (where_sql, where_params) = build_db_package_where(&search, &filters);
    // Built from the enum, so no request string ever reaches the SQL, and it
    // binds no parameters — the LIMIT/OFFSET values below are positional.
    let order_sql = var_package_order_by_sql(
        VarPackageSort::parse(sort.as_deref()),
        parse_sort_desc(sort_dir.as_deref()),
    );

    let limit_i = limit.clamp(1, 1000) as i64;
    let offset_i = offset.min(i64::MAX as u64) as i64;

    // When the status filter is engaged we must materialize all SQL-matching
    // rows, FS-stat each one, then paginate the survivors so `total` and
    // pagination stay correct. Without status, we use index-friendly
    // SQL-side LIMIT/OFFSET.
    if status_filter.is_some() {
        let want_indexed = matches!(status_filter.as_deref(), Some("indexed"));
        let select_sql = format!(
            "SELECT p.package_id, p.file_path, p.size_bytes, p.modified_ns FROM packages p{} \
             ORDER BY {}",
            where_sql, order_sql
        );
        let mut stmt = conn.prepare(&select_sql).map_err(|err| err.to_string())?;
        let bound: Vec<&dyn rusqlite::ToSql> =
            where_params.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
        let rows = stmt
            .query_map(rusqlite::params_from_iter(bound), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .map_err(|err| err.to_string())?;
        let mut filtered: Vec<(String, String, i64, String, bool)> = Vec::new();
        for row in rows {
            let (pid, file_path, size_bytes, modified_ns_text) =
                row.map_err(|err| err.to_string())?;
            let on_disk = Path::new(&file_path).is_file();
            if on_disk == want_indexed {
                filtered.push((pid, file_path, size_bytes, modified_ns_text, on_disk));
            }
        }
        let total = filtered.len() as u64;
        let start = (offset_i as usize).min(filtered.len());
        let end = (start + limit_i as usize).min(filtered.len());
        let page_slice = &filtered[start..end];
        let items = build_items_from_rows(page_slice.iter().map(
            |(pid, fp, size, mns, indexed)| {
                (pid.clone(), fp.clone(), *size, mns.clone(), Some(*indexed))
            },
        ));
        return Ok(VarPackagePage {
            items,
            total,
            facets: None,
        });
    }

    let total: u64 = {
        let count_sql = format!("SELECT COUNT(*) FROM packages p{}", where_sql);
        let mut stmt = conn.prepare(&count_sql).map_err(|err| err.to_string())?;
        let bound: Vec<&dyn rusqlite::ToSql> =
            where_params.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
        stmt.query_row(rusqlite::params_from_iter(bound), |row| row.get::<_, i64>(0))
            .map_err(|err| err.to_string())?
            .max(0) as u64
    };

    let select_sql = format!(
        "SELECT p.package_id, p.file_path, p.size_bytes, p.modified_ns FROM packages p{} \
         ORDER BY {} \
         LIMIT ? OFFSET ?",
        where_sql, order_sql
    );
    let mut stmt = conn.prepare(&select_sql).map_err(|err| err.to_string())?;
    let mut bound: Vec<Value> = where_params;
    bound.push(Value::Integer(limit_i));
    bound.push(Value::Integer(offset_i));
    let bound_refs: Vec<&dyn rusqlite::ToSql> =
        bound.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
    let rows = stmt
        .query_map(rusqlite::params_from_iter(bound_refs), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(|err| err.to_string())?;

    let raw: Vec<(String, String, i64, String)> = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|err| err.to_string())?;
    let items = build_items_from_rows(
        raw.into_iter()
            .map(|(pid, fp, size, mns)| (pid, fp, size, mns, None)),
    );

    Ok(VarPackagePage {
        items,
        total,
        facets: None,
    })
}

/// Shared row-to-item shaping for `list_var_packages_from_db`. The
/// `precomputed_indexed` element lets the status-filter branch reuse the FS
/// stat it already did instead of stat'ing the file twice.
fn build_items_from_rows<I>(rows: I) -> Vec<VarPackageListItem>
where
    I: IntoIterator<Item = (String, String, i64, String, Option<bool>)>,
{
    rows.into_iter()
        .map(|(package_id, file_path, size_bytes, modified_ns_text, precomputed_indexed)| {
            let path = Path::new(&file_path);
            let file_name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            let creator =
                crate::naming::creator_from_package_id(&package_id).map(str::to_string);
            let modified_ms = modified_ns_text
                .parse::<u128>()
                .ok()
                .and_then(|ns| u64::try_from(ns / 1_000_000).ok());
            let indexed = precomputed_indexed.unwrap_or_else(|| path.is_file());
            VarPackageListItem {
                file_path,
                file_name,
                package_id,
                creator,
                size_bytes: size_bytes.max(0) as u64,
                modified_ms,
                indexed,
                ..VarPackageListItem::default()
            }
        })
        .collect()
}

/// Distinct creators for the Creator filter dropdown, sorted case-insensitively.
///
/// `scope` = `"folder"` lists only the creators of the currently scanned folder,
/// read straight off the folder cache `list_var_packages` just populated — the
/// whole-DB list offered creators that aren't in the folder being browsed, so
/// picking one emptied the grid. Any other value (or none) keeps the DB-wide
/// list, which is what database mode wants.
#[tauri::command(async)]
pub(crate) fn list_var_package_filter_options(
    scope: Option<String>,
    db: State<'_, Db>,
    state: State<'_, AppState>,
) -> Result<VarPackageFilterOptions, String> {
    if scope.as_deref().map(str::trim) == Some("folder") {
        let cache = state
            .var_packages_folder_cache
            .lock()
            .map_err(|_| "var packages folder cache poisoned".to_string())?;
        let mut creators: Vec<String> = match cache.as_ref() {
            Some(c) => {
                // Exact case, deduped: matches_var_package_filters compares the
                // selection with `!=`, so a case-folded name would never match.
                let mut seen: HashSet<String> = HashSet::new();
                c.items
                    .iter()
                    .filter_map(|item| item.creator.as_deref())
                    .filter(|name| !name.is_empty())
                    .filter(|name| seen.insert((*name).to_string()))
                    .map(str::to_string)
                    .collect()
            }
            // Nothing scanned yet — an empty menu matches the empty grid.
            None => Vec::new(),
        };
        creators.sort_by_key(|a| a.to_lowercase());
        return Ok(VarPackageFilterOptions { creators });
    }

    let conn = db.read().map_err(|err| err.to_string())?;

    let mut creators: Vec<String> = {
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT c.name FROM creators c \
                 JOIN packages p ON p.creator_id = c.creator_id \
                 WHERE c.name IS NOT NULL AND c.name <> ''",
            )
            .map_err(|err| err.to_string())?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|err| err.to_string())?;
        let mut v: Vec<String> = Vec::new();
        for row in rows {
            v.push(row.map_err(|err| err.to_string())?);
        }
        v
    };
    creators.sort_by_key(|a| a.to_lowercase());

    Ok(VarPackageFilterOptions { creators })
}

/// Builds the WHERE clause + bound params for the Resource List page.
fn build_resource_list_where(
    search: &Option<String>,
    filters: &ResourceListFilters,
) -> (String, Vec<rusqlite::types::Value>) {
    use rusqlite::types::Value;

    let mut clauses: Vec<String> = Vec::new();
    let mut params: Vec<Value> = Vec::new();

    if let Some(needle) = search
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        // Format detection picks the fastest indexed path:
        //   * 8 hex chars → exact CRC32 match (idx_resources_crc32).
        //   * else        → prefix match on package_id (idx_resources_package).
        let is_hex = |s: &str| s.bytes().all(|b| b.is_ascii_hexdigit());
        if needle.len() == 8 && is_hex(needle) {
            if let Ok(crc) = u32::from_str_radix(needle, 16) {
                clauses.push("r.crc32 = ?".to_string());
                params.push(Value::Integer(crc as i64));
            }
        } else {
            let pattern = format!(
                "{}%",
                needle
                    .replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_")
            );
            clauses.push("r.package_id LIKE ? ESCAPE '\\'".to_string());
            params.push(Value::Text(pattern));
        }
    }

    if let Some(category) = filters
        .category
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        clauses.push(
            "EXISTS (SELECT 1 FROM categories cat WHERE cat.category_id = r.category_id AND cat.name = ?)"
                .to_string(),
        );
        params.push(Value::Text(category.to_string()));
    }

    if let Some(bucket) = filters
        .size_bucket
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        // Resource sizes are typically much smaller than package sizes, so the
        // buckets are scaled down: small (< 1 MB), medium (1–100 MB),
        // large (> 100 MB).
        const MB: i64 = 1024 * 1024;
        match bucket {
            "sm" => {
                clauses.push("r.size < ?".to_string());
                params.push(Value::Integer(MB));
            }
            "md" => {
                clauses.push("r.size >= ? AND r.size < ?".to_string());
                params.push(Value::Integer(MB));
                params.push(Value::Integer(100 * MB));
            }
            "lg" => {
                clauses.push("r.size >= ?".to_string());
                params.push(Value::Integer(100 * MB));
            }
            _ => {}
        }
    }

    let where_sql = if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    };
    (where_sql, params)
}

/// Lists every indexed resource in the local database, joined to
/// `categories` and `packages`. Supports the same offset/limit pagination
/// as `list_var_packages_from_db`.
///
/// Returned rows have no explicit ORDER BY — SQLite scans in rowid (PK)
/// order, which is effectively insertion order for an
/// `INTEGER PRIMARY KEY AUTOINCREMENT` table. That's stable enough for
/// pagination within a session and avoids the cost of a sort on
/// multi-million-row tables.
///
/// `skip_total = true` skips the inline COUNT(*) — a multi-second scan on
/// multi-million-row DBs — so page rows can render immediately. Callers
/// that need the total should fire `count_resources_from_db` separately and
/// merge the result. Default (`None` / `Some(false)`) computes COUNT inline
/// for backward compatibility.
#[tauri::command]
pub(crate) fn list_resources_from_db(
    offset: u64,
    limit: u64,
    search: Option<String>,
    filters: Option<ResourceListFilters>,
    skip_total: Option<bool>,
    db: State<'_, Db>,
) -> Result<ResourceListPage, String> {
    use rusqlite::types::Value;

    let filters = filters.unwrap_or_default();

    let conn = db.read().map_err(|err| err.to_string())?;

    let (where_sql, where_params) = build_resource_list_where(&search, &filters);

    let limit_i = limit.clamp(1, 1000) as i64;
    let offset_i = offset.min(i64::MAX as u64) as i64;

    let total: u64 = if skip_total.unwrap_or(false) {
        // Caller will fetch the total via `count_resources_from_db` and
        // merge it in. Skipping it here makes page loads sub-second even on
        // multi-million-row tables.
        0
    } else {
        let count_sql = format!("SELECT COUNT(*) FROM resources r{}", where_sql);
        let mut stmt = conn.prepare(&count_sql).map_err(|err| err.to_string())?;
        let bound_refs: Vec<&dyn rusqlite::ToSql> =
            where_params.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
        stmt.query_row(rusqlite::params_from_iter(bound_refs), |row| row.get::<_, i64>(0))
            .map_err(|err| err.to_string())?
            .max(0) as u64
    };

    let select_sql = format!(
        "SELECT r.id, r.package_id, p.file_path, r.internal_path, r.crc32, \
                r.size, r.effective_size, cat.name \
         FROM resources r \
         LEFT JOIN categories cat ON cat.category_id = r.category_id \
         LEFT JOIN packages   p   ON p.package_id    = r.package_id\
         {where_sql} \
         LIMIT ? OFFSET ?",
        where_sql = where_sql
    );
    let mut stmt = conn.prepare(&select_sql).map_err(|err| err.to_string())?;
    let mut bound: Vec<Value> = where_params;
    bound.push(Value::Integer(limit_i));
    bound.push(Value::Integer(offset_i));
    let bound_refs: Vec<&dyn rusqlite::ToSql> =
        bound.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
    let rows = stmt
        .query_map(rusqlite::params_from_iter(bound_refs), |row| {
            let resource_id: i64 = row.get(0)?;
            let package_id: String = row.get(1)?;
            let package_file: Option<String> = row.get(2)?;
            let internal_path: String = row.get(3)?;
            let crc32: Option<i64> = row.get(4)?;
            let size: i64 = row.get(5)?;
            let effective_size: Option<i64> = row.get(6)?;
            let category: Option<String> = row.get(7)?;
            Ok(ResourceListItem {
                resource_id,
                package_id,
                package_file: package_file.unwrap_or_default(),
                internal_path,
                crc32: crc32.map(|v| v as u32),
                size: size.max(0) as u64,
                effective_size: effective_size.unwrap_or(size).max(0) as u64,
                category,
            })
        })
        .map_err(|err| err.to_string())?;

    let items: Vec<ResourceListItem> = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|err| err.to_string())?;

    Ok(ResourceListPage { items, total })
}

/// Returns the COUNT(*) for the same filter + search as
/// `list_resources_from_db` would have used. Split out so the frontend can
/// fire it AFTER the (fast) page rows return — the count itself is the
/// multi-second part on multi-million-row tables, and the user gets to see
/// the table immediately while it computes.
#[tauri::command]
pub(crate) fn count_resources_from_db(
    search: Option<String>,
    filters: Option<ResourceListFilters>,
    db: State<'_, Db>,
) -> Result<u64, String> {
    let filters = filters.unwrap_or_default();

    let conn = db.read().map_err(|err| err.to_string())?;

    let (where_sql, where_params) = build_resource_list_where(&search, &filters);

    let count_sql = format!("SELECT COUNT(*) FROM resources r{}", where_sql);
    let mut stmt = conn.prepare(&count_sql).map_err(|err| err.to_string())?;
    let bound_refs: Vec<&dyn rusqlite::ToSql> =
        where_params.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
    let total = stmt
        .query_row(rusqlite::params_from_iter(bound_refs), |row| {
            row.get::<_, i64>(0)
        })
        .map_err(|err| err.to_string())?
        .max(0) as u64;
    Ok(total)
}

/// Returns the categories known to the local DB so the Resource List page
/// can populate its category filter dropdown.
///
/// Read directly from the (small) `categories` table instead of joining
/// through the multi-million-row `resources` table. The trade-off is that
/// this lists *all* seeded categories, even ones no resource currently
/// uses — a tiny cosmetic cost for an instant query.
#[tauri::command]
pub(crate) fn list_resource_filter_options(
    db: State<'_, Db>,
) -> Result<ResourceListFilterOptions, String> {
    let conn = db.read().map_err(|err| err.to_string())?;

    let mut categories: Vec<String> = {
        let mut stmt = conn
            .prepare(
                "SELECT name FROM categories \
                 WHERE name IS NOT NULL AND name <> ''",
            )
            .map_err(|err| err.to_string())?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|err| err.to_string())?;
        let mut v: Vec<String> = Vec::new();
        for row in rows {
            v.push(row.map_err(|err| err.to_string())?);
        }
        v
    };
    categories.sort_by_key(|a| a.to_lowercase());

    Ok(ResourceListFilterOptions { categories })
}

/// Returns every indexed `(package, internal_path)` pair sharing a given
/// CRC32. Drives the right-side detail panel on the Resource List page.
#[tauri::command]
pub(crate) fn list_resource_duplicates(
    crc32: u32,
    db: State<'_, Db>,
) -> Result<Vec<ResourceDuplicateRef>, String> {
    let conn = db.read().map_err(|err| err.to_string())?;

    let mut stmt = conn
        .prepare(
            "SELECT r.package_id, p.file_path, r.internal_path, r.size \
             FROM resources r \
             LEFT JOIN packages p ON p.package_id = r.package_id \
             WHERE r.crc32 = ?1 \
             ORDER BY r.package_id COLLATE NOCASE, r.internal_path COLLATE NOCASE",
        )
        .map_err(|err| err.to_string())?;
    let rows = stmt
        .query_map(rusqlite::params![crc32 as i64], |row| {
            let package_id: String = row.get(0)?;
            let package_file: Option<String> = row.get(1)?;
            let internal_path: String = row.get(2)?;
            let size: i64 = row.get(3)?;
            Ok(ResourceDuplicateRef {
                package_id,
                package_file: package_file.unwrap_or_default(),
                internal_path,
                size: size.max(0) as u64,
            })
        })
        .map_err(|err| err.to_string())?;

    let mut out: Vec<ResourceDuplicateRef> = Vec::new();
    for row in rows {
        out.push(row.map_err(|err| err.to_string())?);
    }
    Ok(out)
}

fn finish_db_find_task(
    tasks: &Arc<Mutex<HashMap<u64, ProgressPayload>>>,
    task_id: u64,
    result: Result<DbFindResponse>,
) {
    if let Ok(mut guard) = tasks.lock() {
        if let Some(task) = guard.get_mut(&task_id) {
            task.done = true;
            task.progress = 1.0;
            match result {
                Ok(payload) => {
                    task.phase = "db_find_complete".to_string();
                    task.message = "Lookup completed".to_string();
                    task.db_find_result = Some(payload);
                    task.error = None;
                }
                Err(err) => {
                    task.phase = "db_find_failed".to_string();
                    task.message = "Lookup failed".to_string();
                    task.error = Some(err.to_string());
                }
            }
        }
    }
}

struct HarvestedResource {
    package_id: String,
    file_path: String,
    internal_path: String,
    size: u64,
    crc32: u32,
}

fn harvest_zip_crcs(
    package_id: &str,
    file_path: &str,
    include_vap: bool,
    out: &mut Vec<HarvestedResource>,
) -> Result<()> {
    let archive_file = fs::File::open(file_path)
        .map_err(|err| anyhow::anyhow!("failed to open {}: {}", file_path, err))?;
    let mut archive = ZipArchive::new(archive_file)
        .map_err(|err| anyhow::anyhow!("failed to read zip {}: {}", file_path, err))?;
    let archive_len = archive.len();
    out.reserve(archive_len);
    for index in 0..archive_len {
        let entry = match archive.by_index(index) {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        if entry.is_dir() {
            continue;
        }
        let raw_name = entry.name();
        // Skip .vap on the raw bytes to avoid the to_ascii_lowercase()
        // allocation per entry — common ZIPs hold many entries, and the
        // tail check is cheap and allocation-free.
        if !include_vap && raw_name.len() >= 4 {
            let tail = &raw_name.as_bytes()[raw_name.len() - 4..];
            if tail.eq_ignore_ascii_case(b".vap") {
                continue;
            }
        }
        // ZIP spec mandates forward slashes; only allocate a fresh String
        // when a back-slash actually needs to be rewritten.
        let internal_path = if raw_name.contains('\\') {
            raw_name.replace('\\', "/")
        } else {
            raw_name.to_string()
        };
        out.push(HarvestedResource {
            package_id: package_id.to_string(),
            file_path: file_path.to_string(),
            internal_path,
            size: entry.size(),
            crc32: entry.crc32(),
        });
    }
    Ok(())
}

fn collect_local_var_files(input_dir: &Path) -> Result<Vec<std::path::PathBuf>> {
    let mut out = Vec::new();
    walk_var_files(input_dir, &mut out)?;
    Ok(out)
}

/// Walk multiple roots for .var files, deduped by canonical path (first root
/// wins). Mirrors `collect_var_files_multi` (the cache-bearing scan path) but
/// for the lightweight tasks that use the recursive `walk_var_files` directly.
fn collect_local_var_files_multi(roots: &[PathBuf]) -> Result<Vec<std::path::PathBuf>> {
    let mut out = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    for root in roots {
        let mut files = Vec::new();
        walk_var_files(root, &mut files)?;
        for path in files {
            let canonical = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
            if seen.insert(canonical) {
                out.push(path);
            }
        }
    }
    Ok(out)
}

fn walk_var_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) -> Result<()> {
    let entries = fs::read_dir(dir)
        .map_err(|err| anyhow::anyhow!("failed to read directory {}: {}", dir.display(), err))?;
    for entry in entries {
        let entry = entry
            .map_err(|err| anyhow::anyhow!("failed to enumerate {}: {}", dir.display(), err))?;
        let path = entry.path();
        if path.is_dir() {
            walk_var_files(&path, out)?;
            continue;
        }
        let is_var = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("var"))
            .unwrap_or(false);
        if is_var {
            out.push(path);
        }
    }
    Ok(())
}

fn target_var_source(
    target_var_path: Option<&str>,
) -> Result<(String, String)> {
    let path = target_var_path
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("target_var_path is required"))?;
    let pid = Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow::anyhow!("invalid target var path"))?
        .to_string();
    Ok((pid, path.to_string()))
}

fn build_local_crc_map(
    tasks: &Arc<Mutex<HashMap<u64, ProgressPayload>>>,
    task_id: u64,
    input_dir: &str,
    additional_dirs: &[PathBuf],
    include_vap: bool,
    exclude_path: &str,
) -> Result<HashMap<u32, Vec<DbFindRef>>> {
    let dir = Path::new(input_dir);
    let roots = crate::scan::scan_roots(dir, additional_dirs);
    let files = collect_local_var_files_multi(&roots)?;
    let total = files.len().max(1);
    let mut map: HashMap<u32, Vec<DbFindRef>> = HashMap::new();
    let canonical_exclude = canonicalize_lossy(exclude_path);
    for (idx, file) in files.iter().enumerate() {
        if canonicalize_lossy(&file.display().to_string()) == canonical_exclude {
            continue;
        }
        let progress = 0.42 + (idx as f64 / total as f64) * 0.55;
        let pid = file
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();
        set_task_progress(
            tasks,
            task_id,
            "db_find_match",
            progress,
            format!("Reading {} ({}/{})", pid, idx + 1, total),
        );
        let mut harvested: Vec<HarvestedResource> = Vec::new();
        if harvest_zip_crcs(&pid, &file.display().to_string(), include_vap, &mut harvested).is_err()
        {
            continue;
        }
        for r in harvested {
            map.entry(r.crc32).or_default().push(DbFindRef {
                package_id: r.package_id,
                file_path: r.file_path,
                internal_path: r.internal_path,
                size: r.size as i64,
            });
        }
    }
    Ok(map)
}

fn canonicalize_lossy(path: &str) -> String {
    Path::new(path)
        .canonicalize()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| path.to_string())
}

fn run_db_find_task(
    tasks: &Arc<Mutex<HashMap<u64, ProgressPayload>>>,
    task_id: u64,
    db: &Db,
    request: DbFindRequest,
) -> Result<DbFindResponse> {
    let mode = request.mode.clone();
    set_task_progress(tasks, task_id, "db_find_harvest", 0.02, "Harvesting source CRCs");

    // Two source paths: harvest CRCs from the indexed resources for a given
    // package_id (lets us analyze packages whose .var isn't on disk, e.g.
    // rows opened from a DB-mode listing), or open the .var file directly.
    // Picking source_package_id wins over target_var_path when both are set.
    let source_pid_opt = request
        .source_package_id
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let (source_pid, source_path, harvested): (String, String, Vec<HarvestedResource>) =
        if let Some(pid) = source_pid_opt {
            set_task_progress(
                tasks,
                task_id,
                "db_find_harvest",
                0.10,
                format!("Reading DB resources for {}", pid),
            );
            let conn = db.read()?;
            let stored_path: String = conn
                .query_row(
                    "SELECT file_path FROM packages WHERE package_id = ?1",
                    rusqlite::params![&pid],
                    |row| row.get::<_, String>(0),
                )
                .map_err(|err| match err {
                    rusqlite::Error::QueryReturnedNoRows => {
                        anyhow::anyhow!("package {} is not indexed in the local database", pid)
                    }
                    other => anyhow::anyhow!("failed to read package row for {}: {}", pid, other),
                })?;
            let resources = db::load_resources_for_package(&conn, &pid, &stored_path)?;
            drop(conn);
            let h: Vec<HarvestedResource> = resources
                .into_iter()
                .filter_map(|r| {
                    r.crc32.map(|crc| HarvestedResource {
                        package_id: r.package_id,
                        file_path: r.package_file,
                        internal_path: r.internal_path,
                        size: r.size,
                        crc32: crc,
                    })
                })
                .collect();
            (pid, stored_path, h)
        } else {
            let (pid, path) = target_var_source(request.target_var_path.as_deref())?;
            set_task_progress(
                tasks,
                task_id,
                "db_find_harvest",
                0.10,
                format!("Reading {}", pid),
            );
            let mut h: Vec<HarvestedResource> = Vec::new();
            harvest_zip_crcs(&pid, &path, request.include_vap, &mut h)?;
            (pid, path, h)
        };
    let source_resources = harvested.len();
    if source_resources == 0 {
        return Ok(DbFindResponse {
            mode,
            sources_scanned: 1,
            source_resources: 0,
            groups: Vec::new(),
        });
    }

    let mut unique_crcs: Vec<u32> = harvested.iter().map(|r| r.crc32).collect();
    unique_crcs.sort_unstable();
    unique_crcs.dedup();

    let exclude: HashSet<String> = [source_pid.clone()].into_iter().collect();

    let matches: HashMap<u32, Vec<DbFindRef>> = match mode.as_str() {
        "database" => {
            set_task_progress(
                tasks,
                task_id,
                "db_find_match",
                0.42,
                format!(
                    "Querying {} unique CRCs across the catalog",
                    unique_crcs.len()
                ),
            );
            let conn = db.read()?;
            db::find_crc_matches_bulk(&conn, &unique_crcs, &exclude, |processed, total| {
                let chunk_progress = if total == 0 {
                    1.0
                } else {
                    processed as f64 / total as f64
                };
                let overall = 0.42 + chunk_progress * 0.55;
                set_task_progress(
                    tasks,
                    task_id,
                    "db_find_match",
                    overall,
                    format!("Matched {}/{} CRCs", processed, total),
                );
            })?
        }
        "local" => {
            let dir = request
                .input_dir
                .as_deref()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| anyhow::anyhow!("input_dir is required in local mode"))?;
            let crc_map = build_local_crc_map(
                tasks,
                task_id,
                dir,
                &parse_additional_dirs(&request.additional_input_dirs),
                request.include_vap,
                &source_path,
            )?;
            let mut filtered: HashMap<u32, Vec<DbFindRef>> = HashMap::new();
            for crc in &unique_crcs {
                if let Some(rows) = crc_map.get(crc) {
                    if !rows.is_empty() {
                        filtered.insert(*crc, rows.clone());
                    }
                }
            }
            filtered
        }
        other => return Err(anyhow::anyhow!("unknown mode: {}", other)),
    };

    set_task_progress(tasks, task_id, "db_find_assemble", 0.97, "Assembling results");

    let mut groups: HashMap<u32, DbFindGroup> = HashMap::new();
    for resource in &harvested {
        let match_rows = matches.get(&resource.crc32);
        let has_matches = match_rows.map(|r| !r.is_empty()).unwrap_or(false);
        if !has_matches && !request.include_unshared {
            continue;
        }
        let entry = groups
            .entry(resource.crc32)
            .or_insert_with(|| DbFindGroup {
                crc32: resource.crc32,
                crc32_hex: format!("{:08X}", resource.crc32),
                size: resource.size as i64,
                source_refs: Vec::new(),
                db_matches: Vec::new(),
                db_matches_truncated: 0,
                reclaimable_bytes: 0,
            });
        entry.source_refs.push(DbFindRef {
            package_id: resource.package_id.clone(),
            file_path: resource.file_path.clone(),
            internal_path: resource.internal_path.clone(),
            size: resource.size as i64,
        });
        if has_matches && entry.db_matches.is_empty() {
            let rows = match_rows.unwrap();
            let total_matches = rows.len();
            let take = total_matches.min(DB_FIND_DB_MATCHES_CAP);
            entry.db_matches.extend(rows.iter().take(take).cloned());
            entry.db_matches_truncated = (total_matches - take) as i64;
        }
    }

    for group in groups.values_mut() {
        // Manifest-imported rows land in `resources` with size=0 because the
        // manifest format only carries CRC32. When that happens the source
        // size is 0 but the DB matches (read live from disk via
        // `harvest_zip_crcs`, or from a fully-indexed sibling row) carry the
        // true size — CRC32 collisions on identical content imply identical
        // sizes, so adopt the largest non-zero value as the group size and
        // propagate it into any source_refs that came in zero.
        if group.size == 0 {
            let from_matches = group
                .db_matches
                .iter()
                .map(|m| m.size)
                .filter(|s| *s > 0)
                .max()
                .unwrap_or(0);
            if from_matches > 0 {
                group.size = from_matches;
                for sref in group.source_refs.iter_mut() {
                    if sref.size == 0 {
                        sref.size = from_matches;
                    }
                }
            }
        }

        // Reclaimable bytes = source resource size * (number of catalog matches).
        // CRC32 is the user-chosen dedup key, so this is a theoretical reclaim
        // assuming the matched bytes are also identical.
        let match_count = group.db_matches.len() as i64 + group.db_matches_truncated;
        group.reclaimable_bytes = group.size * match_count;
    }

    let mut group_vec: Vec<DbFindGroup> = groups.into_values().collect();
    group_vec.sort_by(|a, b| {
        let a_count = a.db_matches.len() as i64 + a.db_matches_truncated;
        let b_count = b.db_matches.len() as i64 + b.db_matches_truncated;
        b_count.cmp(&a_count).then_with(|| b.size.cmp(&a.size))
    });

    Ok(DbFindResponse {
        mode,
        sources_scanned: 1,
        source_resources,
        groups: group_vec,
    })
}

#[tauri::command]
pub(crate) fn start_db_find_task(
    request: DbFindRequest,
    state: State<'_, AppState>,
    db: State<'_, Db>,
) -> Result<TaskHandle, String> {
    let task_id = state.next_task_id.fetch_add(1, Ordering::SeqCst) + 1;
    {
        let mut guard = state
            .tasks
            .lock()
            .map_err(|_| "task state poisoned".to_string())?;
        guard.insert(
            task_id,
            new_progress_payload("db_find_starting", "Starting database lookup"),
        );
    }

    let tasks = Arc::clone(&state.tasks);
    let db = db.inner().clone();
    thread::spawn(move || {
        let result = run_db_find_task(&tasks, task_id, &db, request);
        finish_db_find_task(&tasks, task_id, result);
    });

    Ok(TaskHandle { id: task_id })
}

// ----------------------------------------------------------------------------
// Reclaim Space — scan a folder of local VARs, aggregate the unique CRC set,
// then find DB packages whose indexed resources cover the largest byte share.
// Used by the "Reclaim Space" page: the candidates the user could download to
// replace local resources with dependency links, reclaiming duplicate bytes.
// ----------------------------------------------------------------------------

const RECLAIM_CANDIDATE_CAP: usize = 200;

fn finish_reclaim_scan_task(
    tasks: &Arc<Mutex<HashMap<u64, ProgressPayload>>>,
    task_id: u64,
    result: Result<ReclaimScanResponse>,
) {
    if let Ok(mut guard) = tasks.lock() {
        if let Some(task) = guard.get_mut(&task_id) {
            task.done = true;
            task.progress = 1.0;
            match result {
                Ok(payload) => {
                    task.phase = "reclaim_complete".to_string();
                    task.message = format!(
                        "Scanned {} VAR{}, {} unique resource{}, {} candidate{}",
                        payload.local_vars_scanned,
                        if payload.local_vars_scanned == 1 { "" } else { "s" },
                        payload.local_unique_resources,
                        if payload.local_unique_resources == 1 { "" } else { "s" },
                        payload.candidates.len(),
                        if payload.candidates.len() == 1 { "" } else { "s" },
                    );
                    task.reclaim_scan_result = Some(payload);
                    task.error = None;
                }
                Err(err) => {
                    task.phase = "reclaim_failed".to_string();
                    task.message = "Reclaim scan failed".to_string();
                    task.error = Some(err.to_string());
                }
            }
        }
    }
}

fn run_reclaim_scan_task(
    tasks: &Arc<Mutex<HashMap<u64, ProgressPayload>>>,
    task_id: u64,
    db: &Db,
    request: ReclaimScanRequest,
) -> Result<ReclaimScanResponse> {
    let folder = request.folder.trim();
    if folder.is_empty() {
        return Err(anyhow::anyhow!("folder is required"));
    }
    let dir = Path::new(folder);
    if !dir.is_dir() {
        return Err(anyhow::anyhow!("not a directory: {}", dir.display()));
    }

    set_task_progress(tasks, task_id, "reclaim_walk", 0.01, "Listing .var files");
    let roots = crate::scan::scan_roots(dir, &parse_additional_dirs(&request.additional_folders));
    let files = collect_local_var_files_multi(&roots)?;
    let total_files = files.len();
    if total_files == 0 {
        return Ok(ReclaimScanResponse {
            local_vars_scanned: 0,
            local_unique_resources: 0,
            local_unique_bytes: 0,
            candidates: Vec::new(),
        });
    }

    // Favorite creators are protected: their local VARs are skipped entirely
    // in the walk so their CRCs never enter `crc_info`, no DB lookup is done
    // against them, and they never appear in the reclaim results.
    let favorite_creator_names: HashSet<String> = {
        let conn = db.read()?;
        db::get_favorite_creator_names(&conn)?
    };

    // Aggregate per-CRC info across every local VAR:
    //   size  — largest non-zero ZIP-entry size seen for the CRC (the
    //           ground-truth disk size, read straight from the local zips
    //           rather than trusting DB indexed values which can drift).
    //   count — number of times the CRC appears across local VARs. This is
    //           the multiplier for "bytes reclaimable" — a CRC duplicated
    //           across 10 local VARs is worth 10× its size if you offload
    //           it to a single downloaded candidate. Mirrors VAR Details'
    //           `match_count` weighting in its "local" Scan mode so the two
    //           pages report consistent numbers.
    let mut crc_info: HashMap<u32, (u64, u64)> = HashMap::new();
    let mut local_package_ids: HashSet<String> = HashSet::new();
    let mut files_scanned: usize = 0;
    for (idx, file) in files.iter().enumerate() {
        let pid = file
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();
        let progress = 0.02 + (idx as f64 / total_files as f64) * 0.55;
        set_task_progress(
            tasks,
            task_id,
            "reclaim_harvest",
            progress,
            format!("Reading {} ({}/{})", pid, idx + 1, total_files),
        );

        // Skip favorited creators' VARs entirely so they're invisible to
        // reclaim — neither contributing CRCs nor adding to local_package_ids.
        if let Some(creator) = crate::naming::creator_from_package_id(&pid) {
            if favorite_creator_names.contains(creator) {
                files_scanned += 1;
                continue;
            }
        }
        local_package_ids.insert(pid.clone());

        let mut harvested: Vec<HarvestedResource> = Vec::new();
        if harvest_zip_crcs(
            &pid,
            &file.display().to_string(),
            request.include_vap,
            &mut harvested,
        )
        .is_err()
        {
            files_scanned += 1;
            continue;
        }
        for r in harvested {
            crc_info
                .entry(r.crc32)
                .and_modify(|(s, c)| {
                    if r.size > *s {
                        *s = r.size;
                    }
                    *c += 1;
                })
                .or_insert((r.size, 1));
        }
        files_scanned += 1;
    }

    let local_unique_resources = crc_info.len();
    // Unique footprint (size summed once per distinct CRC) for the summary tile.
    let local_unique_bytes: i64 = crc_info.values().map(|(s, _)| *s as i64).sum();
    // Total local disk footprint of resources (size × occurrence count) — the
    // denominator for Coverage %. Only CRCs duplicated locally (count > 1) can
    // ever be reclaimed: a CRC that lives in just one VAR has nothing to dedupe
    // against, so replacing it with a downloaded copy would save zero bytes.
    // Excluding those keeps Coverage % a percentage of *reclaimable* bytes
    // rather than total footprint, so 100% remains achievable.
    let local_total_footprint: i64 = crc_info
        .values()
        .filter(|(_, c)| *c > 1)
        .map(|(s, c)| *s as i64 * *c as i64)
        .sum();

    if local_unique_resources == 0 {
        return Ok(ReclaimScanResponse {
            local_vars_scanned: files_scanned,
            local_unique_resources: 0,
            local_unique_bytes: 0,
            candidates: Vec::new(),
        });
    }

    // Only query the DB for CRCs that appear in more than one local VAR.
    // Unique-to-one resources can't be reclaimed (no duplicate to collapse),
    // so looking them up just inflates the catalog scan with no payoff.
    let unique_crcs: Vec<u32> = crc_info
        .iter()
        .filter(|(_, (_, c))| *c > 1)
        .map(|(crc, _)| *crc)
        .collect();

    if unique_crcs.is_empty() {
        return Ok(ReclaimScanResponse {
            local_vars_scanned: files_scanned,
            local_unique_resources,
            local_unique_bytes,
            candidates: Vec::new(),
        });
    }

    set_task_progress(
        tasks,
        task_id,
        "reclaim_match",
        0.60,
        format!(
            "Querying {} unique CRCs across the catalog",
            unique_crcs.len()
        ),
    );

    let conn = db.read()?;
    let matches = db::find_crc_matches_bulk(
        &conn,
        &unique_crcs,
        &local_package_ids,
        |processed, total| {
            let chunk_progress = if total == 0 {
                1.0
            } else {
                processed as f64 / total as f64
            };
            let overall = 0.60 + chunk_progress * 0.30;
            set_task_progress(
                tasks,
                task_id,
                "reclaim_match",
                overall,
                format!("Matched {}/{} CRCs", processed, total),
            );
        },
    )?;

    set_task_progress(tasks, task_id, "reclaim_aggregate", 0.92, "Aggregating candidates");

    // Per-candidate accumulator: (file_path, count, bytes).
    #[derive(Default)]
    struct CandidateAcc {
        file_path: String,
        count: u64,
        bytes: i64,
    }
    let mut acc: HashMap<String, CandidateAcc> = HashMap::new();
    for (crc, rows) in &matches {
        let (local_size, local_count) = crc_info
            .get(crc)
            .copied()
            .unwrap_or((0u64, 0u64));
        // The reclaim contribution per matching CRC is the local-harvest size
        // times the number of local VARs that contain this CRC. Equivalent to
        // VAR Details' `group.size * match_count` in "local" mode but read
        // entirely from local zips, so DB size drift / CRC32 collisions on
        // unrelated-but-colliding indexed rows can't inflate the number.
        let reclaim_per_match = local_size as i64 * local_count as i64;
        // Each candidate package counted once per matched CRC. Multiple rows
        // for the same package_id (re-export of the same file at different
        // internal paths) shouldn't inflate count, so dedup per package.
        let mut seen_for_crc: HashSet<&str> = HashSet::new();
        for row in rows {
            if !seen_for_crc.insert(row.package_id.as_str()) {
                continue;
            }
            let entry = acc.entry(row.package_id.clone()).or_insert_with(|| CandidateAcc {
                file_path: row.file_path.clone(),
                count: 0,
                bytes: 0,
            });
            if entry.file_path.is_empty() {
                entry.file_path = row.file_path.clone();
            }
            entry.count += 1;
            entry.bytes += reclaim_per_match;
        }
    }

    // Look up creator names in a single query for the candidate package_ids.
    let creator_map: HashMap<String, String> = if acc.is_empty() {
        HashMap::new()
    } else {
        let pids: Vec<&str> = acc.keys().map(|s| s.as_str()).collect();
        let placeholders = vec!["?"; pids.len()].join(",");
        let sql = format!(
            "SELECT p.package_id, c.name
             FROM packages p
             JOIN creators c ON c.creator_id = p.creator_id
             WHERE p.package_id IN ({})",
            placeholders
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut map: HashMap<String, String> = HashMap::new();
        let rows = stmt
            .query_map(rusqlite::params_from_iter(pids.iter().copied()), |row| {
                let pid: String = row.get(0)?;
                let name: String = row.get(1)?;
                Ok((pid, name))
            })?;
        for (pid, name) in rows.flatten() {
            map.insert(pid, name);
        }
        map
    };
    drop(conn);

    // Coverage % compares each candidate's reclaim against the total local
    // disk footprint (size × occurrence count), so the metric and the
    // denominator share the same weighting. 100% means a candidate covers
    // every reclaimable byte you currently store locally.
    let local_bytes_for_pct = local_total_footprint.max(1) as f64;
    let mut candidates: Vec<ReclaimCandidate> = acc
        .into_iter()
        .map(|(package_id, a)| {
            let coverage_pct = (a.bytes as f64 / local_bytes_for_pct) * 100.0;
            ReclaimCandidate {
                creator_name: creator_map.get(&package_id).cloned(),
                package_id,
                file_path: a.file_path,
                matched_resource_count: a.count,
                matched_bytes: a.bytes,
                coverage_pct,
            }
        })
        .collect();

    candidates.sort_by(|a, b| {
        b.matched_bytes
            .cmp(&a.matched_bytes)
            .then_with(|| b.matched_resource_count.cmp(&a.matched_resource_count))
            .then_with(|| a.package_id.cmp(&b.package_id))
    });
    candidates.truncate(RECLAIM_CANDIDATE_CAP);

    Ok(ReclaimScanResponse {
        local_vars_scanned: files_scanned,
        local_unique_resources,
        local_unique_bytes,
        candidates,
    })
}

#[tauri::command]
pub(crate) fn start_reclaim_scan_task(
    request: ReclaimScanRequest,
    state: State<'_, AppState>,
    db: State<'_, Db>,
) -> Result<TaskHandle, String> {
    let task_id = state.next_task_id.fetch_add(1, Ordering::SeqCst) + 1;
    {
        let mut guard = state
            .tasks
            .lock()
            .map_err(|_| "task state poisoned".to_string())?;
        guard.insert(
            task_id,
            new_progress_payload("reclaim_starting", "Starting reclaim scan"),
        );
    }

    let tasks = Arc::clone(&state.tasks);
    let db = db.inner().clone();
    thread::spawn(move || {
        let result = run_reclaim_scan_task(&tasks, task_id, &db, request);
        finish_reclaim_scan_task(&tasks, task_id, result);
    });

    Ok(TaskHandle { id: task_id })
}

// ----------------------------------------------------------------------------
// Unique Resources — walks a folder of .var files locally, deduplicates every
// resource by (crc32, size), and returns the unique set with every source VAR
// that contains each one. No DB writes, no DB reads: a pure in-memory scan.
// Mirrors the Reclaim Space task trio but stripped of catalog matching.
// ----------------------------------------------------------------------------

fn finish_unique_resources_task(
    tasks: &Arc<Mutex<HashMap<u64, ProgressPayload>>>,
    task_id: u64,
    result: Result<UniqueResourcesResponse>,
) {
    if let Ok(mut guard) = tasks.lock() {
        if let Some(task) = guard.get_mut(&task_id) {
            task.done = true;
            task.progress = 1.0;
            match result {
                Ok(payload) => {
                    task.phase = "unique_resources_complete".to_string();
                    task.message = format!(
                        "Scanned {} VAR{}, {} unique resource{}",
                        payload.vars_scanned,
                        if payload.vars_scanned == 1 { "" } else { "s" },
                        payload.unique_resources,
                        if payload.unique_resources == 1 { "" } else { "s" },
                    );
                    task.unique_resources_result = Some(payload);
                    task.error = None;
                }
                Err(err) => {
                    task.phase = "unique_resources_failed".to_string();
                    task.message = "Unique resources scan failed".to_string();
                    task.error = Some(err.to_string());
                }
            }
        }
    }
}

fn run_unique_resources_task(
    tasks: &Arc<Mutex<HashMap<u64, ProgressPayload>>>,
    task_id: u64,
    request: UniqueResourcesRequest,
) -> Result<UniqueResourcesResponse> {
    let folder = request.folder.trim();
    if folder.is_empty() {
        return Err(anyhow::anyhow!("folder is required"));
    }
    let dir = Path::new(folder);
    if !dir.is_dir() {
        return Err(anyhow::anyhow!("not a directory: {}", dir.display()));
    }

    set_task_progress(tasks, task_id, "unique_resources_walk", 0.01, "Listing .var files");
    let roots = crate::scan::scan_roots(dir, &parse_additional_dirs(&request.additional_folders));
    let files = collect_local_var_files_multi(&roots)?;
    let total_files = files.len();
    if total_files == 0 {
        return Ok(UniqueResourcesResponse {
            vars_scanned: 0,
            unique_resources: 0,
            total_unique_bytes: 0,
            total_combined_bytes: 0,
            reclaimable_bytes: 0,
            resources: Vec::new(),
        });
    }

    // Keyed by (crc32, size) so two resources with the same bytes — even under
    // different internal paths in different VARs — collapse to one row. This
    // matches the dedup convention used everywhere else in the app.
    let mut agg: HashMap<(u32, u64), UniqueResource> = HashMap::new();

    // Parallel harvest: ZIP central-directory reads are I/O-bound and each VAR
    // file is independent, so spin up a small worker pool that round-robins
    // file indices via an AtomicUsize dispenser. Workers append into their
    // own thread-local Vec to avoid lock contention on the shared map; the
    // merge into `agg` runs single-threaded afterwards (cheap relative to
    // harvest cost — HashMap inserts are O(1)).
    let num_workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, 8)
        .min(total_files);
    let next_index = AtomicUsize::new(0);
    let done_count = AtomicUsize::new(0);
    let include_vap = request.include_vap;

    let per_worker: Vec<Vec<HarvestedResource>> = std::thread::scope(|s| {
        let mut handles = Vec::with_capacity(num_workers);
        for _ in 0..num_workers {
            let files_ref = &files;
            let next_index_ref = &next_index;
            let done_count_ref = &done_count;
            let tasks_ref = tasks;
            handles.push(s.spawn(move || {
                let mut local: Vec<HarvestedResource> = Vec::new();
                loop {
                    let i = next_index_ref.fetch_add(1, Ordering::Relaxed);
                    if i >= total_files {
                        break;
                    }
                    let file = &files_ref[i];
                    let pid = file
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("unknown")
                        .to_string();
                    let file_path = file.display().to_string();
                    // Unreadable VARs are skipped rather than failing the whole
                    // scan — match prior serial behavior.
                    let _ = harvest_zip_crcs(&pid, &file_path, include_vap, &mut local);
                    let n = done_count_ref.fetch_add(1, Ordering::Relaxed) + 1;
                    let progress = 0.02 + (n as f64 / total_files as f64) * 0.93;
                    set_task_progress(
                        tasks_ref,
                        task_id,
                        "unique_resources_harvest",
                        progress,
                        format!("Reading {} ({}/{})", pid, n, total_files),
                    );
                }
                local
            }));
        }
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_default())
            .collect()
    });
    let files_scanned = done_count.load(Ordering::Relaxed);

    for batch in per_worker {
        for h in batch {
            let key = (h.crc32, h.size);
            let entry = agg.entry(key).or_insert_with(|| UniqueResource {
                crc32: h.crc32,
                crc32_hex: format!("{:08x}", h.crc32),
                representative_path: h.internal_path.clone(),
                unique_size: h.size,
                source_count: 0,
                combined_size: 0,
                sources: Vec::new(),
            });
            entry.sources.push(UniqueResourceSource {
                package_id: h.package_id,
                file_path: h.file_path,
                internal_path: h.internal_path,
            });
            entry.source_count += 1;
        }
    }

    set_task_progress(
        tasks,
        task_id,
        "unique_resources_finalize",
        0.97,
        "Aggregating results",
    );

    let mut resources: Vec<UniqueResource> = agg.into_values().collect();
    for r in resources.iter_mut() {
        r.combined_size = r.unique_size.saturating_mul(r.source_count);
    }
    // Sort by combined_size desc so the biggest space hogs surface first.
    // Tiebreak by source_count desc, then crc32_hex for stable ordering.
    resources.sort_by(|a, b| {
        b.combined_size
            .cmp(&a.combined_size)
            .then_with(|| b.source_count.cmp(&a.source_count))
            .then_with(|| a.crc32_hex.cmp(&b.crc32_hex))
    });

    let total_unique_bytes: u64 = resources.iter().map(|r| r.unique_size).sum();
    let total_combined_bytes: u64 = resources.iter().map(|r| r.combined_size).sum();
    let reclaimable_bytes = total_combined_bytes.saturating_sub(total_unique_bytes);

    Ok(UniqueResourcesResponse {
        vars_scanned: files_scanned,
        unique_resources: resources.len(),
        total_unique_bytes,
        total_combined_bytes,
        reclaimable_bytes,
        resources,
    })
}

#[tauri::command]
pub(crate) fn start_unique_resources_task(
    request: UniqueResourcesRequest,
    state: State<'_, AppState>,
) -> Result<TaskHandle, String> {
    let task_id = state.next_task_id.fetch_add(1, Ordering::SeqCst) + 1;
    {
        let mut guard = state
            .tasks
            .lock()
            .map_err(|_| "task state poisoned".to_string())?;
        guard.insert(
            task_id,
            new_progress_payload("unique_resources_starting", "Starting unique resources scan"),
        );
    }

    let tasks = Arc::clone(&state.tasks);
    thread::spawn(move || {
        let result = run_unique_resources_task(&tasks, task_id, request);
        finish_unique_resources_task(&tasks, task_id, result);
    });

    Ok(TaskHandle { id: task_id })
}

// ----------------------------------------------------------------------------
// Dependency Check — for a chosen target VAR, walk a folder of .var files
// and report every package that depends on the target. "Depends" is decided
// by either of:
//   * the package's meta.json `dependencies` map contains a key whose base
//     equals the target's base (versions and `.latest` collapse to the same
//     base), or
//   * (deep scan only) any payload text file inside the package contains a
//     `<base>.<version>:/<path>` substring pointing at the target.
//
// The target package_id itself is always excluded from the result set —
// a VAR is never reported as depending on itself.
// ----------------------------------------------------------------------------

/// Strips a trailing `.N` (any integer) or `.latest` from a package_id so the
/// remainder can be compared across versions. "Creator.Pkg.3" and
/// "Creator.Pkg.latest" both collapse to "Creator.Pkg". Whitespace is
/// trimmed; case is preserved for display, comparisons are case-insensitive.
fn dependency_package_base(id: &str) -> String {
    crate::naming::package_base(id.trim()).to_string()
}

/// True when `dep_key` (a key inside `meta.json` `dependencies`) refers to the
/// same package family as `target_base`. The base of `dep_key` (its name
/// without the trailing version segment) is compared case-insensitively.
fn dependency_key_matches_base(dep_key: &str, target_base_lc: &str) -> bool {
    let dep_base = dependency_package_base(dep_key);
    dep_base.to_ascii_lowercase() == target_base_lc
}

fn finish_dependency_check_task(
    tasks: &Arc<Mutex<HashMap<u64, ProgressPayload>>>,
    task_id: u64,
    result: Result<DependencyCheckResponse>,
) {
    if let Ok(mut guard) = tasks.lock() {
        if let Some(task) = guard.get_mut(&task_id) {
            task.done = true;
            task.progress = 1.0;
            match result {
                Ok(payload) => {
                    task.phase = "dependency_check_complete".to_string();
                    task.message = format!(
                        "Found {} dependent{} across {} VAR{}",
                        payload.dependents.len(),
                        if payload.dependents.len() == 1 { "" } else { "s" },
                        payload.vars_scanned,
                        if payload.vars_scanned == 1 { "" } else { "s" },
                    );
                    task.dependency_check_result = Some(payload);
                    task.error = None;
                }
                Err(err) => {
                    task.phase = "dependency_check_failed".to_string();
                    task.message = "Dependency check failed".to_string();
                    task.error = Some(err.to_string());
                }
            }
        }
    }
}

/// Walks one .var file looking for evidence that it depends on `target_base`.
/// Returns `None` when the archive can't be opened or is malformed — the
/// outer loop skips those rather than failing the whole scan, mirroring the
/// behavior of `harvest_zip_crcs`.
fn inspect_var_for_dependency(
    file_path: &Path,
    package_id: &str,
    target_base_lc: &str,
    target_full_lc: &str,
    deep_scan: bool,
) -> Option<(Vec<String>, Vec<DependencyDeepRef>)> {
    use crate::utils::{decode_text, normalize_zip_path, read_json_bytes};

    let archive_file = fs::File::open(file_path).ok()?;
    let mut archive = ZipArchive::new(archive_file).ok()?;

    // First pass: meta.json — locate by case-insensitive name match.
    let mut meta_keys: Vec<String> = Vec::new();
    let meta_idx = (0..archive.len()).find(|i| {
        archive
            .by_index(*i)
            .ok()
            .map(|e| normalize_zip_path(e.name()).eq_ignore_ascii_case("meta.json"))
            .unwrap_or(false)
    });
    if let Some(idx) = meta_idx {
        if let Ok(mut entry) = archive.by_index(idx) {
            let mut raw = Vec::new();
            if entry.read_to_end(&mut raw).is_ok() {
                if let Ok(meta) = read_json_bytes(&raw, "meta.json") {
                    if let Some(deps) = meta.get("dependencies").and_then(|v| v.as_object()) {
                        for key in deps.keys() {
                            if dependency_key_matches_base(key, target_base_lc) {
                                meta_keys.push(key.clone());
                            }
                        }
                    }
                }
            }
        }
    }

    // Second pass: deep scan over text payloads. Skipped when the caller
    // doesn't ask for it — opening every text entry is the expensive part
    // of this task.
    let mut deep_refs: Vec<DependencyDeepRef> = Vec::new();
    if deep_scan {
        // Substring needle: `<base>.` — every textual ref to the target
        // package starts with the base + dot + version + `:/`. Searching the
        // base alone over-matches (e.g. unrelated text containing the
        // creator name), so the harvester below validates the full shape
        // `<base>.<token>:/` before recording a hit.
        let base_needle = format!("{target_base_lc}.");
        let base_needle_bytes = base_needle.as_bytes();
        let base_len = base_needle_bytes.len();
        // Skip self-references: when a VAR's package_id base equals the
        // target's base it's almost certainly the target itself or a
        // sibling version that ships the same scenes. Either way it's
        // misleading to flag as a dependent.
        let self_base_lc = dependency_package_base(package_id).to_ascii_lowercase();
        let scans_self = self_base_lc == target_base_lc;

        for index in 0..archive.len() {
            // meta.json was already consumed for the meta check; reading it
            // again here for deep scan is harmless but pointless.
            let mut entry = match archive.by_index(index) {
                Ok(e) => e,
                Err(_) => continue,
            };
            if entry.is_dir() {
                continue;
            }
            let name = entry.name().replace('\\', "/");
            let lower_ext = Path::new(&name)
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| format!(".{}", e.to_ascii_lowercase()));
            let is_text = matches!(
                lower_ext.as_deref(),
                Some(".json") | Some(".vam") | Some(".vaj") | Some(".vmi") | Some(".vap")
            );
            if !is_text {
                continue;
            }
            let mut raw = Vec::new();
            if entry.read_to_end(&mut raw).is_err() {
                continue;
            }
            let Some((text, _)) = decode_text(&raw) else {
                continue;
            };
            let lower = text.to_ascii_lowercase();
            let lb = lower.as_bytes();
            let tb = text.as_bytes();
            let mut search_from = 0usize;
            // Linear substring scan rather than building a regex — the needle
            // (`<base>.`) is short and the input is bounded by the size of
            // one text payload inside a VAR, so allocation-free `find` calls
            // are faster than a compiled pattern.
            while let Some(rel) = lower[search_from..].find(&base_needle) {
                let hit = search_from + rel;
                // Require word-boundary on the left so e.g. "MeshedVR" in a
                // longer creator name like "OtherMeshedVR" is rejected.
                if hit > 0 {
                    let prev = lb[hit - 1];
                    let is_word = prev.is_ascii_uppercase()
                        || prev.is_ascii_lowercase()
                        || prev.is_ascii_digit()
                        || prev == b'_'
                        || prev == b'-'
                        || prev == b'.';
                    if is_word {
                        search_from = hit + 1;
                        continue;
                    }
                }
                // Validate `<base>.<token>:/` shape. token may contain
                // letters/digits/underscores/dots — read until `:` and
                // confirm the next char is `/`.
                let after_base = hit + base_len;
                let mut version_end = after_base;
                while version_end < lb.len() {
                    let c = lb[version_end];
                    let is_token = c.is_ascii_lowercase()
                        || c.is_ascii_digit()
                        || c == b'_'
                        || c == b'-'
                        || c == b'.';
                    if !is_token {
                        break;
                    }
                    version_end += 1;
                }
                if version_end >= lb.len() || lb[version_end] != b':' {
                    search_from = hit + 1;
                    continue;
                }
                let slash_pos = version_end + 1;
                if slash_pos >= lb.len() || lb[slash_pos] != b'/' {
                    search_from = hit + 1;
                    continue;
                }
                // Capture the path until a terminator (quote / whitespace /
                // closing brace / etc.) so the UI shows the full reference.
                let mut path_end = slash_pos + 1;
                while path_end < tb.len() {
                    let c = tb[path_end];
                    if c == b'"'
                        || c == b'\''
                        || c == b'\r'
                        || c == b'\n'
                        || c == b'\t'
                        || c == b' '
                        || c == b','
                        || c == b'}'
                        || c == b']'
                    {
                        break;
                    }
                    path_end += 1;
                }
                // Drop trailing whitespace just in case the terminator
                // detection above let a stray space through.
                while path_end > slash_pos + 1
                    && matches!(tb[path_end - 1], b' ' | b'\t')
                {
                    path_end -= 1;
                }
                let ref_path = String::from_utf8_lossy(&tb[hit..path_end]).to_string();
                if scans_self {
                    // Self-reference (the target's own scenes referencing
                    // itself) — skip without recording, but still advance.
                    search_from = path_end.max(hit + 1);
                    continue;
                }
                // Final guard: ensure the exact package id portion isn't
                // some no-op like just `<base>.:/`. We already required at
                // least one token char, so version_end > after_base; that's
                // already enforced by the loop above. Also drop refs whose
                // full prefix equals `target_full_lc` only when callers
                // explicitly asked for exact-version mode — for the
                // base-match policy we keep all versions.
                let _ = target_full_lc;
                deep_refs.push(DependencyDeepRef {
                    internal_path: name.clone(),
                    ref_path,
                });
                search_from = path_end.max(hit + 1);
            }
        }
    }

    Some((meta_keys, deep_refs))
}

fn run_dependency_check_task(
    tasks: &Arc<Mutex<HashMap<u64, ProgressPayload>>>,
    task_id: u64,
    request: DependencyCheckRequest,
) -> Result<DependencyCheckResponse> {
    let folder = request.folder.trim();
    if folder.is_empty() {
        return Err(anyhow::anyhow!("folder is required"));
    }
    let dir = Path::new(folder);
    if !dir.is_dir() {
        return Err(anyhow::anyhow!("not a directory: {}", dir.display()));
    }
    let target_full = request.target_package_id.trim().to_string();
    if target_full.is_empty() {
        return Err(anyhow::anyhow!("target_package_id is required"));
    }
    let target_base = dependency_package_base(&target_full);
    let target_base_lc = target_base.to_ascii_lowercase();
    let target_full_lc = target_full.to_ascii_lowercase();
    let deep_scan = request.deep_scan;

    set_task_progress(
        tasks,
        task_id,
        "dependency_check_walk",
        0.01,
        "Listing .var files",
    );
    let roots = crate::scan::scan_roots(dir, &parse_additional_dirs(&request.additional_folders));
    let files = collect_local_var_files_multi(&roots)?;
    let total_files = files.len();
    if total_files == 0 {
        return Ok(DependencyCheckResponse {
            target_package_id: target_full,
            target_package_base: target_base,
            deep_scan_used: deep_scan,
            vars_scanned: 0,
            dependents: Vec::new(),
        });
    }

    // Parallel walk modeled on `run_unique_resources_task` — each file is
    // independent, the work is mostly ZIP central-directory reads (I/O-bound),
    // and per-thread output buffers avoid lock contention.
    let num_workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, 8)
        .min(total_files);
    let next_index = AtomicUsize::new(0);
    let done_count = AtomicUsize::new(0);

    type WorkerHit = (
        String, // package_id
        std::path::PathBuf,
        Vec<String>,
        Vec<DependencyDeepRef>,
    );

    let per_worker: Vec<Vec<WorkerHit>> = std::thread::scope(|s| {
        let mut handles = Vec::with_capacity(num_workers);
        for _ in 0..num_workers {
            let files_ref: &Vec<std::path::PathBuf> = &files;
            let next_index_ref = &next_index;
            let done_count_ref = &done_count;
            let tasks_ref = tasks;
            let target_full_ref: &str = target_full.as_str();
            let target_base_ref: &str = target_base.as_str();
            let target_base_lc_ref: &str = target_base_lc.as_str();
            let target_full_lc_ref: &str = target_full_lc.as_str();
            handles.push(s.spawn(move || {
                let mut local: Vec<WorkerHit> = Vec::new();
                loop {
                    let i = next_index_ref.fetch_add(1, Ordering::Relaxed);
                    if i >= total_files {
                        break;
                    }
                    let file = &files_ref[i];
                    let pid = file
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("unknown")
                        .to_string();
                    // Skip the target itself — listing it as its own
                    // dependent would be noise.
                    let is_self = pid.eq_ignore_ascii_case(target_full_ref)
                        || dependency_package_base(&pid)
                            .eq_ignore_ascii_case(target_base_ref);
                    let result = if is_self {
                        Some((Vec::new(), Vec::new()))
                    } else {
                        inspect_var_for_dependency(
                            file,
                            &pid,
                            target_base_lc_ref,
                            target_full_lc_ref,
                            deep_scan,
                        )
                    };
                    if let Some((meta_keys, deep_refs)) = result {
                        if !is_self && (!meta_keys.is_empty() || !deep_refs.is_empty()) {
                            local.push((pid.clone(), file.clone(), meta_keys, deep_refs));
                        }
                    }
                    let n = done_count_ref.fetch_add(1, Ordering::Relaxed) + 1;
                    let progress = 0.02 + (n as f64 / total_files as f64) * 0.95;
                    set_task_progress(
                        tasks_ref,
                        task_id,
                        "dependency_check_scan",
                        progress,
                        format!("Checking {} ({}/{})", pid, n, total_files),
                    );
                }
                local
            }));
        }
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_default())
            .collect()
    });
    let files_scanned = done_count.load(Ordering::Relaxed);

    let mut dependents: Vec<DependencyCheckMatch> = Vec::new();
    for batch in per_worker {
        for (pid, path, meta_keys, deep_refs) in batch {
            let metadata = fs::metadata(&path).ok();
            let size_bytes = metadata.as_ref().map(|m| m.len()).unwrap_or(0);
            let modified_ms = metadata
                .as_ref()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64);
            let creator_name = crate::naming::creator_from_package_id(&pid).map(str::to_string);
            dependents.push(DependencyCheckMatch {
                package_id: pid,
                file_path: path.display().to_string(),
                creator_name,
                size_bytes,
                modified_ms,
                meta_match: !meta_keys.is_empty(),
                meta_match_keys: meta_keys,
                deep_refs,
            });
        }
    }

    // Sort: meta-matched VARs first (they're the more authoritative signal),
    // then deep-only matches, then alphabetical package_id for stability.
    dependents.sort_by(|a, b| {
        b.meta_match
            .cmp(&a.meta_match)
            .then_with(|| b.deep_refs.len().cmp(&a.deep_refs.len()))
            .then_with(|| a.package_id.to_ascii_lowercase().cmp(&b.package_id.to_ascii_lowercase()))
    });

    set_task_progress(
        tasks,
        task_id,
        "dependency_check_finalize",
        0.99,
        "Aggregating results",
    );

    Ok(DependencyCheckResponse {
        target_package_id: target_full,
        target_package_base: target_base,
        deep_scan_used: deep_scan,
        vars_scanned: files_scanned,
        dependents,
    })
}

// ----------------------------------------------------------------------------
// Delete Dependency Scan — for the .var being deleted, read its own meta.json
// dependency tree (recursively — VAM nests the transitive closure) and, for
// each dependency family, list its local files and count which OTHER packages
// also declare it. Powers the delete modal's "Scan dependencies" option:
// exclusive deps are safe to recycle with the target, shared ones get a
// "used by N others" warning (warn-but-allow, nothing is blocked).
// ----------------------------------------------------------------------------

/// Users beyond this per dependency are summarized by `used_by_total` only —
/// a dep shared by 50+ packages is "definitely shared" either way.
const USED_BY_CAP: usize = 50;

/// Reads the TOP-LEVEL meta.json dependency keys of one library .var and
/// returns the subset of `dep_bases_lc` it declares. Top-level only — the
/// nested tree is VAM's build-time snapshot and would mark orphaned deps as
/// "shared" (see `read_var_refs`); any real intermediate consumer declares
/// its own deps at top level. Returns `None` when the archive/meta is
/// unreadable, which the caller counts as a scan_error rather than failing
/// the task (`inspect_var_for_dependency` precedent).
fn inspect_var_for_dep_usage(
    file_path: &Path,
    dep_bases_lc: &HashSet<String>,
) -> Option<Vec<String>> {
    use crate::utils::{normalize_zip_path, read_json_bytes};

    let archive_file = fs::File::open(file_path).ok()?;
    let mut archive = ZipArchive::new(archive_file).ok()?;
    let meta_idx = (0..archive.len()).find(|i| {
        archive
            .by_index(*i)
            .ok()
            .map(|e| normalize_zip_path(e.name()).eq_ignore_ascii_case("meta.json"))
            .unwrap_or(false)
    })?;
    let mut raw = Vec::new();
    archive.by_index(meta_idx).ok()?.read_to_end(&mut raw).ok()?;
    let meta = read_json_bytes(&raw, "meta.json").ok()?;
    let mut matched: Vec<String> = Vec::new();
    if let Some(deps) = meta.get("dependencies").and_then(|v| v.as_object()) {
        let mut seen: HashSet<String> = HashSet::new();
        for key in deps.keys() {
            let base_lc = dependency_package_base(key).to_ascii_lowercase();
            if dep_bases_lc.contains(&base_lc) && seen.insert(base_lc.clone()) {
                matched.push(base_lc);
            }
        }
    }
    Some(matched)
}

fn finish_delete_dependency_scan_task(
    tasks: &Arc<Mutex<HashMap<u64, ProgressPayload>>>,
    task_id: u64,
    result: Result<DeleteDependencyScanResponse>,
) {
    if let Ok(mut guard) = tasks.lock() {
        if let Some(task) = guard.get_mut(&task_id) {
            task.done = true;
            task.progress = 1.0;
            match result {
                Ok(payload) => {
                    let total = payload.dependencies.len();
                    let exclusive = payload.dependencies.iter().filter(|d| d.exclusive).count();
                    let not_installed = payload
                        .dependencies
                        .iter()
                        .filter(|d| d.local_files.is_empty())
                        .count();
                    task.phase = if payload.was_cancelled {
                        "delete_dependency_scan_cancelled".to_string()
                    } else {
                        "delete_dependency_scan_complete".to_string()
                    };
                    task.message = format!(
                        "Found {} dependenc{} ({} exclusive, {} not installed) across {} VAR{}",
                        total,
                        if total == 1 { "y" } else { "ies" },
                        exclusive,
                        not_installed,
                        payload.vars_scanned,
                        if payload.vars_scanned == 1 { "" } else { "s" },
                    );
                    task.delete_dependency_scan_result = Some(payload);
                    task.error = None;
                }
                Err(err) => {
                    task.phase = "delete_dependency_scan_failed".to_string();
                    task.message = "Dependency scan failed".to_string();
                    task.error = Some(err.to_string());
                }
            }
        }
    }
}

pub(crate) fn run_delete_dependency_scan_task(
    tasks: &Arc<Mutex<HashMap<u64, ProgressPayload>>>,
    task_id: u64,
    request: DeleteDependencyScanRequest,
    cancel: Arc<AtomicBool>,
) -> Result<DeleteDependencyScanResponse> {
    let folder = request.folder.trim();
    if folder.is_empty() {
        return Err(anyhow::anyhow!("folder is required"));
    }
    let dir = Path::new(folder);
    if !dir.is_dir() {
        return Err(anyhow::anyhow!("not a directory: {}", dir.display()));
    }
    let target_raw = request.target_var_path.trim();
    if target_raw.is_empty() {
        return Err(anyhow::anyhow!("target_var_path is required"));
    }
    let target_path = Path::new(target_raw);
    let is_var = target_path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("var"))
        .unwrap_or(false);
    if !is_var {
        return Err(anyhow::anyhow!(
            "not a .var file: {}",
            target_path.display()
        ));
    }
    if !target_path.is_file() {
        return Err(anyhow::anyhow!(
            "target package not found: {}",
            target_path.display()
        ));
    }
    let target_pid = target_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    let target_base = dependency_package_base(&target_pid);
    let target_base_lc = target_base.to_ascii_lowercase();
    let target_canon = fs::canonicalize(target_path).unwrap_or_else(|_| target_path.to_path_buf());

    set_task_progress(
        tasks,
        task_id,
        "delete_dependency_scan_read_target",
        0.02,
        format!("Reading {} dependencies", target_pid),
    );
    // The target's archive must be readable — without its dep list there is
    // nothing to scan for.
    let (top_ids, all_ids) = read_var_dependency_sets(target_path)
        .map_err(|msg| anyhow::anyhow!("could not read target package: {msg}"))?;

    // Group the declared ids by version-stripped base. No cycle guard needed
    // for the nested walk: parsed JSON is a finite tree (serde_json's 128-depth
    // parse limit bounds recursion inside read_json_bytes).
    struct DepAccumulator {
        display_base: String,
        declared_ids: std::collections::BTreeSet<String>,
        direct: bool,
    }
    let top_bases_lc: HashSet<String> = top_ids
        .iter()
        .map(|id| dependency_package_base(id).to_ascii_lowercase())
        .collect();
    let mut acc: std::collections::BTreeMap<String, DepAccumulator> =
        std::collections::BTreeMap::new();
    for id in &all_ids {
        let base = dependency_package_base(id);
        if base.is_empty() {
            continue;
        }
        let base_lc = base.to_ascii_lowercase();
        let entry = acc
            .entry(base_lc.clone())
            .or_insert_with(|| DepAccumulator {
                display_base: base.clone(),
                declared_ids: std::collections::BTreeSet::new(),
                direct: false,
            });
        entry.declared_ids.insert(id.clone());
        entry.direct |= top_bases_lc.contains(&base_lc);
    }
    if acc.is_empty() {
        return Ok(DeleteDependencyScanResponse {
            target_package_id: target_pid,
            target_package_base: target_base,
            vars_scanned: 0,
            scan_errors: 0,
            usage_complete: true,
            was_cancelled: false,
            dependencies: Vec::new(),
        });
    }
    let dep_bases_lc: HashSet<String> = acc.keys().cloned().collect();

    set_task_progress(
        tasks,
        task_id,
        "delete_dependency_scan_walk",
        0.04,
        "Listing .var files",
    );
    let files = if cancel.load(Ordering::Relaxed) {
        Vec::new()
    } else {
        let roots =
            crate::scan::scan_roots(dir, &parse_additional_dirs(&request.additional_folders));
        collect_local_var_files_multi(&roots)?
    };
    let total_files = files.len();

    /// One library .var that matters to the aggregation: it belongs to a dep
    /// family, declares one, or couldn't be read.
    struct DepScanRecord {
        package_id: String,
        path: std::path::PathBuf,
        /// Dep bases (lowercase) this file's TOP-LEVEL meta declares.
        declared_dep_bases: Vec<String>,
        /// Some(base_lc) when the file's own stem belongs to a dep family —
        /// stem-only, so corrupt archives are still listed as deletable files.
        member_of: Option<String>,
        unreadable: bool,
    }

    // Parallel walk — same worker-pool shape as `run_dependency_check_task`:
    // ZIP central-directory reads are I/O bound, per-thread buffers avoid locks.
    let num_workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, 8)
        .min(total_files.max(1));
    let next_index = AtomicUsize::new(0);
    let done_count = AtomicUsize::new(0);

    let per_worker: Vec<Vec<DepScanRecord>> = if total_files == 0 {
        Vec::new()
    } else {
        std::thread::scope(|s| {
            let mut handles = Vec::with_capacity(num_workers);
            for _ in 0..num_workers {
                let files_ref: &Vec<std::path::PathBuf> = &files;
                let next_index_ref = &next_index;
                let done_count_ref = &done_count;
                let tasks_ref = tasks;
                let dep_bases_ref = &dep_bases_lc;
                let target_canon_ref: &Path = target_canon.as_path();
                let cancel_ref = &cancel;
                handles.push(s.spawn(move || {
                    let mut local: Vec<DepScanRecord> = Vec::new();
                    loop {
                        if cancel_ref.load(Ordering::Relaxed) {
                            break;
                        }
                        let i = next_index_ref.fetch_add(1, Ordering::Relaxed);
                        if i >= total_files {
                            break;
                        }
                        let file = &files_ref[i];
                        let pid = file
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("unknown")
                            .to_string();
                        // Skip only the exact file being deleted (canonical
                        // path) — never its family: a surviving sibling version
                        // or duplicate copy still needs its deps
                        // (compute_protection precedent).
                        let canonical =
                            fs::canonicalize(file).unwrap_or_else(|_| file.clone());
                        if canonical != target_canon_ref {
                            let stem_base_lc =
                                dependency_package_base(&pid).to_ascii_lowercase();
                            let member_of = dep_bases_ref
                                .contains(&stem_base_lc)
                                .then(|| stem_base_lc.clone());
                            let (declared_dep_bases, unreadable) =
                                match inspect_var_for_dep_usage(file, dep_bases_ref) {
                                    Some(matched) => (matched, false),
                                    None => (Vec::new(), true),
                                };
                            if member_of.is_some()
                                || !declared_dep_bases.is_empty()
                                || unreadable
                            {
                                local.push(DepScanRecord {
                                    package_id: pid.clone(),
                                    path: file.clone(),
                                    declared_dep_bases,
                                    member_of,
                                    unreadable,
                                });
                            }
                        }
                        let n = done_count_ref.fetch_add(1, Ordering::Relaxed) + 1;
                        let progress = 0.05 + (n as f64 / total_files as f64) * 0.90;
                        set_task_progress(
                            tasks_ref,
                            task_id,
                            "delete_dependency_scan_scan",
                            progress,
                            format!("Scanning {} ({}/{})", pid, n, total_files),
                        );
                    }
                    local
                }));
            }
            handles
                .into_iter()
                .map(|h| h.join().unwrap_or_default())
                .collect()
        })
    };
    let was_cancelled = cancel.load(Ordering::Relaxed);

    set_task_progress(
        tasks,
        task_id,
        "delete_dependency_scan_finalize",
        0.97,
        "Aggregating results",
    );

    let mut scan_errors = 0usize;
    let mut users_by_base: HashMap<String, Vec<DependencyUser>> = HashMap::new();
    let mut seen_users_by_base: HashMap<String, HashSet<String>> = HashMap::new();
    let mut files_by_base: HashMap<String, Vec<DependencyLocalFile>> = HashMap::new();
    for batch in per_worker {
        for record in batch {
            if record.unreadable {
                scan_errors += 1;
            }
            let user_base_lc = dependency_package_base(&record.package_id).to_ascii_lowercase();
            for base_lc in &record.declared_dep_bases {
                let seen = seen_users_by_base.entry(base_lc.clone()).or_default();
                if !seen.insert(record.package_id.to_ascii_lowercase()) {
                    continue;
                }
                users_by_base
                    .entry(base_lc.clone())
                    .or_default()
                    .push(DependencyUser {
                        package_id: record.package_id.clone(),
                        is_target_dependency: dep_bases_lc.contains(&user_base_lc),
                        is_target_family: user_base_lc == target_base_lc,
                    });
            }
            if let Some(base_lc) = &record.member_of {
                let metadata = fs::metadata(&record.path).ok();
                let size_bytes = metadata.as_ref().map(|m| m.len()).unwrap_or(0);
                let modified_ms = metadata
                    .as_ref()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as i64);
                files_by_base
                    .entry(base_lc.clone())
                    .or_default()
                    .push(DependencyLocalFile {
                        version: dependency_version_segment(&record.package_id),
                        package_id: record.package_id.clone(),
                        file_path: record.path.display().to_string(),
                        size_bytes,
                        modified_ms,
                        disabled: crate::packages::disabled_sidecar(&record.path).exists(),
                    });
            }
        }
    }

    let mut dependencies: Vec<DeleteDependencyItem> = Vec::with_capacity(acc.len());
    for (base_lc, accum) in acc {
        let mut local_files = files_by_base.remove(&base_lc).unwrap_or_default();
        // Newest version first; None (`.latest`/corrupt stems) last.
        local_files.sort_by(|a, b| {
            match (
                crate::naming::package_version(&a.package_id),
                crate::naming::package_version(&b.package_id),
            ) {
                (Some(x), Some(y)) => y.cmp(&x),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            }
            .then_with(|| a.file_path.cmp(&b.file_path))
        });
        let mut used_by = users_by_base.remove(&base_lc).unwrap_or_default();
        used_by.sort_by(|a, b| {
            a.package_id
                .to_ascii_lowercase()
                .cmp(&b.package_id.to_ascii_lowercase())
        });
        let used_by_total = used_by.len();
        used_by.truncate(USED_BY_CAP);
        dependencies.push(DeleteDependencyItem {
            creator: crate::naming::creator_from_package_id(&accum.display_base)
                .map(str::to_string),
            package_base: accum.display_base,
            declared_ids: accum.declared_ids.into_iter().collect(),
            direct: accum.direct,
            local_files,
            used_by,
            used_by_total,
            exclusive: used_by_total == 0,
        });
    }
    // The stated ordering requirement: exclusive first, then ascending by how
    // many other packages share the dep, then base for stability.
    dependencies.sort_by(|a, b| {
        b.exclusive
            .cmp(&a.exclusive)
            .then_with(|| a.used_by_total.cmp(&b.used_by_total))
            .then_with(|| {
                a.package_base
                    .to_ascii_lowercase()
                    .cmp(&b.package_base.to_ascii_lowercase())
            })
    });

    Ok(DeleteDependencyScanResponse {
        target_package_id: target_pid,
        target_package_base: target_base,
        vars_scanned: done_count.load(Ordering::Relaxed),
        scan_errors,
        usage_complete: scan_errors == 0 && !was_cancelled,
        was_cancelled,
        dependencies,
    })
}

// ----------------------------------------------------------------------------
// Download VARs page — forward dependency lookup
//
// Given dropped/selected .var files, read each one's declared dependencies from
// meta.json (transitively, since VAM nests a flattened dependency tree) and,
// when VAR library folders are supplied, mark each dependency present ("found")
// or "missing" by checking for a .var of the same package family on disk. The
// library check is a recursive directory walk + filename parse only — no
// archives are opened for the library, so it stays fast on large collections.
// ----------------------------------------------------------------------------

/// Returns the trailing version segment of a package id ("3", "latest") or None
/// when there is no version suffix. Companion to `dependency_package_base`.
fn dependency_version_segment(id: &str) -> Option<String> {
    crate::naming::package_version_segment(id.trim()).map(str::to_string)
}

/// Recursively collects every package id under nested `dependencies` maps. VAM
/// writes each dependency with its own `dependencies` object, so walking the
/// tree captures transitive deps as well as direct ones.
fn collect_meta_dependency_ids(
    deps: &serde_json::Map<String, serde_json::Value>,
    out: &mut std::collections::BTreeSet<String>,
) {
    for (key, val) in deps {
        let trimmed = key.trim();
        if !trimmed.is_empty() {
            out.insert(trimmed.to_string());
        }
        if let Some(nested) = val.get("dependencies").and_then(|v| v.as_object()) {
            collect_meta_dependency_ids(nested, out);
        }
    }
}

/// Opens a .var and returns `(top_level_ids, all_ids)` from its meta.json
/// `dependencies`: the direct declarations and the full recursive set (VAM
/// nests each dependency's own tree, so `all_ids` is the transitive closure
/// as recorded at build time). One archive open, one JSON parse. Errors are
/// returned as strings so a single bad file can be recorded without aborting
/// the whole batch.
#[allow(clippy::type_complexity)]
pub(crate) fn read_var_dependency_sets(
    path: &Path,
) -> Result<
    (
        std::collections::BTreeSet<String>,
        std::collections::BTreeSet<String>,
    ),
    String,
> {
    use crate::utils::{normalize_zip_path, read_json_bytes};

    let file = fs::File::open(path).map_err(|err| err.to_string())?;
    let mut archive = ZipArchive::new(file).map_err(|err| err.to_string())?;
    let meta_idx = (0..archive.len()).find(|i| {
        archive
            .by_index(*i)
            .ok()
            .map(|e| normalize_zip_path(e.name()).eq_ignore_ascii_case("meta.json"))
            .unwrap_or(false)
    });
    let idx = meta_idx.ok_or_else(|| "meta.json not found in archive".to_string())?;
    let mut raw = Vec::new();
    {
        let mut entry = archive.by_index(idx).map_err(|err| err.to_string())?;
        entry.read_to_end(&mut raw).map_err(|err| err.to_string())?;
    }
    let meta = read_json_bytes(&raw, "meta.json").map_err(|err| err.to_string())?;
    let mut top = std::collections::BTreeSet::new();
    let mut all = std::collections::BTreeSet::new();
    if let Some(deps) = meta.get("dependencies").and_then(|v| v.as_object()) {
        for key in deps.keys() {
            let trimmed = key.trim();
            if !trimmed.is_empty() {
                top.insert(trimmed.to_string());
            }
        }
        collect_meta_dependency_ids(deps, &mut all);
    }
    Ok((top, all))
}

/// Opens a .var and returns the set of package ids it declares as dependencies
/// (the full recursive/transitive set).
fn read_var_dependency_ids(path: &Path) -> Result<std::collections::BTreeSet<String>, String> {
    read_var_dependency_sets(path).map(|(_, all)| all)
}

/// Reads the dependencies of one or more dropped .var files and, when
/// `library_dirs` are supplied, marks each as present ("found") or "missing" by
/// checking the library folders for a .var of the same package family. With no
/// library, presence is "unknown". Nothing is written to disk or the database.
/// Runs as a background task so a large library walk reports progress instead of
/// freezing the UI; see `start_analyze_var_dependencies_task`.
fn run_analyze_var_dependencies_task(
    tasks: &Arc<Mutex<HashMap<u64, ProgressPayload>>>,
    task_id: u64,
    var_paths: Vec<String>,
    library_dirs: Vec<String>,
    db: Db,
) -> Result<AnalyzeVarDepsResponse> {
    use std::collections::{BTreeMap, BTreeSet};

    let total_inputs = var_paths.len();
    // 1. Read each dropped VAR's declared dependencies.
    let mut sources: Vec<DownloadDepSource> = Vec::new();
    // declared dependency id -> set of source VAR package ids that declared it.
    let mut dep_sources: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (idx, raw_path) in var_paths.iter().enumerate() {
        let path = Path::new(raw_path.trim());
        let pkg_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        // Reading dropped VARs spans 0.05..0.55 of the bar; the library walk
        // and list build take the remainder.
        let progress = if total_inputs == 0 {
            0.55
        } else {
            0.05 + (idx as f64 / total_inputs as f64) * 0.5
        };
        set_task_progress(
            tasks,
            task_id,
            "analyze_var_deps_read",
            progress,
            format!("Reading {} ({}/{})", pkg_id, idx + 1, total_inputs),
        );
        match read_var_dependency_ids(path) {
            Ok(ids) => {
                for id in &ids {
                    dep_sources.entry(id.clone()).or_default().insert(pkg_id.clone());
                }
                sources.push(DownloadDepSource {
                    file_path: path.display().to_string(),
                    package_id: pkg_id,
                    dependency_count: ids.len(),
                    error: None,
                });
            }
            Err(err) => sources.push(DownloadDepSource {
                file_path: path.display().to_string(),
                package_id: pkg_id,
                dependency_count: 0,
                error: Some(err),
            }),
        }
    }

    // 2 & 3. Index the library and build the dependency rows with presence
    //        status (shared with the pasted-text path).
    set_task_progress(
        tasks,
        task_id,
        "analyze_var_deps_library",
        0.6,
        "Indexing VAR library",
    );
    let (library_used, library_var_count, mut dependencies) =
        build_dependency_items(&dep_sources, &library_dirs);
    attach_hub_sources(&mut dependencies, &db, tasks, task_id);

    set_task_progress(
        tasks,
        task_id,
        "analyze_var_deps_finalize",
        0.99,
        "Finalizing",
    );
    Ok(AnalyzeVarDepsResponse {
        sources,
        library_used,
        library_var_count,
        dependencies,
    })
}

/// Indexes the library folders by package-family base and builds the dependency
/// rows with found/missing/unknown status. Shared by the VAR-analysis and
/// pasted-text paths. `deps` maps a declared package id → the source VAR ids that
/// referenced it (empty for the text path). Returns
/// `(library_used, library_var_count, sorted_rows)`.
pub(crate) fn build_dependency_items(
    deps: &std::collections::BTreeMap<String, std::collections::BTreeSet<String>>,
    library_dirs: &[String],
) -> (bool, usize, Vec<DownloadDepItem>) {
    // Directory walk + filename parse only — no archives opened here.
    let library_roots: Vec<PathBuf> = library_dirs
        .iter()
        .map(|d| PathBuf::from(d.trim()))
        .filter(|p| p.is_dir())
        .collect();
    let library_used = !library_roots.is_empty();
    let mut library_var_count = 0usize;
    // base (lowercased) -> every local file of that family, with its version
    // and the rank of the folder it is in (the first library dir holding it, so
    // AddonPackages, listed first, wins). Built from filenames only (no per-file
    // stat); found items are stat'd on demand below so we don't metadata-call
    // the whole library.
    let mut available: HashMap<String, Vec<(PathBuf, i64, usize)>> = HashMap::new();
    if library_used {
        if let Ok(files) = collect_local_var_files_multi(&library_roots) {
            library_var_count = files.len();
            for f in files {
                let stem = match f.file_stem().and_then(|s| s.to_str()) {
                    Some(s) => s.to_string(),
                    None => continue,
                };
                let base = dependency_package_base(&stem).to_ascii_lowercase();
                let version = dependency_version_segment(&stem)
                    .and_then(|v| v.parse::<i64>().ok())
                    .unwrap_or(0);
                let rank = library_roots
                    .iter()
                    .position(|root| crate::offload::path_is_under(&f, root))
                    .unwrap_or(library_roots.len());
                available.entry(base).or_default().push((f, version, rank));
            }
        }
    }

    let mut items: Vec<DownloadDepItem> = Vec::new();
    for (dep_id, src_set) in deps {
        let base = dependency_package_base(dep_id);
        // The copy that satisfies it: from the first folder that has the family,
        // the exact version asked for when it is there, else the newest.
        let local = if library_used {
            available.get(&base.to_ascii_lowercase()).and_then(|files| {
                let want = wanted_version(dep_id);
                files.iter().min_by_key(|(_, version, rank)| {
                    (*rank, want != Some(*version), std::cmp::Reverse(*version))
                })
            })
        } else {
            None
        };
        let status = if !library_used {
            "unknown"
        } else if local.is_some() {
            "found"
        } else {
            "missing"
        };
        // For found items, surface the local file's size + filename + path so the
        // row shows what's already on disk (and can load a scene preview).
        let (file_size, filename, source_host, local_path) = match local {
            Some((path, _, _)) => (
                std::fs::metadata(path).ok().map(|m| m.len()),
                path.file_name().and_then(|n| n.to_str()).map(str::to_string),
                Some("library".to_string()),
                Some(path.display().to_string()),
            ),
            None => (None, None, None, None),
        };
        items.push(DownloadDepItem {
            package_id: dep_id.clone(),
            package_base: base,
            creator: crate::naming::creator_from_package_id(dep_id).map(str::to_string),
            version: dependency_version_segment(dep_id),
            status: status.to_string(),
            source_vars: src_set.iter().cloned().collect(),
            download_url: None,
            file_size,
            filename,
            source_host,
            local_path,
        });
    }

    // Sort: missing first (the actionable ones), then found, then unknown;
    // package id alphabetical within each group for stable display.
    fn status_rank(status: &str) -> u8 {
        match status {
            "missing" => 0,
            "found" => 1,
            _ => 2,
        }
    }
    items.sort_by(|a, b| {
        status_rank(&a.status)
            .cmp(&status_rank(&b.status))
            .then_with(|| {
                a.package_id
                    .to_ascii_lowercase()
                    .cmp(&b.package_id.to_ascii_lowercase())
            })
    });

    (library_used, library_var_count, items)
}

/// Resolves Hub download sources (URL + size + concrete filename) for the
/// `missing` items and attaches them in place, so each row can show its source.
/// Network errors are swallowed — the offline found/missing result still stands;
/// affected rows simply won't show a source (the UI offers a retry on download).
fn attach_hub_sources(
    items: &mut [DownloadDepItem],
    db: &Db,
    tasks: &Arc<Mutex<HashMap<u64, ProgressPayload>>>,
    task_id: u64,
) {
    let missing_ids: Vec<String> = items
        .iter()
        .filter(|i| i.status == "missing")
        .map(|i| i.package_id.clone())
        .collect();
    if missing_ids.is_empty() {
        return;
    }
    set_task_progress(
        tasks,
        task_id,
        "analyze_sources",
        0.97,
        "Finding download sources",
    );
    // 1. VaM Hub.
    let hub_resolved = match crate::hub::hub_client() {
        Ok(client) => crate::hub::resolve_download_urls(&client, &missing_ids).0,
        Err(_) => std::collections::HashMap::new(),
    };
    for item in items.iter_mut() {
        if item.status != "missing" {
            continue;
        }
        if let Some(r) = hub_resolved.get(&item.package_id) {
            item.download_url = Some(r.download_url.clone());
            item.file_size = r.file_size;
            item.filename = Some(r.filename.clone());
            item.source_host = Some("hub".to_string());
            continue;
        }
        // 2. Imported mirror links (DB).
        let base = dependency_package_base(&item.package_id).to_ascii_lowercase();
        if let Some(link) = pick_db_link(db, &base, wanted_version(&item.package_id)) {
            item.download_url = Some(link.url);
            item.filename = Some(link.filename);
            item.source_host = Some(link.host);
            // Size is known only after a prior download recorded it; until then
            // mirror rows show no size.
            item.file_size = link.size.map(|s| s as u64);
        }
    }
}

/// The numeric version a package id asks for (`Pkg.3` -> 3; `.latest` -> none).
fn wanted_version(package_id: &str) -> Option<i64> {
    dependency_version_segment(package_id).and_then(|v| v.parse().ok())
}

/// Picks the best stored mirror link for a package family: among links for
/// the exact version asked for when there are any (a source added for
/// `Pkg.3` beats an imported `Pkg.5`), else among all; within those the
/// auto-downloadable Pixeldrain mirror first, else the newest. Shared by the
/// dependency analysis and the single-VAR resolver so they can't drift.
pub(crate) fn pick_db_link(db: &Db, base_lc: &str, want: Option<i64>) -> Option<DownloadLinkRow> {
    let links = crate::db::find_download_links(db, base_lc).ok()?;
    let exact: Vec<&DownloadLinkRow> = links.iter().filter(|l| want.is_some() && l.version == want).collect();
    let mut pool: Vec<&DownloadLinkRow> = if exact.is_empty() { links.iter().collect() } else { exact };
    // Healthiest first (a link that worked beats an untried one, a failing
    // one comes last), then the newest version, then Pixeldrain.
    pool.sort_by_key(|l| (l.health_rank(), std::cmp::Reverse(l.version), l.host != "pixeldrain"));
    pool.first().map(|l| (*l).clone())
}

/// Resolves the download source for ONE package id: VaM Hub first, then the
/// imported mirror-link DB. `error` is set only when the Hub couldn't be reached
/// (so the caller can distinguish "not available" from "couldn't check").
fn resolve_one_source(db: &Db, package_id: &str) -> VarSourceInfo {
    let mut info = VarSourceInfo {
        download_url: None,
        filename: None,
        file_size: None,
        host: None,
        error: None,
    };
    // 1. VaM Hub.
    match crate::hub::hub_client() {
        Ok(client) => {
            let ids = [package_id.to_string()];
            let (resolved, err) = crate::hub::resolve_download_urls(&client, &ids);
            if let Some(r) = resolved.get(package_id) {
                info.download_url = Some(r.download_url.clone());
                info.filename = Some(r.filename.clone());
                info.file_size = r.file_size;
                info.host = Some("hub".to_string());
                return info;
            }
            // Hub reachable but no hit → err is None; unreachable → Some(reason).
            info.error = err;
        }
        Err(e) => info.error = Some(e.to_string()),
    }
    // 2. Imported mirror links (DB).
    let base = dependency_package_base(package_id).to_ascii_lowercase();
    if let Some(link) = pick_db_link(db, &base, wanted_version(package_id)) {
        info.download_url = Some(link.url);
        info.filename = Some(link.filename);
        info.host = Some(link.host);
        info.file_size = link.size.map(|s| s as u64);
        info.error = None; // a source was found
    }
    info
}

fn finish_resolve_var_source_task(
    tasks: &Arc<Mutex<HashMap<u64, ProgressPayload>>>,
    task_id: u64,
    result: VarSourceInfo,
) {
    if let Ok(mut guard) = tasks.lock() {
        if let Some(task) = guard.get_mut(&task_id) {
            task.done = true;
            task.progress = 1.0;
            task.phase = "resolve_var_source_complete".to_string();
            task.message = match (&result.host, &result.error) {
                (Some(h), _) => format!("Source: {h}"),
                (None, Some(_)) => "Could not reach the Hub".to_string(),
                (None, None) => "No source found".to_string(),
            };
            task.error = None;
            task.var_source_result = Some(result);
        }
    }
}

/// Resolves where a single VAR can be downloaded from (Hub / imported mirror).
/// Used by the VAR Details "download this VAR" banner when the file isn't local.
#[tauri::command]
pub(crate) fn start_resolve_var_source_task(
    package_id: String,
    state: State<'_, AppState>,
    db: State<'_, Db>,
) -> Result<TaskHandle, String> {
    let task_id = state.next_task_id.fetch_add(1, Ordering::SeqCst) + 1;
    {
        let mut guard = state
            .tasks
            .lock()
            .map_err(|_| "task state poisoned".to_string())?;
        guard.insert(
            task_id,
            new_progress_payload("resolve_var_source_starting", "Checking the Hub"),
        );
    }
    let tasks = Arc::clone(&state.tasks);
    let db = db.inner().clone();
    thread::spawn(move || {
        let result = resolve_one_source(&db, &package_id);
        finish_resolve_var_source_task(&tasks, task_id, result);
    });
    Ok(TaskHandle { id: task_id })
}

fn finish_analyze_var_dependencies_task(
    tasks: &Arc<Mutex<HashMap<u64, ProgressPayload>>>,
    task_id: u64,
    result: Result<AnalyzeVarDepsResponse>,
) {
    if let Ok(mut guard) = tasks.lock() {
        if let Some(task) = guard.get_mut(&task_id) {
            task.done = true;
            task.progress = 1.0;
            match result {
                Ok(payload) => {
                    let missing = payload
                        .dependencies
                        .iter()
                        .filter(|d| d.status == "missing")
                        .count();
                    task.phase = "analyze_var_deps_complete".to_string();
                    task.message = format!(
                        "{} dependenc{} ({} missing)",
                        payload.dependencies.len(),
                        if payload.dependencies.len() == 1 { "y" } else { "ies" },
                        missing,
                    );
                    task.analyze_var_deps_result = Some(payload);
                    task.error = None;
                }
                Err(err) => {
                    task.phase = "analyze_var_deps_failed".to_string();
                    task.message = "Dependency analysis failed".to_string();
                    task.error = Some(err.to_string());
                }
            }
        }
    }
}

#[tauri::command]
pub(crate) fn start_analyze_var_dependencies_task(
    var_paths: Vec<String>,
    library_dirs: Vec<String>,
    state: State<'_, AppState>,
    db: State<'_, Db>,
) -> Result<TaskHandle, String> {
    let task_id = state.next_task_id.fetch_add(1, Ordering::SeqCst) + 1;
    {
        let mut guard = state
            .tasks
            .lock()
            .map_err(|_| "task state poisoned".to_string())?;
        guard.insert(
            task_id,
            new_progress_payload("analyze_var_deps_starting", "Starting dependency analysis"),
        );
    }

    let tasks = Arc::clone(&state.tasks);
    let db = db.inner().clone();
    thread::spawn(move || {
        let result =
            run_analyze_var_dependencies_task(&tasks, task_id, var_paths, library_dirs, db);
        finish_analyze_var_dependencies_task(&tasks, task_id, result);
    });

    Ok(TaskHandle { id: task_id })
}

/// A fresh ProgressPayload with all result slots empty — the state every task
/// starts in. `..Default::default()` keeps new result variants from touching
/// this function at all.
pub(crate) fn new_progress_payload(phase: &str, message: &str) -> ProgressPayload {
    ProgressPayload {
        phase: phase.to_string(),
        message: message.to_string(),
        ..Default::default()
    }
}

// ----------------------------------------------------------------------------
// Download VARs — pasted-text dependency extraction
//
// Port of Sharp VaM Tools' GlobalExtractVarNames.ExtractVarNamesFromText: pull
// VaM package ids out of arbitrary text (scene JSON, meta.json, forum posts).
// `.latest`/`.minN` collapse to the version-stripped family base; explicit
// `Creator.Package.N` ids are kept as-is.
// ----------------------------------------------------------------------------

/// Splits `text` on any of the given delimiter substrings, dropping empties.
/// (The reference uses one multi-delimiter String.Split; applying them in turn
/// yields the same final token set.)
fn split_on_any(text: &str, delimiters: &[&str]) -> Vec<String> {
    let mut parts = vec![text.to_string()];
    for d in delimiters {
        let mut next = Vec::new();
        for p in parts {
            for piece in p.split(d) {
                if !piece.is_empty() {
                    next.push(piece.to_string());
                }
            }
        }
        parts = next;
    }
    parts
}

fn extract_package_ids_from_text(text: &str) -> Vec<String> {
    use regex::Regex;
    use std::collections::BTreeSet;

    // Built per call — analysis tasks are infrequent, so this avoids a
    // once_cell dependency.
    let normalize = Regex::new(r"(?i)(\.min[0-9]+|\.latest)").unwrap();
    // Reject pure version-number tokens like "1.2.3" / "1.0_3.0" / "1.x.3".
    let bad = Regex::new(
        r"([0-9]+\.+[0-9]+\.[0-9]+\.[0-9]+|[0-9]+\.[0-9]+\.[0-9]+|[0-9]+\.[0-9]+_+[0-9]+\.[0-9]+|^[0-9]\..*\.[0-9]+)",
    )
    .unwrap();
    let versioned = Regex::new(r"[^\s]+\..*?\.[0-9]+").unwrap();
    let latest = Regex::new(r"[^\s]+\..*?\.latest").unwrap();

    // Normalize .minN / .latest to ".latest\n", then split on the same
    // delimiters the reference uses (extensions, URL/markup punctuation, ws).
    let normalized = normalize.replace_all(text, ".latest\n");
    let delimiters = [
        ".var", ".txt", ".jpg", ".jpeg", ".gif", ".png", ".zip", ".rar", ".7z", "\r\n", "\n", "\r",
        "http", " - ", ":", "<", ">", "/", ";", "=", "\\", "\"",
    ];
    let tokens = split_on_any(&normalized, &delimiters);

    let mut out: BTreeSet<String> = BTreeSet::new();
    for token in tokens {
        let token = token.trim();
        if token.is_empty() || bad.is_match(token) {
            continue;
        }
        let mut m = String::new();
        if let Some(hit) = versioned.find(token) {
            m = hit.as_str().replace("&amp;", "&");
        }
        // `.latest` wins over the versioned match (matches the reference order).
        if let Some(hit) = latest.find(token) {
            m = hit.as_str().replace('"', "").replace(".latest", "");
        }
        // Strip a leading '-' (forum / F95 formatting artifact).
        let m = m.trim_start_matches('-').trim().to_string();
        if !m.is_empty() && m.contains('.') {
            out.insert(m);
        }
    }
    out.into_iter().collect()
}

// ----------------------------------------------------------------------------
// Download VARs — pasted-text analysis task (mirrors the VAR-analysis task but
// sources the package ids from text instead of dropped VAR meta.json files).
// Reuses the same AnalyzeVarDepsResponse payload + finish handler.
// ----------------------------------------------------------------------------

fn run_analyze_text_dependencies_task(
    tasks: &Arc<Mutex<HashMap<u64, ProgressPayload>>>,
    task_id: u64,
    text: String,
    library_dirs: Vec<String>,
    db: Db,
) -> Result<AnalyzeVarDepsResponse> {
    use std::collections::{BTreeMap, BTreeSet};

    set_task_progress(
        tasks,
        task_id,
        "analyze_text_parse",
        0.1,
        "Extracting package ids from text",
    );
    let mut dep_sources: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for id in extract_package_ids_from_text(&text) {
        dep_sources.entry(id).or_default();
    }

    set_task_progress(
        tasks,
        task_id,
        "analyze_text_library",
        0.6,
        "Indexing VAR library",
    );
    let (library_used, library_var_count, mut dependencies) =
        build_dependency_items(&dep_sources, &library_dirs);
    attach_hub_sources(&mut dependencies, &db, tasks, task_id);

    set_task_progress(tasks, task_id, "analyze_text_finalize", 0.99, "Finalizing");
    Ok(AnalyzeVarDepsResponse {
        sources: Vec::new(),
        library_used,
        library_var_count,
        dependencies,
    })
}

#[tauri::command]
pub(crate) fn start_analyze_text_dependencies_task(
    text: String,
    library_dirs: Vec<String>,
    state: State<'_, AppState>,
    db: State<'_, Db>,
) -> Result<TaskHandle, String> {
    let task_id = state.next_task_id.fetch_add(1, Ordering::SeqCst) + 1;
    {
        let mut guard = state
            .tasks
            .lock()
            .map_err(|_| "task state poisoned".to_string())?;
        guard.insert(
            task_id,
            new_progress_payload("analyze_text_starting", "Starting text analysis"),
        );
    }
    let tasks = Arc::clone(&state.tasks);
    let db = db.inner().clone();
    thread::spawn(move || {
        let result = run_analyze_text_dependencies_task(&tasks, task_id, text, library_dirs, db);
        // Same payload shape as the VAR-analysis task → reuse its finisher.
        finish_analyze_var_dependencies_task(&tasks, task_id, result);
    });

    Ok(TaskHandle { id: task_id })
}

// ----------------------------------------------------------------------------
// Download VARs — per-package download task (VaM Hub only)
//
// Streams one already-resolved download URL to the destination folder with
// progress, polling a cancel flag between chunks, and validating the file is a
// real .var before moving it into place. Each row drives its own task, so
// downloads run independently and can be cancelled individually.
// ----------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)] // worker entry point: the task's shared state plus its download
pub(crate) fn run_download_one_task(
    tasks: &Arc<Mutex<HashMap<u64, ProgressPayload>>>,
    task_id: u64,
    package_id: String,
    download_url: String,
    filename: String,
    dest_dir: String,
    cancel: Arc<AtomicBool>,
    pause: Arc<AtomicBool>,
    replace: bool,
    db: Db,
) -> Result<DownloadVarsResponse> {
    let dest = PathBuf::from(dest_dir.trim());
    if dest.as_os_str().is_empty() {
        return Err(anyhow::anyhow!("destination folder is required"));
    }
    if download_url.trim().is_empty() {
        return Err(anyhow::anyhow!("no download URL for {package_id}"));
    }
    std::fs::create_dir_all(&dest)
        .map_err(|e| anyhow::anyhow!("cannot create destination {}: {e}", dest.display()))?;

    let safe_name = if filename.trim().is_empty() {
        format!("{package_id}.var")
    } else {
        filename.clone()
    };
    let final_path = dest.join(&safe_name);

    // Builds a single-item response in the shared DownloadVarsResponse shape.
    let mk = |status: &str, error: Option<String>, retryable: bool| DownloadVarsResponse {
        items: vec![DownloadVarItem {
            package_id: package_id.clone(),
            status: status.to_string(),
            filename: Some(safe_name.clone()),
            error,
            retryable,
        }],
        downloaded: usize::from(status == "downloaded" || status == "exists"),
        failed: usize::from(status == "failed"),
        no_source: 0,
        dest_dir: dest.display().to_string(),
    };

    // `replace` (Redownload of a damaged copy) downloads anyway and swaps the
    // new file in over the old one.
    if final_path.exists() && !replace {
        if let Ok(meta) = std::fs::metadata(&final_path) {
            let _ = crate::db::update_download_link_size(&db, &safe_name, meta.len());
        }
        return Ok(mk("exists", None, false));
    }

    let tmp_path = dest.join(format!(".part-{safe_name}"));
    // Which link the partial came from: it is resumed only from that link.
    let src_path = dest.join(format!(".part-{safe_name}.src"));
    let discard_partial = || {
        let _ = std::fs::remove_file(&tmp_path);
        let _ = std::fs::remove_file(&src_path);
    };
    // Browser-style progress: bytes downloaded / total · current speed. Speed is
    // measured over the interval between emits; updates are throttled to ~150ms
    // to limit lock churn (the read loop fires per 64 KB chunk).
    let started = std::time::Instant::now();
    let mut last_emit = started;
    let mut last_bytes: u64 = 0;
    let mut report = |bytes: u64, content_total: Option<u64>| {
        let done = content_total.map(|t| bytes >= t).unwrap_or(false);
        let now = std::time::Instant::now();
        let since = now.duration_since(last_emit).as_secs_f64();
        if since < 0.15 && !done {
            return;
        }
        let speed = if since > 0.0 {
            bytes.saturating_sub(last_bytes) as f64 / since
        } else {
            0.0
        };
        last_emit = now;
        last_bytes = bytes;
        let frac = match content_total {
            Some(t) if t > 0 => (bytes as f64 / t as f64).min(1.0),
            _ => 0.0,
        };
        let msg = match content_total {
            Some(t) if t > 0 => format!(
                "{} / {} · {}/s",
                crate::utils::format_bytes(bytes),
                crate::utils::format_bytes(t),
                crate::utils::format_bytes(speed as u64)
            ),
            _ => format!(
                "{} · {}/s",
                crate::utils::format_bytes(bytes),
                crate::utils::format_bytes(speed as u64)
            ),
        };
        set_task_progress(tasks, task_id, "download_item", frac.clamp(0.0, 1.0), msg);
    };

    // The link asked for first, then this file's other saved links (best
    // first), so one dead mirror — say a new link that never worked — doesn't
    // fail the download while an older one still does.
    let mut links: Vec<String> = vec![download_url.clone()];
    for link in crate::db::links_for_file(&db, &safe_name) {
        if !links.contains(&link.url) {
            links.push(link.url);
        }
    }
    let mut errors: Vec<String> = Vec::new();
    for (attempt, url) in links.iter().enumerate() {
        if attempt > 0 {
            set_task_progress(
                tasks,
                task_id,
                "download_item",
                0.0,
                format!("That link failed — trying another ({} of {})", attempt + 1, links.len()),
            );
        }
        let archive = crate::db::find_link_archive(&db, &safe_name, url);
        if archive.is_none() {
            let from = std::fs::read_to_string(&src_path).ok();
            if from.as_deref() != Some(url.as_str()) {
                let _ = std::fs::remove_file(&tmp_path);
            }
            let _ = std::fs::write(&src_path, url);
        }
        // A source inside a .zip: fetch the archive (cached for the session,
        // so several packages from it download it once) and extract the one
        // member.
        let fetched = match archive {
            Some((entry, password)) => match crate::archives::ensure_cached(url, &cancel, &mut report) {
                Ok(zip) => {
                    set_task_progress(tasks, task_id, "download_item", 1.0, format!("Extracting {safe_name}"));
                    crate::archives::extract_entry(&zip, &entry, password.as_deref(), &tmp_path)
                        .map(|()| true)
                        .map_err(|e| anyhow::anyhow!(e))
                }
                Err(e) if e == "cancelled" => Ok(false),
                Err(e) => Err(anyhow::anyhow!(e)),
            },
            None => crate::hub::download_to_file(url, &tmp_path, &cancel, &mut report),
        };
        // Every entry's checksum, not just "it opens": a cut-short or garbled
        // transfer is caught here instead of in VaM.
        let fetched = match fetched {
            Ok(true) if crate::hub::is_valid_var(&tmp_path) => match crate::integrity::verify_var(&tmp_path) {
                Ok(()) => Ok(true),
                Err(e) => {
                    discard_partial();
                    Err(anyhow::anyhow!("downloaded file is damaged ({e})"))
                }
            },
            other => other,
        };
        let error = match fetched {
            Ok(true) if crate::hub::is_valid_var(&tmp_path) => {
                let _ = std::fs::remove_file(&src_path);
                return match std::fs::rename(&tmp_path, &final_path) {
                    Ok(()) => {
                        crate::db::record_link_result(&db, &safe_name, url, None);
                        // Record the real size so future analyses know it —
                        // especially for Pixeldrain, which has no size up front.
                        if let Ok(meta) = std::fs::metadata(&final_path) {
                            let _ = crate::db::update_download_link_size(&db, &safe_name, meta.len());
                        }
                        Ok(mk("downloaded", None, false))
                    }
                    Err(e) => {
                        discard_partial();
                        Ok(mk("failed", Some(format!("move failed: {e}")), false))
                    }
                };
            }
            Ok(true) => "downloaded file is not a valid .var".to_string(),
            // Paused: keep the partial for the resume.
            Ok(false) if pause.load(Ordering::SeqCst) => return Ok(mk("paused", None, false)),
            Ok(false) => {
                // Cancelled mid-stream — discard the partial file.
                discard_partial();
                return Ok(mk("cancelled", None, false));
            }
            Err(e) => e.to_string(),
        };
        crate::db::record_link_result(&db, &safe_name, url, Some(&error));
        // A network hiccup part-way through: keep what arrived and let the
        // queue retry this link later, instead of starting over on another.
        let partial = std::fs::metadata(&tmp_path).map(|m| m.len()).unwrap_or(0);
        if is_transient_download_error(&error) && partial > 0 {
            return Ok(mk("failed", Some(error), true));
        }
        discard_partial();
        errors.push(error);
    }
    let retryable = errors.last().is_some_and(|e| is_transient_download_error(e));
    let message = match errors.as_slice() {
        [one] => one.clone(),
        many => format!("all {} links failed — {}", many.len(), many.last().cloned().unwrap_or_default()),
    };
    Ok(mk("failed", Some(message), retryable))
}

/// Network trouble worth retrying later: timeouts, dropped connections,
/// stalls, server errors and rate limits. A 404 or a bad file is not.
pub(crate) fn is_transient_download_error(error: &str) -> bool {
    let lc = error.to_ascii_lowercase();
    [
        "request failed",
        "read error",
        "stalled",
        "timed out",
        "timeout",
        "connection",
        "dns",
        "http 5",
        "http 429",
        "http 408",
        "damaged",
    ]
    .iter()
    .any(|p| lc.contains(p))
}

fn finish_download_vars_task(
    tasks: &Arc<Mutex<HashMap<u64, ProgressPayload>>>,
    task_id: u64,
    result: Result<DownloadVarsResponse>,
) {
    if let Ok(mut guard) = tasks.lock() {
        if let Some(task) = guard.get_mut(&task_id) {
            task.done = true;
            task.progress = 1.0;
            match result {
                Ok(payload) => {
                    task.phase = "download_vars_complete".to_string();
                    task.message = format!(
                        "Downloaded {}, failed {}, no source {}",
                        payload.downloaded, payload.failed, payload.no_source
                    );
                    task.download_vars_result = Some(payload);
                    task.error = None;
                }
                Err(err) => {
                    task.phase = "download_vars_failed".to_string();
                    task.message = "Download failed".to_string();
                    task.error = Some(err.to_string());
                }
            }
        }
    }
}

#[tauri::command]
pub(crate) fn start_download_one_task(
    package_id: String,
    download_url: String,
    filename: String,
    dest_dir: String,
    replace: Option<bool>,
    state: State<'_, AppState>,
    db: State<'_, Db>,
) -> Result<TaskHandle, String> {
    let task_id = state.next_task_id.fetch_add(1, Ordering::SeqCst) + 1;
    {
        let mut guard = state
            .tasks
            .lock()
            .map_err(|_| "task state poisoned".to_string())?;
        guard.insert(
            task_id,
            new_progress_payload("download_starting", "Starting download"),
        );
    }
    // Register a cancel flag so the per-row Cancel button (cancel_task) can stop
    // this download mid-stream.
    let cancel_flag = Arc::new(AtomicBool::new(false));
    {
        let mut cancels = state
            .cancellations
            .lock()
            .map_err(|_| "cancellation state poisoned".to_string())?;
        cancels.insert(task_id, Arc::clone(&cancel_flag));
    }
    let pause_flag = Arc::new(AtomicBool::new(false));
    if let Ok(mut pauses) = state.pauses.lock() {
        pauses.insert(task_id, Arc::clone(&pause_flag));
    }
    let tasks = Arc::clone(&state.tasks);
    let db = db.inner().clone();
    thread::spawn(move || {
        let result = run_download_one_task(
            &tasks,
            task_id,
            package_id,
            download_url,
            filename,
            dest_dir,
            cancel_flag,
            pause_flag,
            replace.unwrap_or(false),
            db,
        );
        finish_download_vars_task(&tasks, task_id, result);
    });

    Ok(TaskHandle { id: task_id })
}

// ----------------------------------------------------------------------------
// Download Links DB — import community Pixeldrain/MediaFire mirror lists into
// the database so the Download VARs page can resolve Hub-absent packages.
// ----------------------------------------------------------------------------

/// Parses one `<filename>.var   <url>` line from a download-links file. Mirrors
/// the reference's `.*https:` split. Returns None for blank/garbage lines.
pub(crate) fn parse_link_line(line: &str) -> Option<DownloadLinkRow> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    // The URL is everything from the first http(s); the filename precedes it.
    let url_start = line.find("http")?;
    // Normalized like a hand-added source: a Pixeldrain share page becomes the
    // file's direct download URL.
    let (url, host) = crate::sources::normalize_link(&line[url_start..])?;
    let name = line[..url_start]
        .trim()
        .trim_matches(|c| c == '"' || c == '\'')
        .trim();
    if url.is_empty() || !name.to_ascii_lowercase().ends_with(".var") {
        return None;
    }
    let stem = &name[..name.len() - 4]; // strip ".var"
    let base = dependency_package_base(stem).to_ascii_lowercase();
    if base.is_empty() {
        return None;
    }
    let version = dependency_version_segment(stem).and_then(|v| v.parse::<i64>().ok());
    Some(DownloadLinkRow {
        package_base: base,
        version,
        filename: name.to_string(),
        host: host.to_string(),
        url,
        size: None,
        archive_entry: None,
        archive_password: None,
        last_ok: None,
        fail_count: 0,
        last_error: None,
    })
}

/// Imports download links from a folder of `<creator>.txt` files and/or a single
/// text file into the `download_links` table. Idempotent (dedupes on filename+url).
/// Runs as a background task so scanning hundreds of files + the DB write don't
/// block the UI; progress is reported per file.
fn run_import_download_links_task(
    tasks: &Arc<Mutex<HashMap<u64, ProgressPayload>>>,
    task_id: u64,
    source_folder: Option<String>,
    source_file: Option<String>,
    db: Db,
) -> Result<ImportLinksResult> {
    set_task_progress(tasks, task_id, "import_links_collect", 0.01, "Listing link files");
    let mut files: Vec<PathBuf> = Vec::new();
    if let Some(folder) = source_folder.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        let dir = Path::new(folder);
        if !dir.is_dir() {
            return Err(anyhow::anyhow!("not a folder: {folder}"));
        }
        let entries = fs::read_dir(dir).map_err(|e| anyhow::anyhow!(e.to_string()))?;
        for entry in entries.flatten() {
            let p = entry.path();
            let is_txt = p
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("txt"))
                .unwrap_or(false);
            if is_txt {
                files.push(p);
            }
        }
    }
    if let Some(file) = source_file.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        let p = Path::new(file);
        if !p.is_file() {
            return Err(anyhow::anyhow!("not a file: {file}"));
        }
        files.push(p.to_path_buf());
    }
    if files.is_empty() {
        return Err(anyhow::anyhow!("provide a download-links folder or a text file"));
    }

    let total = files.len().max(1);
    let mut rows: Vec<DownloadLinkRow> = Vec::new();
    let mut files_scanned = 0usize;
    for (idx, file) in files.iter().enumerate() {
        // Lossy decode tolerates the odd non-UTF-8 link file; URLs + ids are ASCII.
        let bytes = match fs::read(file) {
            Ok(b) => b,
            Err(_) => continue,
        };
        files_scanned += 1;
        for line in String::from_utf8_lossy(&bytes).lines() {
            if let Some(row) = parse_link_line(line) {
                rows.push(row);
            }
        }
        // Scanning spans 0.05..0.85 of the bar.
        let progress = 0.05 + (idx as f64 / total as f64) * 0.8;
        let name = file
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_string();
        set_task_progress(
            tasks,
            task_id,
            "import_links_scan",
            progress,
            format!("Scanning {} ({}/{}) — {} links so far", name, idx + 1, total, rows.len()),
        );
    }

    set_task_progress(
        tasks,
        task_id,
        "import_links_save",
        0.9,
        format!("Saving {} links to the database", rows.len()),
    );
    let links_added = db::insert_download_links(&db, &rows)?;
    let total_in_db = db::count_download_links(&db)?;
    Ok(ImportLinksResult {
        files_scanned,
        links_added,
        total_in_db,
    })
}

fn finish_import_download_links_task(
    tasks: &Arc<Mutex<HashMap<u64, ProgressPayload>>>,
    task_id: u64,
    result: Result<ImportLinksResult>,
) {
    if let Ok(mut guard) = tasks.lock() {
        if let Some(task) = guard.get_mut(&task_id) {
            task.done = true;
            task.progress = 1.0;
            match result {
                Ok(payload) => {
                    task.phase = "import_links_complete".to_string();
                    task.message = format!(
                        "Imported {} new link(s) from {} file(s)",
                        payload.links_added, payload.files_scanned
                    );
                    task.import_links_result = Some(payload);
                    task.error = None;
                }
                Err(err) => {
                    task.phase = "import_links_failed".to_string();
                    task.message = "Link import failed".to_string();
                    task.error = Some(err.to_string());
                }
            }
        }
    }
}

#[tauri::command]
pub(crate) fn start_import_download_links_task(
    source_folder: Option<String>,
    source_file: Option<String>,
    state: State<'_, AppState>,
    db: State<'_, Db>,
) -> Result<TaskHandle, String> {
    let task_id = state.next_task_id.fetch_add(1, Ordering::SeqCst) + 1;
    {
        let mut guard = state
            .tasks
            .lock()
            .map_err(|_| "task state poisoned".to_string())?;
        guard.insert(
            task_id,
            new_progress_payload("import_links_starting", "Starting link import"),
        );
    }
    let tasks = Arc::clone(&state.tasks);
    let db = db.inner().clone();
    thread::spawn(move || {
        let result = run_import_download_links_task(&tasks, task_id, source_folder, source_file, db);
        finish_import_download_links_task(&tasks, task_id, result);
    });

    Ok(TaskHandle { id: task_id })
}

#[tauri::command]
pub(crate) fn get_download_links_count(db: State<'_, Db>) -> Result<i64, String> {
    db::count_download_links(&db).map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) fn clear_download_links(db: State<'_, Db>) -> Result<(), String> {
    db::clear_download_links(&db).map_err(|e| e.to_string())
}

/// Opens an http(s) URL in the user's default browser (used for MediaFire
/// links, which can't be auto-downloaded without a real browser session).
#[tauri::command]
pub(crate) fn open_url(url: String) -> Result<(), String> {
    let url = url.trim();
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("refusing to open a non-http(s) URL".to_string());
    }
    #[cfg(target_os = "windows")]
    {
        // `cmd /C start "" <url>` launches the default browser; the empty "" is
        // the title argument `start` would otherwise consume the URL as.
        Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(not(target_os = "windows"))]
    {
        Command::new("xdg-open")
            .arg(url)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod download_vars_text_tests {
    use super::extract_package_ids_from_text;

    #[test]
    fn extracts_versioned_and_latest() {
        let text = "needs AcidBubbles.Timeline.291.var and MeshedVR.PluginsPack.latest here";
        let ids = extract_package_ids_from_text(text);
        assert!(ids.contains(&"AcidBubbles.Timeline.291".to_string()));
        // `.latest` collapses to the version-stripped family base.
        assert!(ids.contains(&"MeshedVR.PluginsPack".to_string()));
    }

    #[test]
    fn rejects_plain_version_numbers() {
        let ids = extract_package_ids_from_text("version 1.2.3 and 1.0_3.0 build");
        assert!(ids.is_empty(), "plain version numbers must not be treated as packages: {ids:?}");
    }

    #[test]
    fn parses_json_dependency_lines() {
        let text = "\"dependencies\" : { \"AcidBubbles.Embody.61\" : { }, \"Foo.Bar.latest\" : {} }";
        let ids = extract_package_ids_from_text(text);
        assert!(ids.contains(&"AcidBubbles.Embody.61".to_string()), "{ids:?}");
        assert!(ids.contains(&"Foo.Bar".to_string()), "{ids:?}");
    }
}

#[tauri::command]
pub(crate) fn start_dependency_check_task(
    request: DependencyCheckRequest,
    state: State<'_, AppState>,
) -> Result<TaskHandle, String> {
    let task_id = state.next_task_id.fetch_add(1, Ordering::SeqCst) + 1;
    {
        let mut guard = state
            .tasks
            .lock()
            .map_err(|_| "task state poisoned".to_string())?;
        guard.insert(
            task_id,
            new_progress_payload("dependency_check_starting", "Starting dependency check"),
        );
    }

    let tasks = Arc::clone(&state.tasks);
    thread::spawn(move || {
        let result = run_dependency_check_task(&tasks, task_id, request);
        finish_dependency_check_task(&tasks, task_id, result);
    });

    Ok(TaskHandle { id: task_id })
}

#[tauri::command]
pub(crate) fn start_delete_dependency_scan_task(
    request: DeleteDependencyScanRequest,
    state: State<'_, AppState>,
) -> Result<TaskHandle, String> {
    let task_id = state.next_task_id.fetch_add(1, Ordering::SeqCst) + 1;
    {
        let mut guard = state
            .tasks
            .lock()
            .map_err(|_| "task state poisoned".to_string())?;
        guard.insert(
            task_id,
            new_progress_payload("delete_dependency_scan_starting", "Starting dependency scan"),
        );
    }
    // Register a cancel flag so the modal's Cancel button (cancel_task) can
    // stop the walk mid-scan; partial results are still returned.
    let cancel_flag = Arc::new(AtomicBool::new(false));
    {
        let mut cancels = state
            .cancellations
            .lock()
            .map_err(|_| "cancellation state poisoned".to_string())?;
        cancels.insert(task_id, Arc::clone(&cancel_flag));
    }
    let tasks = Arc::clone(&state.tasks);
    thread::spawn(move || {
        let result = run_delete_dependency_scan_task(&tasks, task_id, request, cancel_flag);
        finish_delete_dependency_scan_task(&tasks, task_id, result);
    });

    Ok(TaskHandle { id: task_id })
}

// ----------------------------------------------------------------------------
// Backfill Resource Sizes — heals manifest-imported `resources.size = 0` rows
// by harvesting CRC->size from .var files in a folder. Cancellable via the
// shared cancel flag registered in AppState.cancellations.
// ----------------------------------------------------------------------------

fn finish_backfill_task(
    tasks: &Arc<Mutex<HashMap<u64, ProgressPayload>>>,
    task_id: u64,
    result: Result<BackfillSizesResponse>,
) {
    if let Ok(mut guard) = tasks.lock() {
        if let Some(task) = guard.get_mut(&task_id) {
            task.done = true;
            task.progress = 1.0;
            match result {
                Ok(payload) => {
                    task.phase = if payload.was_cancelled {
                        "backfill_cancelled".to_string()
                    } else {
                        "backfill_complete".to_string()
                    };
                    task.message = if payload.was_cancelled {
                        format!(
                            "Cancelled — patched {} row{} from {} file{}",
                            payload.rows_updated,
                            if payload.rows_updated == 1 { "" } else { "s" },
                            payload.files_scanned,
                            if payload.files_scanned == 1 { "" } else { "s" },
                        )
                    } else {
                        format!(
                            "Patched {} row{} from {} file{}",
                            payload.rows_updated,
                            if payload.rows_updated == 1 { "" } else { "s" },
                            payload.files_scanned,
                            if payload.files_scanned == 1 { "" } else { "s" },
                        )
                    };
                    task.backfill_result = Some(payload);
                    task.error = None;
                }
                Err(err) => {
                    task.phase = "backfill_failed".to_string();
                    task.message = "Backfill failed".to_string();
                    task.error = Some(err.to_string());
                }
            }
        }
    }
}

fn run_backfill_sizes(
    tasks: &Arc<Mutex<HashMap<u64, ProgressPayload>>>,
    task_id: u64,
    db: &Db,
    request: BackfillSizesRequest,
    cancel: Arc<AtomicBool>,
) -> Result<BackfillSizesResponse> {
    let dir_str = request.input_dir.trim();
    if dir_str.is_empty() {
        return Err(anyhow::anyhow!("input_dir is required"));
    }
    let dir = Path::new(dir_str);
    if !dir.is_dir() {
        return Err(anyhow::anyhow!("not a directory: {}", dir.display()));
    }

    set_task_progress(tasks, task_id, "backfill_walk", 0.01, "Listing .var files");
    let files = collect_local_var_files(dir)?;
    let total_files = files.len();
    if total_files == 0 {
        return Ok(BackfillSizesResponse {
            files_scanned: 0,
            crcs_collected: 0,
            rows_updated: 0,
            was_cancelled: false,
        });
    }

    let mut crc_sizes: HashMap<u32, u64> = HashMap::new();
    let mut files_scanned: u64 = 0;
    for (idx, file) in files.iter().enumerate() {
        if cancel.load(Ordering::SeqCst) {
            return Ok(BackfillSizesResponse {
                files_scanned,
                crcs_collected: crc_sizes.len() as u64,
                rows_updated: 0,
                was_cancelled: true,
            });
        }
        let pid = file
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();
        let progress = 0.02 + (idx as f64 / total_files as f64) * 0.68;
        set_task_progress(
            tasks,
            task_id,
            "backfill_harvest",
            progress,
            format!("Reading {} ({}/{})", pid, idx + 1, total_files),
        );

        let mut harvested: Vec<HarvestedResource> = Vec::new();
        if harvest_zip_crcs(
            &pid,
            &file.display().to_string(),
            request.include_vap,
            &mut harvested,
        )
        .is_err()
        {
            // Skip unreadable files; not fatal.
            files_scanned += 1;
            continue;
        }
        for r in harvested {
            // Defensive: keep the largest size seen for a CRC.
            crc_sizes
                .entry(r.crc32)
                .and_modify(|s| {
                    if r.size > *s {
                        *s = r.size;
                    }
                })
                .or_insert(r.size);
        }
        files_scanned += 1;
    }

    let total_crcs = crc_sizes.len();
    if total_crcs == 0 {
        return Ok(BackfillSizesResponse {
            files_scanned,
            crcs_collected: 0,
            rows_updated: 0,
            was_cancelled: false,
        });
    }

    set_task_progress(
        tasks,
        task_id,
        "backfill_update",
        0.72,
        format!("Updating {} matching rows", total_crcs),
    );

    let mut conn = db
        .conn
        .lock()
        .map_err(|_| anyhow::anyhow!("database connection poisoned"))?;
    let tx = conn.transaction()?;
    let mut rows_updated: u64 = 0;
    let mut was_cancelled = false;
    {
        let mut stmt = tx.prepare(
            "UPDATE resources SET size = ?1, effective_size = ?1
             WHERE crc32 = ?2 AND size = 0",
        )?;
        for (i, (crc, size)) in crc_sizes.iter().enumerate() {
            if i % 256 == 0 {
                if cancel.load(Ordering::SeqCst) {
                    was_cancelled = true;
                    break;
                }
                let progress = 0.72 + (i as f64 / total_crcs as f64) * 0.27;
                set_task_progress(
                    tasks,
                    task_id,
                    "backfill_update",
                    progress,
                    format!("Patching CRC {}/{}", i + 1, total_crcs),
                );
            }
            // Skip CRCs that resolved to zero (defensive).
            if *size == 0 {
                continue;
            }
            let changed = stmt.execute(rusqlite::params![*size as i64, *crc])? as u64;
            rows_updated += changed;
        }
    }
    tx.commit()?;

    Ok(BackfillSizesResponse {
        files_scanned,
        crcs_collected: total_crcs as u64,
        rows_updated,
        was_cancelled,
    })
}

#[tauri::command]
pub(crate) fn start_backfill_sizes_task(
    request: BackfillSizesRequest,
    state: State<'_, AppState>,
    db: State<'_, Db>,
) -> Result<TaskHandle, String> {
    let task_id = state.next_task_id.fetch_add(1, Ordering::SeqCst) + 1;
    {
        let mut guard = state
            .tasks
            .lock()
            .map_err(|_| "task state poisoned".to_string())?;
        guard.insert(
            task_id,
            new_progress_payload("backfill_starting", "Starting size backfill"),
        );
    }

    let cancel_flag = Arc::new(AtomicBool::new(false));
    {
        let mut cancels = state
            .cancellations
            .lock()
            .map_err(|_| "cancellation state poisoned".to_string())?;
        cancels.insert(task_id, Arc::clone(&cancel_flag));
    }

    let tasks = Arc::clone(&state.tasks);
    let db = db.inner().clone();
    thread::spawn(move || {
        let result = run_backfill_sizes(&tasks, task_id, &db, request, cancel_flag);
        finish_backfill_task(&tasks, task_id, result);
    });

    Ok(TaskHandle { id: task_id })
}

/// Scans a target VAR for broken dependency references using the local scan
/// cached under `input_dir`. Returns an error if no scan is cached — the
/// frontend prompts the user to scan via the Overview / Missing Resources
/// page first so the local resource index is available.
///
/// Kept for backward compatibility with any external callers; the in-app UI
/// now goes through `start_scan_missing_resources_task` so the UI thread
/// stays responsive while the I/O-heavy walk runs on a worker.
#[tauri::command]
pub(crate) fn scan_missing_resources(
    input_dir: String,
    additional_input_dirs: Option<Vec<String>>,
    target_var_path: String,
    state: State<'_, AppState>,
    db: State<'_, Db>,
) -> Result<Vec<BrokenRef>, String> {
    let additional = parse_additional_dirs(&additional_input_dirs.unwrap_or_default());
    let input_path = Path::new(&input_dir);
    let target_path = Path::new(&target_var_path);
    if !target_path.is_file() {
        return Err(format!(
            "target VAR not found on disk: {}",
            target_path.display()
        ));
    }

    let scanned = load_cached_scan_with_target(&state.scan_cache, input_path, &additional, Some(target_path))
        .map_err(|err| err.to_string())?
        .ok_or_else(|| {
            "No cached scan for this input folder. Run a scan from the Overview or Missing \
             Resources page first."
                .to_string()
        })?;

    scan_target_var_for_broken_refs(target_path, &scanned, &db).map_err(|err| err.to_string())
}

/// Background variant of `scan_missing_resources`. Returns immediately with a
/// `TaskHandle`; a worker thread runs `scan_target_var_for_broken_refs` and
/// the UI polls `get_task_progress` to pick up the result via
/// `ProgressPayload.missing_resources_result`. Without this, a target VAR
/// with thousands of text-ref references kept the IPC dispatcher tied up
/// long enough that the WebView event loop visibly froze during the
/// "Analyzing target VAR for broken refs…" phase.
#[tauri::command]
pub(crate) fn start_scan_missing_resources_task(
    input_dir: String,
    // The extra folders the cached scan was made with (its cache key).
    additional_input_dirs: Option<Vec<String>>,
    target_var_path: String,
    state: State<'_, AppState>,
    db: State<'_, Db>,
) -> Result<TaskHandle, String> {
    let task_id = state.next_task_id.fetch_add(1, Ordering::SeqCst) + 1;
    {
        let mut guard = state
            .tasks
            .lock()
            .map_err(|_| "task state poisoned".to_string())?;
        guard.insert(
            task_id,
            new_progress_payload("missing_scan_starting", "Starting analysis"),
        );
    }
    let additional = parse_additional_dirs(&additional_input_dirs.unwrap_or_default());

    let tasks = Arc::clone(&state.tasks);
    let scan_cache = Arc::clone(&state.scan_cache);
    let db = db.inner().clone();
    thread::spawn(move || {
        let result = (|| -> Result<Vec<BrokenRef>, String> {
            let input_path = Path::new(&input_dir);
            let target_path = Path::new(&target_var_path);
            if !target_path.is_file() {
                return Err(format!(
                    "target VAR not found on disk: {}",
                    target_path.display()
                ));
            }
            set_task_progress(
                &tasks,
                task_id,
                "missing_scan_loading",
                0.05,
                "Loading scan cache",
            );
            let scanned =
                load_cached_scan_with_target(&scan_cache, input_path, &additional, Some(target_path))
                    .map_err(|err| err.to_string())?
                    .ok_or_else(|| {
                        "No cached scan for this input folder. Run a scan first.".to_string()
                    })?;

            set_task_progress(
                &tasks,
                task_id,
                "missing_scan_analyzing",
                0.2,
                "Analyzing target VAR for broken refs",
            );
            scan_target_var_for_broken_refs(target_path, &scanned, &db)
                .map_err(|err| err.to_string())
        })();

        finish_scan_missing_resources_task(&tasks, task_id, result);
    });

    Ok(TaskHandle { id: task_id })
}

fn finish_scan_missing_resources_task(
    tasks: &Arc<Mutex<HashMap<u64, ProgressPayload>>>,
    task_id: u64,
    result: Result<Vec<BrokenRef>, String>,
) {
    if let Ok(mut guard) = tasks.lock() {
        if let Some(task) = guard.get_mut(&task_id) {
            task.done = true;
            task.progress = 1.0;
            match result {
                Ok(refs) => {
                    task.phase = "missing_scan_complete".to_string();
                    task.message = format!(
                        "Scan complete — {} broken reference{}.",
                        refs.len(),
                        if refs.len() == 1 { "" } else { "s" }
                    );
                    task.missing_resources_result = Some(refs);
                    task.error = None;
                }
                Err(err) => {
                    task.phase = "missing_scan_failed".to_string();
                    task.message = "Scan failed".to_string();
                    task.error = Some(err);
                }
            }
        }
    }
}

/// Per-click async DB lookup for the Missing Resources right panel. When
/// `crc32` is known (the broken resource was indexed before deletion) we use
/// the existing CRC index; otherwise we fall back to an exact
/// `internal_path` match across all indexed packages.
#[tauri::command]
pub(crate) fn find_db_candidates_for_broken_ref(
    crc32: Option<u32>,
    internal_path: String,
    exclude_pkg: Option<String>,
    search: Option<String>,
    offset: Option<i64>,
    limit: Option<i64>,
    db: State<'_, Db>,
) -> Result<crate::models::PagedDbCandidates, String> {
    let conn = db.read().map_err(|err| err.to_string())?;

    // Clamp pagination args. Over-large limits would let the UI request an
    // unreasonable page; offset is just trusted as the user paginates.
    let limit = limit.unwrap_or(50).clamp(1, 500);
    let offset = offset.unwrap_or(0).max(0);
    let exclude = exclude_pkg.unwrap_or_default();
    let search_lc = search
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty());
    // SQL LIKE wildcard around the user's term — same case-insensitive
    // substring match the rest of the app uses for ad-hoc filters.
    let search_like = search_lc
        .as_deref()
        .map(|s| format!("%{s}%"))
        .unwrap_or_default();
    let has_search = !search_like.is_empty();

    // Fetch one extra row to cheaply determine `has_more` without a separate
    // COUNT query (which would scan the whole filtered set).
    let fetch_limit = limit + 1;

    let mut items: Vec<ResourceDuplicateRef> = Vec::new();

    if let Some(crc) = crc32 {
        let sql = "SELECT r.package_id, p.file_path, r.internal_path, r.size \
                   FROM resources r \
                   LEFT JOIN packages p ON p.package_id = r.package_id \
                   WHERE r.crc32 = ?1 \
                     AND (?2 = '' OR r.package_id != ?2) \
                     AND (?3 = '' OR LOWER(r.package_id) LIKE ?3 \
                                  OR LOWER(r.internal_path) LIKE ?3) \
                   ORDER BY r.package_id COLLATE NOCASE, r.internal_path COLLATE NOCASE \
                   LIMIT ?4 OFFSET ?5";
        let mut stmt = conn.prepare(sql).map_err(|err| err.to_string())?;
        let rows = stmt
            .query_map(
                rusqlite::params![
                    crc as i64,
                    exclude,
                    if has_search { search_like.clone() } else { String::new() },
                    fetch_limit,
                    offset,
                ],
                |row| {
                    let package_id: String = row.get(0)?;
                    let package_file: Option<String> = row.get(1)?;
                    let internal_path: String = row.get(2)?;
                    let size: i64 = row.get(3)?;
                    Ok(ResourceDuplicateRef {
                        package_id,
                        package_file: package_file.unwrap_or_default(),
                        internal_path,
                        size: size.max(0) as u64,
                    })
                },
            )
            .map_err(|err| err.to_string())?;
        for row in rows {
            items.push(row.map_err(|err| err.to_string())?);
        }
    } else {
        let sql = "SELECT r.package_id, p.file_path, r.internal_path, r.size \
                   FROM resources r \
                   LEFT JOIN packages p ON p.package_id = r.package_id \
                   WHERE r.internal_path = ?1 \
                     AND (?2 = '' OR r.package_id != ?2) \
                     AND (?3 = '' OR LOWER(r.package_id) LIKE ?3 \
                                  OR LOWER(r.internal_path) LIKE ?3) \
                   ORDER BY r.package_id COLLATE NOCASE \
                   LIMIT ?4 OFFSET ?5";
        let mut stmt = conn.prepare(sql).map_err(|err| err.to_string())?;
        let rows = stmt
            .query_map(
                rusqlite::params![
                    internal_path,
                    exclude,
                    if has_search { search_like.clone() } else { String::new() },
                    fetch_limit,
                    offset,
                ],
                |row| {
                    let package_id: String = row.get(0)?;
                    let package_file: Option<String> = row.get(1)?;
                    let internal_path: String = row.get(2)?;
                    let size: i64 = row.get(3)?;
                    Ok(ResourceDuplicateRef {
                        package_id,
                        package_file: package_file.unwrap_or_default(),
                        internal_path,
                        size: size.max(0) as u64,
                    })
                },
            )
            .map_err(|err| err.to_string())?;
        for row in rows {
            items.push(row.map_err(|err| err.to_string())?);
        }
    }

    let has_more = items.len() as i64 > limit;
    if has_more {
        items.truncate(limit as usize);
    }
    Ok(crate::models::PagedDbCandidates { items, has_more })
}

/// Applies user-approved fixes to a target VAR: rewrites broken `Pkg:/path`
/// refs in payload text files, updates `meta.json` dependencies. The actual
/// work (opening the source .var, streaming entries through a ZipWriter,
/// rewriting text payloads, atomic-rename) is heavy I/O and must NOT run on
/// the Tauri command thread — that would freeze the webview until the rewrite
/// finishes. Spawns a worker thread and returns a `TaskHandle` immediately;
/// the UI polls `get_task_progress(task_id)` and reads `fix_report` off the
/// final payload.
#[allow(clippy::too_many_arguments)] // a Tauri command: one argument per field the UI sends
#[tauri::command]
pub(crate) fn start_apply_missing_resources_fix_task(
    input_dir: String,
    additional_input_dirs: Option<Vec<String>>,
    target_var_path: String,
    output_dir: Option<String>,
    replace_in_place: bool,
    fixes: Vec<FixDirective>,
    backup: bool,
    backup_dir: Option<String>,
    state: State<'_, AppState>,
    db: State<'_, Db>,
) -> Result<TaskHandle, String> {
    let task_id = state.next_task_id.fetch_add(1, Ordering::SeqCst) + 1;
    {
        let mut guard = state
            .tasks
            .lock()
            .map_err(|_| "task state poisoned".to_string())?;
        guard.insert(
            task_id,
            new_progress_payload("apply_fix_starting", "Starting fix"),
        );
    }
    let additional = parse_additional_dirs(&additional_input_dirs.unwrap_or_default());

    let tasks = Arc::clone(&state.tasks);
    let scan_cache = Arc::clone(&state.scan_cache);
    let db = db.inner().clone();
    thread::spawn(move || {
        let result = (|| -> Result<FixReport, String> {
            let input_path = Path::new(&input_dir);
            let target_path = Path::new(&target_var_path);
            if !target_path.is_file() {
                return Err(format!(
                    "target VAR not found on disk: {}",
                    target_path.display()
                ));
            }
            set_task_progress(&tasks, task_id, "apply_fix_loading", 0.05, "Loading scan cache");
            let scanned =
                load_cached_scan_with_target(&scan_cache, input_path, &additional, Some(target_path))
                    .map_err(|err| err.to_string())?
                    .ok_or_else(|| {
                        "No cached scan for this input folder. Run a scan first.".to_string()
                    })?;

            // Mirror Overview's execute layout:
            // `<output_dir>/changed/<basename>.var` for the rewritten file
            // (output-folder mode) and `<output_dir>/backup/` for originals
            // (replace-in-place + backup).
            let trimmed_output_dir = output_dir
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty());

            let output_path_owned = if replace_in_place {
                None
            } else {
                let dir = trimmed_output_dir.ok_or_else(|| {
                    "Output Folder is required unless 'Replace in place' is enabled."
                        .to_string()
                })?;
                let file_name = target_path
                    .file_name()
                    .ok_or_else(|| "invalid target file name".to_string())?;
                Some(Path::new(dir).join("changed").join(file_name))
            };
            // Checking the fixed copy and fixing it again would write it over
            // itself while reading it.
            if let Some(out) = &output_path_owned {
                if out.exists()
                    && fs::canonicalize(out).ok() == fs::canonicalize(target_path).ok()
                {
                    return Err(
                        "This is the fixed copy itself: fix it in place instead.".to_string()
                    );
                }
            }

            // Backup only makes sense when we're overwriting the original;
            // with an output-folder workflow the original IS the backup.
            // The folder the user chose for backups; else the output folder's
            // `backup`; else `fix-var-backup` next to the package.
            let chosen_backup_dir = backup_dir.as_deref().map(str::trim).filter(|s| !s.is_empty());
            let backup_root_owned = if backup && replace_in_place {
                if let Some(dir) = chosen_backup_dir {
                    Some(PathBuf::from(dir))
                } else if let Some(dir) = trimmed_output_dir {
                    Some(Path::new(dir).join("backup"))
                } else {
                    target_path.parent().map(|p| p.join("fix-var-backup"))
                }
            } else {
                None
            };

            set_task_progress(
                &tasks,
                task_id,
                "apply_fix_rewriting",
                0.15,
                "Rewriting target VAR",
            );
            apply_fix_var(
                target_path,
                output_path_owned.as_deref(),
                &fixes,
                &scanned,
                &db,
                backup_root_owned.as_deref(),
            )
            .map_err(|err| err.to_string())
        })();

        finish_apply_missing_resources_fix_task(&tasks, task_id, result);
    });

    Ok(TaskHandle { id: task_id })
}

fn finish_apply_missing_resources_fix_task(
    tasks: &Arc<Mutex<HashMap<u64, ProgressPayload>>>,
    task_id: u64,
    result: Result<FixReport, String>,
) {
    if let Ok(mut guard) = tasks.lock() {
        if let Some(task) = guard.get_mut(&task_id) {
            task.done = true;
            task.progress = 1.0;
            match result {
                Ok(report) => {
                    task.phase = "apply_fix_complete".to_string();
                    task.message = "Fix applied".to_string();
                    task.fix_report = Some(report);
                    task.error = None;
                }
                Err(err) => {
                    task.phase = "apply_fix_failed".to_string();
                    task.message = "Fix failed".to_string();
                    task.error = Some(err);
                }
            }
        }
    }
}

/// Scans a target VAR for *valid* external `Pkg:/path` refs that could be
/// internalized. Mirrors `scan_missing_resources` but on the opposite side of
/// the partition: only refs whose source pkg is present locally AND whose path
/// exists inside that source pkg are returned. Requires a cached scan over
/// `input_dir` (so the source vars can be resolved without re-scanning every
/// time the user opens the page).
#[tauri::command(async)]
pub(crate) fn scan_internalize_candidates(
    input_dir: String,
    additional_input_dirs: Option<Vec<String>>,
    target_var_path: String,
    state: State<'_, AppState>,
    db: State<'_, Db>,
) -> Result<Vec<ExternalRefGroup>, String> {
    let input_path = Path::new(&input_dir);
    let target_path = Path::new(&target_var_path);
    if !target_path.is_file() {
        return Err(format!(
            "target VAR not found on disk: {}",
            target_path.display()
        ));
    }

    let additional = parse_additional_dirs(&additional_input_dirs.unwrap_or_default());
    let scanned =
        load_cached_scan_with_target(&state.scan_cache, input_path, &additional, Some(target_path))
        .map_err(|err| err.to_string())?
        .ok_or_else(|| {
            "No cached scan for this input folder. Run a scan from the Overview or Missing \
             Resources page first."
                .to_string()
        })?;

    let mut groups =
        scan_target_var_for_external_refs(target_path, &scanned).map_err(|err| err.to_string())?;
    // Files a source package doesn't have: an exact copy elsewhere lets Copy
    // in bring them too (the missing-file check, as Fix Missing runs it).
    if groups.iter().any(|g| !g.other_refs.is_empty()) {
        let broken = crate::fix_var::scan_target_var_for_broken_refs(target_path, &scanned, &db)
            .map_err(|err| err.to_string())?;
        crate::internalize::fill_from_copies(&mut groups, &broken, target_path)
            .map_err(|err| err.to_string())?;
    }
    // Who else needs each source package, from the VAR Packages listing: the
    // one only this package uses can go once its files are copied in.
    if let Ok(cache) = state.var_packages_folder_cache.lock() {
        if let Some(cache) = cache.as_ref() {
            let paths: Vec<String> = groups.iter().map(|g| g.source_var_path.clone()).collect();
            // (A package that isn't installed has no path: no users to count.)
            let users = crate::library::users_of(&cache.items, &paths, &target_var_path);
            for (group, users) in groups.iter_mut().zip(users) {
                if let Some(mut users) = users {
                    group.used_by_others = Some(users.len() as u32);
                    users.truncate(5);
                    group.other_users = users;
                }
            }
        }
    }
    Ok(groups)
}

/// Runs the user-approved internalization plan on a target VAR — copies each
/// selected ref's bundle out of its source VAR into the target, rewrites the
/// matching `SourcePkg:/path` references to `SELF:/path`, and drops the source
/// pkg from `meta.json` dependencies when every ref to it was internalized.
/// I/O-heavy work runs on a worker thread; the UI polls `get_task_progress`
/// and reads `internalize_report` off the final payload.
#[allow(clippy::too_many_arguments)] // a Tauri command: one argument per field the UI sends
#[tauri::command]
pub(crate) fn start_apply_internalize_task(
    input_dir: String,
    additional_input_dirs: Option<Vec<String>>,
    target_var_path: String,
    output_dir: Option<String>,
    replace_in_place: bool,
    selections: Vec<InternalizeSelection>,
    backup: bool,
    backup_dir: Option<String>,
    state: State<'_, AppState>,
) -> Result<TaskHandle, String> {
    let task_id = state.next_task_id.fetch_add(1, Ordering::SeqCst) + 1;
    {
        let mut guard = state
            .tasks
            .lock()
            .map_err(|_| "task state poisoned".to_string())?;
        guard.insert(
            task_id,
            new_progress_payload("internalize_starting", "Starting internalize"),
        );
    }

    let tasks = Arc::clone(&state.tasks);
    let scan_cache = Arc::clone(&state.scan_cache);
    let additional = parse_additional_dirs(&additional_input_dirs.unwrap_or_default());
    thread::spawn(move || {
        let result = (|| -> Result<InternalizeReport, String> {
            let input_path = Path::new(&input_dir);
            let target_path = Path::new(&target_var_path);
            if !target_path.is_file() {
                return Err(format!(
                    "target VAR not found on disk: {}",
                    target_path.display()
                ));
            }
            set_task_progress(
                &tasks,
                task_id,
                "internalize_loading",
                0.05,
                "Loading scan cache",
            );
            let scanned =
                load_cached_scan_with_target(&scan_cache, input_path, &additional, Some(target_path))
                    .map_err(|err| err.to_string())?
                    .ok_or_else(|| {
                        "No cached scan for this input folder. Run a scan first.".to_string()
                    })?;

            // Output-folder convention matches the Missing Resources page:
            // `<output_dir>/changed/<basename>.var` for the rewritten file,
            // `<output_dir>/backup/<basename>.var` for the original when
            // replace-in-place + backup is on.
            let trimmed_output_dir = output_dir
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty());

            let output_path_owned = if replace_in_place {
                None
            } else {
                let dir = trimmed_output_dir.ok_or_else(|| {
                    "Output Folder is required unless 'Replace in place' is enabled."
                        .to_string()
                })?;
                let file_name = target_path
                    .file_name()
                    .ok_or_else(|| "invalid target file name".to_string())?;
                Some(Path::new(dir).join("changed").join(file_name))
            };

            // The folder chosen for backups, else <output>/backup, else a
            // sibling internalize-backup folder.
            let chosen_backup_dir = backup_dir
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let backup_root_owned = if backup && replace_in_place {
                if let Some(dir) = chosen_backup_dir {
                    Some(Path::new(dir).to_path_buf())
                } else if let Some(dir) = trimmed_output_dir {
                    Some(Path::new(dir).join("backup"))
                } else {
                    target_path.parent().map(|p| p.join("internalize-backup"))
                }
            } else {
                None
            };

            set_task_progress(
                &tasks,
                task_id,
                "internalize_rewriting",
                0.15,
                "Internalizing resources",
            );
            apply_internalize(
                target_path,
                output_path_owned.as_deref(),
                &scanned,
                &selections,
                backup_root_owned.as_deref(),
            )
            .map_err(|err| err.to_string())
        })();

        finish_apply_internalize_task(&tasks, task_id, result);
    });

    Ok(TaskHandle { id: task_id })
}

fn finish_apply_internalize_task(
    tasks: &Arc<Mutex<HashMap<u64, ProgressPayload>>>,
    task_id: u64,
    result: Result<InternalizeReport, String>,
) {
    if let Ok(mut guard) = tasks.lock() {
        if let Some(task) = guard.get_mut(&task_id) {
            task.done = true;
            task.progress = 1.0;
            match result {
                Ok(report) => {
                    task.phase = "internalize_complete".to_string();
                    task.message = "Internalize applied".to_string();
                    task.internalize_report = Some(report);
                    task.error = None;
                }
                Err(err) => {
                    task.phase = "internalize_failed".to_string();
                    task.message = "Internalize failed".to_string();
                    task.error = Some(err);
                }
            }
        }
    }
}
