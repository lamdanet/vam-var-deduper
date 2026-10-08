//! Offload and Restore: moving packages out of AddonPackages into an offload
//! folder beside it, where VaM no longer loads them, and back again.
//!
//! Offloading one package without what it needs is pointless and offloading a
//! dependency that something still in AddonPackages uses breaks that package,
//! so both directions start with a plan: the picked packages, every package
//! they need (transitively) resolved against the library listing, and for each
//! dependency who else uses it. The UI shows the plan, the user adjusts the
//! selection, and the move runs as a background task.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs,
    path::{Path, PathBuf},
    sync::{atomic::Ordering, Arc},
    thread,
};

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::{
    db::Db,
    library::{DepResolution, LibIndex},
    models::{AppState, OffloadFailure, OffloadMove, OffloadResponse, TaskHandle, VarPackageListItem},
    naming,
    packages::{
        begin_task, disabled_sidecar, existing_creator_dir, friendly_io_error, image_sidecars,
        is_in_addon_packages, prune_empty_dirs, wide_len, MAX_PATH_UTF16,
    },
    tasks::set_task_progress,
};

/// The default offload folder's name, created beside AddonPackages.
pub(crate) const OFFLOAD_FOLDER_NAME: &str = "AddonPackages_offload";

/// `path` relative to `root`, when it lies strictly inside it. Compared per
/// component and case-insensitively, so separator and drive-letter spelling
/// don't matter.
pub(crate) fn relative_to(path: &Path, root: &Path) -> Option<PathBuf> {
    let mut rest = path.components();
    for want in root.components() {
        let have = rest.next()?;
        if have.as_os_str().to_string_lossy().to_lowercase()
            != want.as_os_str().to_string_lossy().to_lowercase()
        {
            return None;
        }
    }
    let rel: PathBuf = rest.collect();
    (!rel.as_os_str().is_empty()).then_some(rel)
}

pub(crate) fn path_is_under(path: &Path, root: &Path) -> bool {
    relative_to(path, root).is_some()
}

fn same_dir(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        p.to_string_lossy()
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_lowercase()
    };
    norm(a) == norm(b)
}

/// The two folders must be distinct and neither inside the other: an offload
/// folder inside AddonPackages is still loaded by VaM, and AddonPackages
/// inside the offload folder would make every package look offloaded.
fn check_folders(addon: &Path, offload: &Path) -> Result<(), String> {
    if same_dir(addon, offload) {
        return Err("The offload folder can't be AddonPackages itself.".to_string());
    }
    if path_is_under(offload, addon) {
        return Err(
            "The offload folder is inside AddonPackages, so VaM would still load what is moved \
             there. Pick a folder outside it in Settings."
                .to_string(),
        );
    }
    if path_is_under(addon, offload) {
        return Err("AddonPackages is inside the offload folder — pick another one in Settings.".to_string());
    }
    Ok(())
}

// ----------------------------------------------------------------------------
// Plan
// ----------------------------------------------------------------------------

/// What a plan is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum PlanMode {
    /// Move the packages and their dependencies out of AddonPackages.
    #[default]
    Offload,
    /// Move offloaded packages and their dependencies back.
    Restore,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct OffloadPlanEntry {
    pub(crate) package_id: String,
    pub(crate) file_path: String,
    pub(crate) size_bytes: u64,
    pub(crate) pkg_type: String,
    /// `"active"` (in AddonPackages), `"offloaded"`, or `"other"` (another
    /// scanned folder, which neither direction touches).
    pub(crate) location: String,
    /// Dependencies: a picked package lists it itself, rather than it coming in
    /// through another dependency.
    pub(crate) direct: bool,
    /// Dependencies: no exact match, so this is the version VaM falls back to.
    pub(crate) other_version: bool,
    /// File paths of plan members (picked packages and dependencies) that
    /// depend on this one.
    pub(crate) required_by: Vec<String>,
    /// Package ids of packages outside the plan that depend on this one —
    /// offloading it breaks them. Only packages in AddonPackages count
    /// (nothing else is loaded).
    pub(crate) used_by: Vec<String>,
    /// The plan can act on it (offload: in AddonPackages; restore: offloaded).
    pub(crate) movable: bool,
    /// Dependencies: nothing outside the plan needs it, directly or through a
    /// dependency that stays — so offloading it breaks nothing.
    pub(crate) safe: bool,
    /// Offload: `safe`; Restore: everything movable.
    pub(crate) default_selected: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct OffloadPlan {
    pub(crate) mode: PlanMode,
    pub(crate) targets: Vec<OffloadPlanEntry>,
    pub(crate) deps: Vec<OffloadPlanEntry>,
    /// Dependency keys no version of which is in the library.
    pub(crate) missing: Vec<String>,
}

fn location_of(item: &VarPackageListItem) -> &'static str {
    if item.offloaded {
        "offloaded"
    } else if is_in_addon_packages(Path::new(&item.file_path)) {
        "active"
    } else {
        "other"
    }
}

/// The plan for `targets` (indices into `items`).
pub(crate) fn build_plan(items: &[VarPackageListItem], targets: &[usize], mode: PlanMode) -> OffloadPlan {
    let index = LibIndex::build(items);
    let resolve = |dep: &str| match index.resolve(dep) {
        DepResolution::Found(t) => Some((t, false)),
        DepResolution::OtherVersion(t) => Some((t, true)),
        DepResolution::Missing => None,
    };

    let target_set: HashSet<usize> = targets.iter().copied().collect();
    let mut in_plan: HashSet<usize> = target_set.clone();
    let mut order: Vec<usize> = Vec::new();
    let mut direct: HashSet<usize> = HashSet::new();
    let mut exact: HashSet<usize> = HashSet::new();
    let mut missing: Vec<String> = Vec::new();
    let mut missing_seen: HashSet<String> = HashSet::new();

    // Everything the picked packages need, however deep.
    let mut queue: VecDeque<usize> = targets.iter().copied().collect();
    while let Some(i) = queue.pop_front() {
        for dep in &items[i].deps {
            let Some((t, other)) = resolve(dep) else {
                if missing_seen.insert(dep.to_ascii_lowercase()) {
                    missing.push(dep.clone());
                }
                continue;
            };
            if t == i {
                continue;
            }
            if target_set.contains(&i) {
                direct.insert(t);
            }
            if !other {
                exact.insert(t);
            }
            if in_plan.insert(t) {
                order.push(t);
                queue.push_back(t);
            }
        }
    }

    let mut required_by: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut used_by: HashMap<usize, Vec<usize>> = HashMap::new();
    for (u, item) in items.iter().enumerate() {
        let member = in_plan.contains(&u);
        // Only packages VaM loads can be broken by an offload.
        if !member && location_of(item) != "active" {
            continue;
        }
        let mut seen: HashSet<usize> = HashSet::new();
        for dep in &item.deps {
            let Some((t, _)) = resolve(dep) else { continue };
            if t == u || !in_plan.contains(&t) || !seen.insert(t) {
                continue;
            }
            if member {
                required_by.entry(t).or_default().push(u);
            } else {
                used_by.entry(t).or_default().push(u);
            }
        }
    }

    let movable = |i: usize| match mode {
        PlanMode::Offload => location_of(&items[i]) == "active",
        PlanMode::Restore => location_of(&items[i]) == "offloaded",
    };

    // The safe set: a dependency is left out when a package outside the plan
    // uses it, or a dependency that stays (left unselected) needs it. For
    // Offload only packages in AddonPackages "stay" in the sense that matters.
    let mut safe: HashSet<usize> = order
        .iter()
        .copied()
        .filter(|&d| movable(d) && !used_by.contains_key(&d))
        .collect();
    loop {
        let staying_user = |d: usize, safe: &HashSet<usize>| {
            required_by.get(&d).is_some_and(|users| {
                users.iter().any(|&r| {
                    !target_set.contains(&r)
                        && location_of(&items[r]) == "active"
                        && !safe.contains(&r)
                })
            })
        };
        let drop: Vec<usize> = safe.iter().copied().filter(|&d| staying_user(d, &safe)).collect();
        if drop.is_empty() {
            break;
        }
        for d in drop {
            safe.remove(&d);
        }
    }
    // Restore brings back everything the packages need; Offload takes what is
    // safe.
    let selected = |i: usize, is_target: bool| match mode {
        PlanMode::Restore => movable(i),
        PlanMode::Offload => (is_target && movable(i)) || safe.contains(&i),
    };

    let paths = |list: Option<&Vec<usize>>| -> Vec<String> {
        list.map(|v| v.iter().map(|&i| items[i].file_path.clone()).collect()).unwrap_or_default()
    };
    let ids = |list: Option<&Vec<usize>>| -> Vec<String> {
        let mut out: Vec<String> = list
            .map(|v| v.iter().map(|&i| items[i].package_id.clone()).collect())
            .unwrap_or_default();
        out.sort_by_key(|id| id.to_lowercase());
        out
    };
    let entry = |i: usize, is_target: bool| {
        let item = &items[i];
        OffloadPlanEntry {
            package_id: item.package_id.clone(),
            file_path: item.file_path.clone(),
            size_bytes: item.size_bytes,
            pkg_type: item.pkg_type.clone(),
            location: location_of(item).to_string(),
            direct: !is_target && direct.contains(&i),
            other_version: !is_target && !exact.contains(&i),
            required_by: paths(required_by.get(&i)),
            used_by: ids(used_by.get(&i)),
            movable: movable(i),
            safe: !is_target && safe.contains(&i),
            default_selected: selected(i, is_target),
        }
    };

    let mut deps: Vec<OffloadPlanEntry> = order.iter().map(|&i| entry(i, false)).collect();
    deps.sort_by(|a, b| {
        b.direct
            .cmp(&a.direct)
            .then_with(|| a.package_id.to_lowercase().cmp(&b.package_id.to_lowercase()))
    });
    missing.sort_by_key(|k| k.to_lowercase());
    OffloadPlan {
        mode,
        targets: targets.iter().map(|&i| entry(i, true)).collect(),
        deps,
        missing,
    }
}

/// What Offload or Restore would act on for the picked
/// packages, from the listing the VAR Packages page is showing.
#[tauri::command(async)]
pub(crate) fn plan_offload(
    file_paths: Vec<String>,
    mode: PlanMode,
    state: State<'_, AppState>,
) -> Result<OffloadPlan, String> {
    let cache = state
        .var_packages_folder_cache
        .lock()
        .map_err(|_| "var packages folder cache poisoned".to_string())?;
    let items: &[VarPackageListItem] = cache.as_ref().map(|c| c.items.as_slice()).unwrap_or(&[]);
    let by_path: HashMap<String, usize> = items
        .iter()
        .enumerate()
        .map(|(i, it)| (it.file_path.to_lowercase(), i))
        .collect();
    let mut targets: Vec<usize> = Vec::new();
    for fp in &file_paths {
        let i = by_path
            .get(&fp.to_lowercase())
            .copied()
            .ok_or_else(|| format!("{fp} is not in the current listing — rescan and try again."))?;
        if !targets.contains(&i) {
            targets.push(i);
        }
    }
    Ok(build_plan(items, &targets, mode))
}

// ----------------------------------------------------------------------------
// Moving
// ----------------------------------------------------------------------------

/// `fs::rename`, falling back to copy + delete when the folders are on
/// different drives (the offload folder can be anywhere).
fn move_file(src: &Path, dest: &Path) -> Result<(), String> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("Couldn't create {}: {e}", parent.display()))?;
    }
    match fs::rename(src, dest) {
        Ok(()) => Ok(()),
        // ERROR_NOT_SAME_DEVICE on Windows, EXDEV elsewhere.
        Err(err) if matches!(err.raw_os_error(), Some(17) | Some(18)) => {
            fs::copy(src, dest).map_err(|e| {
                let _ = fs::remove_file(dest);
                friendly_io_error(&e, "copy")
            })?;
            fs::remove_file(src).map_err(|e| {
                // Never leave the package in both places.
                let _ = fs::remove_file(dest);
                friendly_io_error(&e, "move")
            })
        }
        Err(err) => Err(friendly_io_error(&err, "move")),
    }
}

/// Where `src` goes: `<dest_root>/<Creator>/<file>` when filing by creator (and
/// the name has a usable creator), else the same path relative to `dest_root`
/// that it had relative to `src_root`.
fn destination(src: &Path, src_root: &Path, dest_root: &Path, by_creator: bool) -> Result<PathBuf, String> {
    let rel = relative_to(src, src_root)
        .ok_or_else(|| format!("It isn't inside {}.", src_root.display()))?;
    let file_name = src.file_name().ok_or_else(|| "Bad file name.".to_string())?;
    if by_creator {
        let folder = src
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(naming::creator_from_package_id)
            .and_then(naming::sanitize_creator_folder);
        if let Some(folder) = folder {
            return Ok(existing_creator_dir(dest_root, &folder).join(file_name));
        }
    }
    Ok(dest_root.join(rel))
}

/// Moves one package (and its `.disabled` marker and preview images). Returns
/// the destination and any notes about sidecars that stayed behind.
pub(crate) fn move_package(
    src: &Path,
    src_root: &Path,
    dest_root: &Path,
    by_creator: bool,
    restore: bool,
) -> Result<(PathBuf, Vec<String>), String> {
    let is_var = src
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("var"));
    if !is_var {
        return Err("Not a .var file.".to_string());
    }
    if !src.is_file() {
        return Err("That package is gone — rescan.".to_string());
    }
    let dest = destination(src, src_root, dest_root, by_creator)?;
    if dest.exists() {
        return Err(format!("{} already exists.", dest.display()));
    }
    // VaM (Mono) can't open long paths, so a restore must not create one.
    if restore && wide_len(&dest) >= MAX_PATH_UTF16 {
        return Err("The path in AddonPackages would be too long for VaM to load.".to_string());
    }
    move_file(src, &dest)?;

    let mut notes = Vec::new();
    let marker = disabled_sidecar(src);
    if marker.exists() {
        if let Err(err) = move_file(&marker, &disabled_sidecar(&dest)) {
            notes.push(format!("its .disabled marker stayed behind: {err}"));
        }
    }
    if let Some(dest_dir) = dest.parent() {
        for img in image_sidecars(src) {
            let Some(name) = img.file_name() else { continue };
            let img_dest = dest_dir.join(name);
            if img_dest.exists() {
                notes.push("its preview image stayed behind — one already exists there".to_string());
            } else if let Err(err) = move_file(&img, &img_dest) {
                notes.push(format!("its preview image stayed behind: {err}"));
            }
        }
    }
    Ok((dest, notes))
}

/// Points the database's `packages.file_path` at the new location so the
/// database pages keep opening moved packages. Best effort.
fn repoint_db_paths(db: &Db, moves: &[OffloadMove]) {
    let Ok(mut conn) = db.conn.lock() else { return };
    let Ok(tx) = conn.transaction() else { return };
    {
        let Ok(mut stmt) = tx.prepare("UPDATE packages SET file_path = ?2 WHERE file_path = ?1") else {
            return;
        };
        for m in moves {
            let _ = stmt.execute(rusqlite::params![m.from, m.to]);
        }
    }
    let _ = tx.commit();
}

/// The folder a restored package is moved out of: the first of `roots` that
/// holds it. A root that is (or is inside) AddonPackages never counts — what is
/// there is already loaded.
pub(crate) fn restore_source_root<'a>(src: &Path, roots: &'a [PathBuf], addon: &Path) -> Option<&'a PathBuf> {
    roots
        .iter()
        .filter(|root| !same_dir(root, addon) && !path_is_under(root, addon))
        .find(|root| path_is_under(src, root))
}

/// Offloads (or, with `restore`, restores) the given packages as a background
/// task. `addon_dir` is AddonPackages and `offload_dir` the offload folder;
/// `by_creator` files each package under a creator folder at the destination.
/// A restore also takes packages from `source_dirs` (Scan Dependencies' extra
/// folders) into AddonPackages.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub(crate) fn start_offload_task(
    file_paths: Vec<String>,
    restore: bool,
    addon_dir: String,
    offload_dir: String,
    by_creator: bool,
    source_dirs: Option<Vec<String>>,
    state: State<'_, AppState>,
    db: State<'_, Db>,
) -> Result<TaskHandle, String> {
    let addon = PathBuf::from(addon_dir.trim());
    let offload = PathBuf::from(offload_dir.trim());
    if addon.as_os_str().is_empty() || !addon.is_dir() {
        return Err("Set your VaM directory in Settings first.".to_string());
    }
    if offload.as_os_str().is_empty() {
        return Err("Set an offload folder in Settings first.".to_string());
    }
    check_folders(&addon, &offload)?;
    if !restore {
        fs::create_dir_all(&offload)
            .map_err(|e| format!("Couldn't create the offload folder {}: {e}", offload.display()))?;
    }
    // Restore: every folder a package may come from, the offload folder first.
    let source_roots: Vec<PathBuf> = std::iter::once(offload.clone())
        .chain(
            source_dirs
                .unwrap_or_default()
                .iter()
                .map(|d| d.trim())
                .filter(|d| !d.is_empty())
                .map(PathBuf::from),
        )
        .collect();
    let dest_root = if restore { addon.clone() } else { offload.clone() };

    let verb = if restore { "Restoring" } else { "Offloading" };
    let (task_id, tasks, cancel) = begin_task(&state, "offload_starting", verb)?;
    let folder_cache = Arc::clone(&state.var_packages_folder_cache);
    let generation = Arc::clone(&state.var_packages_cache_generation);
    let info_cache = Arc::clone(&state.var_info_cache);
    let db = db.inner().clone();

    // Any listing walked from here on is torn until the moves finish.
    generation.fetch_add(1, Ordering::SeqCst);

    thread::spawn(move || {
        let mut response = OffloadResponse { restore, ..Default::default() };
        let mut left_dirs: Vec<(PathBuf, PathBuf)> = Vec::new();
        let total = file_paths.len().max(1) as f64;
        for (n, fp) in file_paths.iter().enumerate() {
            if cancel.load(Ordering::SeqCst) {
                response.was_cancelled = true;
                break;
            }
            let src = PathBuf::from(fp);
            let package_id = src
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            set_task_progress(
                &tasks,
                task_id,
                "offload_moving",
                n as f64 / total,
                format!("{verb} {package_id} ({}/{})", n + 1, file_paths.len()),
            );
            let size = fs::metadata(&src).map(|m| m.len()).unwrap_or(0);
            let src_root = if restore {
                match restore_source_root(&src, &source_roots, &addon) {
                    Some(root) => root.clone(),
                    None => {
                        response.failed.push(OffloadFailure {
                            package_id,
                            file_path: fp.clone(),
                            error: "It isn't in the offload folder or a scan folder.".to_string(),
                        });
                        continue;
                    }
                }
            } else {
                addon.clone()
            };
            match move_package(&src, &src_root, &dest_root, by_creator, restore) {
                Ok((dest, notes)) => {
                    if let Some(parent) = src.parent() {
                        left_dirs.push((parent.to_path_buf(), src_root.clone()));
                    }
                    response.moved_bytes += size;
                    response
                        .notes
                        .extend(notes.into_iter().map(|note| format!("{package_id}: {note}")));
                    response.moved.push(OffloadMove {
                        package_id,
                        from: fp.clone(),
                        to: dest.display().to_string(),
                    });
                }
                Err(error) => response.failed.push(OffloadFailure {
                    package_id,
                    file_path: fp.clone(),
                    error,
                }),
            }
        }

        // A creator folder whose last package just left shouldn't linger.
        let mut by_root: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
        for (dir, root) in left_dirs {
            by_root.entry(root).or_default().push(dir);
        }
        for (root, dirs) in by_root {
            prune_empty_dirs(dirs, &root);
        }

        let renamed: Vec<(String, String)> =
            response.moved.iter().map(|m| (m.from.clone(), m.to.clone())).collect();
        crate::library::rekey_info_cache(&info_cache, &db, &renamed);
        repoint_db_paths(&db, &response.moved);
        generation.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut cache) = folder_cache.lock() {
            *cache = None;
        }

        if let Ok(mut guard) = tasks.lock() {
            if let Some(task) = guard.get_mut(&task_id) {
                task.done = true;
                task.progress = 1.0;
                task.phase = if response.was_cancelled { "offload_cancelled" } else { "offload_complete" }
                    .to_string();
                task.message = format!(
                    "{} {} package(s){}",
                    if restore { "Restored" } else { "Offloaded" },
                    response.moved.len(),
                    if response.failed.is_empty() {
                        String::new()
                    } else {
                        format!(", {} failed", response.failed.len())
                    }
                );
                task.offload_result = Some(response);
            }
        }
    });

    Ok(TaskHandle { id: task_id })
}
