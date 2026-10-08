//! VAR Packages "Collect Dependencies".
//!
//! Scan Dependencies checks a package's dependencies against the VAM library
//! only, so one sitting in a download folder elsewhere on disk shows up as
//! "missing" with a Hub link. This finds them in folders the user picks and
//! files them beside the package:
//!
//! ```text
//! <library>/<Creator>/
//!     Creator.Pkg.3.var      <- moved here, with its .disabled marker and preview image
//!     Creator.Pkg.3.jpg
//!     deps/
//!         Other.Dep.2.var    <- copied from the search folders
//! ```
//!
//! Dependencies are COPIED, never moved: the search folders are the user's own
//! download archive. The package itself MOVES, by exactly the rules of "Move to
//! creator folder" (`creator_folder_destination` + `move_package_with_sidecars`).

use std::{
    collections::{BTreeSet, HashMap, HashSet, VecDeque},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread,
    time::SystemTime,
};

use tauri::State;
use walkdir::WalkDir;

use crate::{
    models::{
        AppState, CollectDepCopyResult, CollectDepItem, CollectDepsCopyResponse,
        CollectDepsScanResponse, TaskHandle, TaskMap, VarPackagesFolderCache,
    },
    naming,
    packages::{
        begin_task, creator_folder_destination, existing_creator_dir, is_in_addon_packages,
        move_package_with_sidecars, same_path_spelling, wide_len, FileOps, RealFileOps,
        APP_MANAGED_DIRS, MAX_DIR_UTF16, MAX_PATH_UTF16, OUT_OF_ADDON_PACKAGES,
    },
    tasks::{read_var_dependency_sets, set_task_progress},
    utils::format_bytes,
};

/// The folder, inside the package's creator folder, dependencies are copied into.
pub(crate) const DEPS_DIR_NAME: &str = "deps";

/// Read/write buffer for dependency copies. Big enough that a multi-GB `.var`
/// is a few hundred chunks, small enough that Cancel answers promptly.
const COPY_CHUNK: usize = 8 * 1024 * 1024;

fn is_var_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("var"))
        .unwrap_or(false)
}

fn file_name_of(path: &Path) -> Option<String> {
    path.file_name().map(|n| n.to_string_lossy().to_string())
}

// ----------------------------------------------------------------------------
// Walking
// ----------------------------------------------------------------------------

/// One `.var` seen while walking the search folders.
#[derive(Debug, Clone)]
pub(crate) struct FoundVar {
    pub(crate) path: PathBuf,
    /// The file stem, lowercased: the package id VAM would load it as.
    pub(crate) id_lc: String,
    pub(crate) version: Option<u64>,
    pub(crate) size: u64,
}

/// The folders worth walking: missing ones are reported back rather than
/// failing the scan, and a folder inside another picked folder is dropped
/// (picking `D:\Downloads` and `D:\Downloads\VAM` would otherwise walk the
/// second one twice). Returns `(roots, missing)`.
pub(crate) fn usable_roots(dirs: &[String]) -> (Vec<PathBuf>, Vec<String>) {
    let mut missing = Vec::new();
    let mut found: Vec<(PathBuf, PathBuf)> = Vec::new();
    for raw in dirs {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let path = PathBuf::from(trimmed);
        if !path.is_dir() {
            missing.push(trimmed.to_string());
            continue;
        }
        let canonical = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        found.push((canonical, path));
    }
    // Shallowest first, so a parent is always kept before anything inside it
    // is considered. `starts_with` also drops an exact repeat of a kept root.
    found.sort_by_key(|(canonical, _)| canonical.components().count());
    let mut kept: Vec<(PathBuf, PathBuf)> = Vec::new();
    for (canonical, path) in found {
        if kept.iter().any(|(k, _)| canonical.starts_with(k)) {
            continue;
        }
        kept.push((canonical, path));
    }
    (kept.into_iter().map(|(_, path)| path).collect(), missing)
}

/// Every `.var` under `roots` with its size. Unreadable subfolders are skipped,
/// not fatal: one locked folder in a big download archive must not hide the
/// rest of it.
///
/// `skip_managed` prunes this app's own backup trees (`APP_MANAGED_DIRS`) below
/// each root, counting them into `skipped_managed`. A root the user picked is
/// never pruned, even one named `backup`. Returns `None` when cancelled.
fn walk_vars(
    roots: &[PathBuf],
    skip_managed: bool,
    cancel: &AtomicBool,
    skipped_managed: &mut usize,
    on_count: &mut dyn FnMut(usize),
) -> Option<Vec<(PathBuf, u64)>> {
    let mut out = Vec::new();
    let mut visited = 0usize;
    for root in roots {
        let mut walker = WalkDir::new(root).into_iter();
        while let Some(next) = walker.next() {
            let Ok(entry) = next else { continue };
            visited += 1;
            if visited.is_multiple_of(512) {
                if cancel.load(Ordering::SeqCst) {
                    return None;
                }
                on_count(out.len());
            }
            if entry.file_type().is_dir() {
                let name = entry.file_name().to_string_lossy();
                if skip_managed
                    && entry.depth() > 0
                    && APP_MANAGED_DIRS
                        .iter()
                        .any(|d| name.eq_ignore_ascii_case(d))
                {
                    *skipped_managed += 1;
                    walker.skip_current_dir();
                }
                continue;
            }
            if !entry.file_type().is_file() || !is_var_path(entry.path()) {
                continue;
            }
            // Free on Windows: walkdir keeps the size from the directory read.
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            out.push((entry.into_path(), size));
        }
    }
    if cancel.load(Ordering::SeqCst) {
        return None;
    }
    Some(out)
}

/// Package family (lowercased base) -> the newest library file of that family.
///
/// Presence is decided per FAMILY, exactly as `tasks::build_dependency_items`
/// does, so this modal and Scan Dependencies agree on what "missing" means —
/// the rows that sent the user here are the ones it goes looking for.
pub(crate) fn index_library(files: Vec<(PathBuf, u64)>) -> HashMap<String, PathBuf> {
    let mut best: HashMap<String, (PathBuf, u64)> = HashMap::new();
    for (path, _size) in files {
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let base = naming::package_base(stem).to_ascii_lowercase();
        let version = naming::package_version(stem).unwrap_or(0);
        match best.get(&base) {
            Some((_, existing)) if *existing >= version => {}
            _ => {
                best.insert(base, (path, version));
            }
        }
    }
    best.into_iter()
        .map(|(base, (path, _))| (base, path))
        .collect()
}

/// Package family (lowercased base) -> every file of it in the search folders,
/// leaving out the package being collected for: it is never its own source.
pub(crate) fn index_search(
    files: Vec<(PathBuf, u64)>,
    package_path: &Path,
) -> HashMap<String, Vec<FoundVar>> {
    let package_stem_lc = package_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let package_canonical = fs::canonicalize(package_path).ok();

    let mut index: HashMap<String, Vec<FoundVar>> = HashMap::new();
    for (path, size) in files {
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let id_lc = stem.to_ascii_lowercase();
        // Only a same-named file can be the package, so only those pay for a
        // canonicalize — the spelling check alone misses junctions and the like.
        if id_lc == package_stem_lc
            && (same_path_spelling(&path, package_path)
                || (package_canonical.is_some()
                    && fs::canonicalize(&path).ok() == package_canonical))
        {
            continue;
        }
        let base = naming::package_base(stem).to_ascii_lowercase();
        index.entry(base).or_default().push(FoundVar {
            version: naming::package_version(stem),
            id_lc,
            path,
            size,
        });
    }
    index
}

// ----------------------------------------------------------------------------
// Matching
// ----------------------------------------------------------------------------

/// A family's files best-first for one declared dependency id, each flagged
/// with whether it IS the version asked for.
///
/// The exact id wins. Otherwise the newest version: it is what VAM falls back
/// to when the exact one is missing, and what `.latest` (or no version at all)
/// means. Then the larger file, because a cut-off download is smaller than the
/// real thing. Then path order, so the pick is stable between scans.
pub(crate) fn rank_dep_candidates<'a>(
    dep_id: &str,
    files: &'a [FoundVar],
) -> Vec<(&'a FoundVar, bool)> {
    let dep_lc = dep_id.trim().to_ascii_lowercase();
    let wants_exact = naming::package_version(&dep_lc).is_some();
    let mut ranked: Vec<(&FoundVar, bool)> = files
        .iter()
        .map(|f| (f, !wants_exact || f.id_lc == dep_lc))
        .collect();
    ranked.sort_by(|(a, a_exact), (b, b_exact)| {
        b_exact
            .cmp(a_exact)
            // Option orders None first, so descending puts unversioned last.
            .then_with(|| b.version.cmp(&a.version))
            .then_with(|| b.size.cmp(&a.size))
            .then_with(|| a.path.cmp(&b.path))
    });
    ranked
}

/// Resolves every declared dependency against the library first, then the
/// search folders. A dependency found in the search folders has its own
/// meta.json read too, and anything it needs that was not already listed is
/// resolved the same way: a different version than the one the package was
/// built against can bring dependencies of its own.
///
/// A candidate that can't be read is skipped for the next best one, so a
/// truncated download never gets copied in place of a good copy elsewhere.
/// Returns `None` when cancelled.
pub(crate) fn resolve_dependencies(
    package_id: &str,
    declared: &BTreeSet<String>,
    library: &HashMap<String, PathBuf>,
    search: &HashMap<String, Vec<FoundVar>>,
    cancel: &AtomicBool,
) -> Option<Vec<CollectDepItem>> {
    let self_lc = package_id.to_ascii_lowercase();
    let mut seen: HashSet<String> = declared.iter().map(|d| d.to_ascii_lowercase()).collect();
    seen.insert(self_lc.clone());
    let mut queue: VecDeque<(String, Option<String>)> =
        declared.iter().map(|d| (d.clone(), None)).collect();
    let mut items: Vec<CollectDepItem> = Vec::new();

    while let Some((dep_id, via)) = queue.pop_front() {
        if cancel.load(Ordering::SeqCst) {
            return None;
        }
        if dep_id.to_ascii_lowercase() == self_lc {
            continue;
        }
        let base_lc = naming::package_base(&dep_id).to_ascii_lowercase();
        let mut item = CollectDepItem {
            creator: naming::creator_from_package_id(&dep_id).map(str::to_string),
            version: naming::package_version_segment(&dep_id).map(str::to_string),
            via,
            package_id: dep_id.clone(),
            ..Default::default()
        };

        if let Some(lib_path) = library.get(&base_lc) {
            item.status = "in_library".to_string();
            item.path = Some(lib_path.display().to_string());
            item.file_name = file_name_of(lib_path);
            items.push(item);
            continue;
        }

        let candidates = search
            .get(&base_lc)
            .map(|files| rank_dep_candidates(&dep_id, files))
            .unwrap_or_default();
        let mut damaged: Vec<String> = Vec::new();
        let mut chosen: Option<(&FoundVar, bool, BTreeSet<String>)> = None;
        for (cand, exact) in candidates {
            match read_var_dependency_sets(&cand.path) {
                Ok((_, nested)) => {
                    chosen = Some((cand, exact, nested));
                    break;
                }
                Err(_) => damaged.push(cand.path.display().to_string()),
            }
        }

        match chosen {
            Some((cand, exact, nested)) => {
                item.status = "found".to_string();
                item.path = Some(cand.path.display().to_string());
                item.file_name = file_name_of(&cand.path);
                item.size = Some(cand.size);
                item.exact = exact;
                let mut notes: Vec<String> = Vec::new();
                if !exact {
                    let stem = cand
                        .path
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_default();
                    notes.push(format!(
                        "That exact version isn't in your folders — this is {stem}, the newest one there."
                    ));
                }
                if !damaged.is_empty() {
                    notes.push(format!(
                        "Skipped {} damaged cop{} (incomplete download?): {}",
                        damaged.len(),
                        if damaged.len() == 1 { "y" } else { "ies" },
                        damaged.join(", "),
                    ));
                }
                if !notes.is_empty() {
                    item.note = Some(notes.join(" "));
                }
                for nested_id in nested {
                    let trimmed = nested_id.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    if seen.insert(trimmed.to_ascii_lowercase()) {
                        queue.push_back((trimmed.to_string(), Some(dep_id.clone())));
                    }
                }
            }
            None => {
                item.status = "missing".to_string();
                if !damaged.is_empty() {
                    item.note = Some(format!(
                        "Found but can't be read (incomplete download?): {}",
                        damaged.join(", "),
                    ));
                }
            }
        }
        items.push(item);
    }

    // What can be copied first, then what can't be found, then what VAM
    // already has; alphabetical within each group.
    fn rank(status: &str) -> u8 {
        match status {
            "found" => 0,
            "missing" => 1,
            _ => 2,
        }
    }
    items.sort_by(|a, b| {
        rank(&a.status).cmp(&rank(&b.status)).then_with(|| {
            a.package_id
                .to_ascii_lowercase()
                .cmp(&b.package_id.to_ascii_lowercase())
        })
    });
    Some(items)
}

// ----------------------------------------------------------------------------
// Destination
// ----------------------------------------------------------------------------

/// Where a copy files things.
#[derive(Debug, Clone)]
pub(crate) struct CollectDestination {
    /// `<library>/<Creator>/<package file name>`.
    pub(crate) package_dest: PathBuf,
    pub(crate) creator_dir: PathBuf,
    pub(crate) deps_dir: PathBuf,
    /// The package is already there, so nothing moves.
    pub(crate) in_place: bool,
}

/// Works out the destination without touching anything, refusing up front
/// whatever the copy would refuse — so the scan can say so before the user
/// clicks Copy, and the copy fails before writing a single file.
pub(crate) fn plan_destination(
    var_path: &Path,
    root_dir: &str,
) -> Result<CollectDestination, String> {
    let package_dest = creator_folder_destination(var_path, root_dir)?;
    let in_place = same_path_spelling(&package_dest, var_path);
    // Checked here rather than left to the move: dependencies copied under a
    // root outside AddonPackages would never load either.
    if !in_place && is_in_addon_packages(var_path) && !is_in_addon_packages(&package_dest) {
        return Err(OUT_OF_ADDON_PACKAGES.to_string());
    }
    let creator_dir = package_dest
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "Bad destination folder.".to_string())?;
    // Reuse an existing `Deps/` whatever its casing, like the creator folder.
    let deps_dir = existing_creator_dir(&creator_dir, DEPS_DIR_NAME);
    Ok(CollectDestination {
        package_dest,
        creator_dir,
        deps_dir,
        in_place,
    })
}

// ----------------------------------------------------------------------------
// Scan
// ----------------------------------------------------------------------------

/// Reads the package's dependencies and resolves each against the library and
/// the search folders. Read-only: nothing on disk changes.
pub(crate) fn run_collect_deps_scan(
    var_path: &Path,
    search_dirs: &[String],
    library_dirs: &[String],
    root_dir: &str,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(f64, String),
) -> Result<CollectDepsScanResponse, String> {
    let package_id = var_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    if !var_path.is_file() {
        return Err(format!(
            "That package is no longer on disk: {}",
            var_path.display()
        ));
    }

    let (search_roots, missing_search) = usable_roots(search_dirs);
    if search_roots.is_empty() {
        return Err(if missing_search.is_empty() {
            "Add a folder to search first.".to_string()
        } else {
            format!(
                "None of the search folders exist: {}",
                missing_search.join(", ")
            )
        });
    }

    progress(0.02, format!("Reading {package_id}"));
    // The full tree VAM recorded at build time, not just the direct entries:
    // a nested dependency missing from the library is just as missing.
    let (_, declared) = read_var_dependency_sets(var_path)
        .map_err(|e| format!("Couldn't read {package_id}: {e}"))?;

    let mut response = CollectDepsScanResponse {
        package_id: package_id.clone(),
        var_path: var_path.display().to_string(),
        search_dirs: search_roots
            .iter()
            .map(|p| p.display().to_string())
            .collect(),
        ..Default::default()
    };
    for dir in &missing_search {
        response
            .notes
            .push(format!("Folder not found, so it was skipped: {dir}"));
    }

    if root_dir.trim().is_empty() {
        response.destination_error =
            Some("Set your VAR library (AddonPackages root) folder in Settings first.".to_string());
    } else {
        match plan_destination(var_path, root_dir) {
            Ok(dest) => {
                if !is_in_addon_packages(&dest.creator_dir) {
                    response.destination_warning = Some(format!(
                        "{} is not inside an AddonPackages folder, so VAM won't load what is \
                         copied there. Check the VAR library folder in Settings.",
                        dest.creator_dir.display(),
                    ));
                }
                response.creator_dir = Some(dest.creator_dir.display().to_string());
                response.deps_dir = Some(dest.deps_dir.display().to_string());
                response.package_in_place = dest.in_place;
            }
            Err(err) => response.destination_error = Some(err),
        }
    }

    // The library decides what VAM already has; filenames only, no archives.
    let (library_roots, _) = usable_roots(library_dirs);
    response.library_used = !library_roots.is_empty();
    progress(0.1, "Indexing your VAR library".to_string());
    let mut ignored = 0usize;
    let Some(library_files) = walk_vars(&library_roots, false, cancel, &mut ignored, &mut |n| {
        progress(0.1, format!("Indexing your VAR library ({n} VARs)"))
    }) else {
        response.was_cancelled = true;
        return Ok(response);
    };
    response.library_var_count = library_files.len();
    let library = index_library(library_files);

    progress(0.4, "Searching your folders".to_string());
    let mut skipped_managed = 0usize;
    let Some(search_files) = walk_vars(
        &search_roots,
        true,
        cancel,
        &mut skipped_managed,
        &mut |n| progress(0.4, format!("Searching your folders ({n} VARs)")),
    ) else {
        response.was_cancelled = true;
        return Ok(response);
    };
    if skipped_managed > 0 {
        response.notes.push(format!(
            "Skipped {skipped_managed} backup folder(s) made by this app ({}).",
            APP_MANAGED_DIRS.join(", "),
        ));
    }
    response.search_var_count = search_files.len();
    let search = index_search(search_files, var_path);

    progress(0.8, "Matching dependencies".to_string());
    let Some(items) = resolve_dependencies(&package_id, &declared, &library, &search, cancel)
    else {
        response.was_cancelled = true;
        return Ok(response);
    };
    response.items = items;
    progress(0.99, "Finalizing".to_string());
    Ok(response)
}

// ----------------------------------------------------------------------------
// Copy
// ----------------------------------------------------------------------------

enum CopyError {
    Cancelled,
    Io(std::io::Error),
}

/// Streams `src` into a fresh `dest`, checking `cancel` between chunks. Keeps
/// the original's modified time, so date sorts still see it as the same download.
fn copy_with_cancel(
    src: &Path,
    dest: &Path,
    modified: Option<SystemTime>,
    cancel: &AtomicBool,
    on_bytes: &mut dyn FnMut(u64),
) -> Result<u64, CopyError> {
    let mut reader = fs::File::open(src).map_err(CopyError::Io)?;
    let mut writer = fs::File::create(dest).map_err(CopyError::Io)?;
    let mut buf = vec![0u8; COPY_CHUNK];
    let mut total = 0u64;
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err(CopyError::Cancelled);
        }
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(CopyError::Io(err)),
        };
        writer.write_all(&buf[..n]).map_err(CopyError::Io)?;
        total += n as u64;
        on_bytes(n as u64);
    }
    writer.flush().map_err(CopyError::Io)?;
    if let Some(modified) = modified {
        let _ = writer.set_modified(modified);
    }
    Ok(total)
}

fn outcome(
    mut result: CollectDepCopyResult,
    status: &str,
    detail: impl Into<String>,
) -> CollectDepCopyResult {
    result.status = status.to_string();
    result.detail = detail.into();
    result
}

/// Copies one dependency `.var` into `deps_dir`, never overwriting anything.
///
/// The bytes go to `<name>.part`, which is renamed into place only once
/// complete, so a cancelled or failed copy can never leave a truncated `.var`
/// where VAM would try to load it (VAM only picks up `*.var`). `on_bytes` gets
/// each chunk's size as it lands.
pub(crate) fn copy_dep_file(
    src: &Path,
    deps_dir: &Path,
    cancel: &AtomicBool,
    on_bytes: &mut dyn FnMut(u64),
) -> CollectDepCopyResult {
    let mut result = CollectDepCopyResult {
        source_path: src.display().to_string(),
        ..Default::default()
    };

    if !is_var_path(src) {
        return outcome(result, "failed", "not a .var file");
    }
    let src_meta = match fs::metadata(src) {
        Ok(meta) if meta.is_file() => meta,
        Ok(_) => {
            return outcome(result, "failed", "not a file");
        }
        Err(err) => {
            return outcome(result, "failed", format!("couldn't read it: {err}"));
        }
    };
    let Some(file_name) = src.file_name() else {
        return outcome(result, "failed", "bad file name");
    };
    let dest = deps_dir.join(file_name);
    result.dest_path = dest.display().to_string();

    if let Ok(existing) = fs::metadata(&dest) {
        if existing.len() == src_meta.len() {
            return outcome(result, "exists", "already in deps");
        }
        return outcome(
            result,
            "skipped",
            "a different file with this name is already in deps — left it alone",
        );
    }
    // VAM (Mono) is not long-path aware, and a copy it can't open is no copy.
    if wide_len(&dest) >= MAX_PATH_UTF16 || wide_len(deps_dir) >= MAX_DIR_UTF16 {
        return outcome(
            result,
            "skipped",
            "the destination path would be too long for VAM to load",
        );
    }
    if let Err(err) = fs::create_dir_all(deps_dir) {
        return outcome(
            result,
            "failed",
            format!("couldn't create {}: {err}", deps_dir.display()),
        );
    }

    let mut part_name = file_name.to_os_string();
    part_name.push(".part");
    let part = deps_dir.join(part_name);
    let bytes = match copy_with_cancel(src, &part, src_meta.modified().ok(), cancel, on_bytes) {
        Ok(bytes) => bytes,
        Err(CopyError::Cancelled) => {
            let _ = fs::remove_file(&part);
            return outcome(result, "cancelled", "cancelled — nothing was left behind");
        }
        Err(CopyError::Io(err)) => {
            let _ = fs::remove_file(&part);
            return outcome(result, "failed", format!("couldn't copy: {err}"));
        }
    };

    // fs::rename is MoveFileExW with REPLACE_EXISTING, so check again: a long
    // copy is plenty of time for something else to land on the name.
    if dest.exists() {
        let _ = fs::remove_file(&part);
        return outcome(
            result,
            "skipped",
            "something appeared at the destination during the copy — left it alone",
        );
    }
    if let Err(err) = fs::rename(&part, &dest) {
        let _ = fs::remove_file(&part);
        return outcome(result, "failed", format!("couldn't finish the copy: {err}"));
    }

    result.status = "copied".to_string();
    result.detail = format!("copied to {}", dest.display());
    result.bytes = bytes;
    result
}

/// Moves the package into its creator folder (unless it is already there),
/// then copies each dependency into `deps/` beside it.
///
/// A package that can't move (in use, or a file already at the destination)
/// does not stop the copy: the dependencies load from `deps/` either way, and
/// the reason comes back in `var_note`. Only problems that would make the copy
/// itself wrong — no library folder, a root outside AddonPackages — fail the
/// whole run, before anything is touched.
pub(crate) fn run_collect_deps_copy(
    var_path: &Path,
    root_dir: &str,
    dep_paths: &[String],
    ops: &dyn FileOps,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(f64, String),
) -> Result<CollectDepsCopyResponse, String> {
    let dest = plan_destination(var_path, root_dir)?;

    // Two rows can resolve to one file (`Pkg.3` and `Pkg.latest` when only one
    // version exists), and copies are named by file name, so dedupe on that.
    let mut seen_names: HashSet<String> = HashSet::new();
    let sources: Vec<PathBuf> = dep_paths
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .filter(|p| {
            seen_names.insert(
                p.file_name()
                    .map(|n| n.to_string_lossy().to_ascii_lowercase())
                    .unwrap_or_default(),
            )
        })
        .collect();
    if sources.is_empty() {
        return Err("Nothing to copy.".to_string());
    }

    let mut response = CollectDepsCopyResponse {
        var_path: var_path.display().to_string(),
        creator_dir: dest.creator_dir.display().to_string(),
        deps_dir: dest.deps_dir.display().to_string(),
        ..Default::default()
    };

    // Cancelled before anything happened: leave the package where it was too.
    if cancel.load(Ordering::SeqCst) {
        response.was_cancelled = true;
    } else if !dest.in_place {
        let name = file_name_of(var_path).unwrap_or_default();
        progress(0.0, format!("Moving {name}"));
        match move_package_with_sidecars(ops, var_path, &dest.package_dest) {
            Ok(notes) => {
                crate::packages::prune_after_removal(var_path, &[PathBuf::from(root_dir.trim())]);
                response.var_moved = true;
                response.var_path = dest.package_dest.display().to_string();
                if !notes.is_empty() {
                    response.var_note = Some(notes.join("; "));
                }
            }
            Err(err) => response.var_note = Some(err),
        }
    }

    let total_bytes = sources
        .iter()
        .map(|p| fs::metadata(p).map(|m| m.len()).unwrap_or(0))
        .sum::<u64>()
        .max(1);
    let mut done_bytes = 0u64;
    let count = sources.len();
    for (index, src) in sources.iter().enumerate() {
        if cancel.load(Ordering::SeqCst) {
            response.was_cancelled = true;
        }
        if response.was_cancelled {
            response.results.push(CollectDepCopyResult {
                source_path: src.display().to_string(),
                status: "cancelled".to_string(),
                detail: "not copied — cancelled".to_string(),
                ..Default::default()
            });
            continue;
        }

        let name = file_name_of(src).unwrap_or_default();
        progress(
            done_bytes as f64 / total_bytes as f64,
            format!("Copying {name} ({}/{count})", index + 1),
        );
        let result = {
            let mut on_bytes = |n: u64| {
                done_bytes += n;
                progress(
                    done_bytes as f64 / total_bytes as f64,
                    format!(
                        "Copying {name} ({}/{count}) — {} of {}",
                        index + 1,
                        format_bytes(done_bytes),
                        format_bytes(total_bytes),
                    ),
                );
            };
            copy_dep_file(src, &dest.deps_dir, cancel, &mut on_bytes)
        };
        match result.status.as_str() {
            "copied" => {
                response.copied += 1;
                response.bytes_copied += result.bytes;
            }
            "failed" => response.failed += 1,
            "cancelled" => response.was_cancelled = true,
            _ => {}
        }
        response.results.push(result);
    }

    progress(1.0, "Finished".to_string());
    Ok(response)
}

// ----------------------------------------------------------------------------
// Tauri commands + task wiring
// ----------------------------------------------------------------------------

fn finish_scan_task(
    tasks: &TaskMap,
    task_id: u64,
    result: Result<CollectDepsScanResponse, String>,
) {
    let Ok(mut guard) = tasks.lock() else { return };
    let Some(task) = guard.get_mut(&task_id) else {
        return;
    };
    task.done = true;
    task.progress = 1.0;
    match result {
        Ok(payload) => {
            let found = payload.items.iter().filter(|i| i.status == "found").count();
            task.phase = if payload.was_cancelled {
                "collect_deps_scan_cancelled".to_string()
            } else {
                "collect_deps_scan_complete".to_string()
            };
            task.message = if payload.was_cancelled {
                "Cancelled".to_string()
            } else {
                format!(
                    "{found} dependenc{} to copy",
                    if found == 1 { "y" } else { "ies" }
                )
            };
            task.error = None;
            task.collect_deps_scan_result = Some(payload);
        }
        Err(err) => {
            task.phase = "collect_deps_scan_failed".to_string();
            task.message = "Failed".to_string();
            task.error = Some(err);
        }
    }
}

fn finish_copy_task(
    tasks: &TaskMap,
    task_id: u64,
    result: Result<CollectDepsCopyResponse, String>,
) {
    let Ok(mut guard) = tasks.lock() else { return };
    let Some(task) = guard.get_mut(&task_id) else {
        return;
    };
    task.done = true;
    task.progress = 1.0;
    match result {
        Ok(payload) => {
            task.phase = if payload.was_cancelled {
                "collect_deps_copy_cancelled".to_string()
            } else {
                "collect_deps_copy_complete".to_string()
            };
            task.message = format!("{} copied", payload.copied);
            task.error = None;
            task.collect_deps_copy_result = Some(payload);
        }
        Err(err) => {
            task.phase = "collect_deps_copy_failed".to_string();
            task.message = "Failed".to_string();
            task.error = Some(err);
        }
    }
}

/// Finds `var_path`'s dependencies in `search_dirs`. `library_dirs` are the
/// Settings VAR library folders (what VAM already has); `root_dir` is the one a
/// copy files into, sent so the modal can show the destination up front.
#[tauri::command]
pub(crate) fn start_collect_deps_scan_task(
    var_path: String,
    search_dirs: Vec<String>,
    library_dirs: Vec<String>,
    root_dir: Option<String>,
    state: State<'_, AppState>,
) -> Result<TaskHandle, String> {
    let (task_id, tasks, cancel) =
        begin_task(&state, "collect_deps_scan_starting", "Reading dependencies")?;
    thread::spawn(move || {
        let mut progress = |fraction: f64, message: String| {
            set_task_progress(
                &tasks,
                task_id,
                "collect_deps_scan_running",
                fraction,
                message,
            );
        };
        let result = run_collect_deps_scan(
            Path::new(var_path.trim()),
            &search_dirs,
            &library_dirs,
            root_dir.as_deref().unwrap_or(""),
            &cancel,
            &mut progress,
        );
        finish_scan_task(&tasks, task_id, result);
    });
    Ok(TaskHandle { id: task_id })
}

/// Moves `var_path` into `<root_dir>/<Creator>/` and copies `dep_paths` into
/// `<root_dir>/<Creator>/deps/`.
#[tauri::command]
pub(crate) fn start_collect_deps_copy_task(
    var_path: String,
    root_dir: String,
    dep_paths: Vec<String>,
    state: State<'_, AppState>,
) -> Result<TaskHandle, String> {
    let (task_id, tasks, cancel) =
        begin_task(&state, "collect_deps_copy_starting", "Copying dependencies")?;
    let folder_cache: Arc<Mutex<Option<VarPackagesFolderCache>>> =
        Arc::clone(&state.var_packages_folder_cache);
    let generation: Arc<AtomicU64> = Arc::clone(&state.var_packages_cache_generation);

    thread::spawn(move || {
        let mut progress = |fraction: f64, message: String| {
            set_task_progress(
                &tasks,
                task_id,
                "collect_deps_copy_running",
                fraction,
                message,
            );
        };
        let result = run_collect_deps_copy(
            Path::new(var_path.trim()),
            &root_dir,
            &dep_paths,
            &RealFileOps,
            &cancel,
            &mut progress,
        );

        // Every exit path invalidates, as the plan apply does: the package may
        // have moved and new files sit in the library either way, so a cached
        // listing would show the old path. (Inlined because AppState can't
        // cross the thread boundary — the worker holds the two Arcs directly.)
        generation.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut guard) = folder_cache.lock() {
            *guard = None;
        }

        finish_copy_task(&tasks, task_id, result);
    });
    Ok(TaskHandle { id: task_id })
}
