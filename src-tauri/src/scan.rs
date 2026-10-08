use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
    sync::{mpsc, Arc, Mutex},
    thread,
};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use zip::ZipArchive;

use crate::{
    db::{self, Db},
    execute::binary_cache_sibling_descriptor,
    models::{
        CachedScan, DuplicateGroup, PreparedPackage, ResourceRef, ScanDuplicateGroup,
        ScanInfo, ScanResourceRef, ScanResponse, ScanSummary, ScannedData,
        KEEP_ALL_VALUE, META_PATH,
    },
    naming,
    utils::{
        collect_var_files_with_targets, normalize_zip_path, read_json_bytes, scan_cache_key_multi,
    },
};

struct ScanWorkerEvent {
    file_name: String,
    package: Result<PreparedPackage>,
}

// Nearly every outcome is Loaded, so boxing it (clippy's suggestion) would add
// an allocation per package to save space only on the rare Skipped.
#[allow(clippy::large_enum_variant)]
enum ScanOutcome {
    Loaded(PreparedPackage),
    Skipped { file_name: String, reason: String },
}

pub(crate) fn is_skippable_meta_error(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        let text = cause.to_string();
        text.contains(META_PATH)
            && (text.contains("failed to parse JSON")
                || text.contains("missing meta.json")
                || text.contains("not a recognized text JSON file"))
    })
}

fn resolve_relative_zip_path(base_file_path: &str, referenced_path: &str) -> Option<String> {
    let referenced_path = referenced_path.trim();
    if referenced_path.is_empty()
        || referenced_path.contains(":/")
        || referenced_path.starts_with("SELF:/")
    {
        return None;
    }

    let referenced_path = referenced_path.replace('\\', "/");
    let base_dir = Path::new(base_file_path)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let joined = if referenced_path.starts_with('/') {
        PathBuf::from(referenced_path.trim_start_matches('/'))
    } else {
        base_dir.join(referenced_path)
    };

    let mut normalized = Vec::new();
    for component in joined.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => {}
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part.to_string_lossy().to_string()),
        }
    }

    if normalized.is_empty() {
        None
    } else {
        Some(normalized.join("/"))
    }
}

/// True for `.vaj` `customTexture_*` values that VAM uses as "no texture
/// here" placeholders rather than as real file paths. Common forms in the
/// wild: bare `NULL`, `./NULL`, `.\NULL`, with surrounding whitespace, and
/// in any case. Empty/whitespace-only strings get the same treatment —
/// they're not real refs either. Dropping these at collect-time means the
/// bundle index, the effective_size rollup, and the incomplete-ref check
/// all stay free of phantom siblings that no .var ever contains.
fn is_vaj_null_texture_value(raw: &str) -> bool {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return true;
    }
    let basename = trimmed
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("");
    basename.eq_ignore_ascii_case("NULL")
}

fn collect_vaj_texture_paths(value: &Value, paths: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if key.starts_with("customTexture_") {
                    if let Some(path) = child.as_str() {
                        if !is_vaj_null_texture_value(path) {
                            paths.push(path.to_string());
                        }
                    }
                }
                collect_vaj_texture_paths(child, paths);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_vaj_texture_paths(item, paths);
            }
        }
        _ => {}
    }
}

pub(crate) fn extract_vaj_support_paths(vaj_path: &str, raw: &[u8]) -> BTreeSet<String> {
    let Ok(parsed) = read_json_bytes(raw, vaj_path) else {
        return BTreeSet::new();
    };

    let mut referenced_paths = Vec::new();
    collect_vaj_texture_paths(&parsed, &mut referenced_paths);
    referenced_paths
        .into_iter()
        .filter_map(|path| resolve_relative_zip_path(vaj_path, &path))
        .collect()
}

/// Extracts `SELF:/<path>` texture references from a `.vaj`, returning the
/// inner paths with the `SELF:/` prefix stripped. Used by internalize so a
/// source `.vaj` whose textures sit elsewhere in *its own* package (referenced
/// as `SELF:/`) still pulls those textures into the copied bundle. In the
/// target var the same `SELF:/` strings then resolve to the freshly-copied
/// files. Sibling to `extract_vaj_support_paths`, which deliberately drops
/// `SELF:/` because dedupe never needs to follow them.
pub(crate) fn extract_vaj_self_paths(vaj_path: &str, raw: &[u8]) -> BTreeSet<String> {
    let Ok(parsed) = read_json_bytes(raw, vaj_path) else {
        return BTreeSet::new();
    };

    let mut referenced_paths = Vec::new();
    collect_vaj_texture_paths(&parsed, &mut referenced_paths);
    referenced_paths
        .into_iter()
        .filter_map(|path| {
            let trimmed = path.trim();
            let stripped = trimmed.strip_prefix("SELF:/")?;
            if stripped.is_empty() {
                return None;
            }
            let normalized = stripped.replace('\\', "/");
            let trimmed = normalized.trim_start_matches('/');
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        })
        .collect()
}

fn apply_effective_resource_sizes(
    resource_refs: &mut [ResourceRef],
    support_paths: &BTreeMap<String, BTreeSet<String>>,
) {
    let size_map = resource_refs
        .iter()
        .map(|resource| (resource.internal_path.clone(), resource.size))
        .collect::<HashMap<_, _>>();

    for resource in resource_refs.iter_mut() {
        let support_bytes = support_paths
            .get(&resource.internal_path)
            .into_iter()
            .flatten()
            .map(|path| size_map.get(path).copied().unwrap_or(0))
            .sum::<u64>();
        resource.effective_size = resource.size + support_bytes;
    }
}

pub(crate) fn same_stem_texture_paths(path: &str) -> BTreeSet<String> {
    let source = Path::new(path);
    let Some(extension) = source.extension().and_then(|ext| ext.to_str()) else {
        return BTreeSet::new();
    };
    if !extension.eq_ignore_ascii_case("vam") {
        return BTreeSet::new();
    }

    let Some(stem) = source.file_stem().and_then(|stem| stem.to_str()) else {
        return BTreeSet::new();
    };
    let parent = source.parent();
    ["png", "jpg", "jpeg"]
        .into_iter()
        .map(|new_ext| {
            let file_name = format!("{stem}.{new_ext}");
            match parent {
                Some(parent) if !parent.as_os_str().is_empty() => parent.join(file_name),
                _ => PathBuf::from(file_name),
            }
            .to_string_lossy()
            .replace('\\', "/")
        })
        .collect()
}

/// Combine the primary root with any additional scan roots into a single list.
pub(crate) fn scan_roots(input_dir: &Path, additional_dirs: &[PathBuf]) -> Vec<PathBuf> {
    std::iter::once(input_dir.to_path_buf())
        .chain(additional_dirs.iter().cloned())
        .collect()
}

fn display_roots(roots: &[PathBuf]) -> String {
    roots
        .iter()
        .map(|root| root.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

pub(crate) fn scan_directory_with_target_with_progress<F>(
    input_dir: &Path,
    additional_dirs: &[PathBuf],
    target_var_path: Option<&Path>,
    on_progress: F,
) -> Result<ScannedData>
where
    F: FnMut(f64, String),
{
    scan_directory_with_target_with_progress_db(
        input_dir,
        additional_dirs,
        target_var_path,
        None,
        false,
        on_progress,
    )
}

pub(crate) fn scan_directory_with_target_with_progress_db<F>(
    input_dir: &Path,
    additional_dirs: &[PathBuf],
    target_var_path: Option<&Path>,
    db: Option<&Db>,
    index_only: bool,
    mut on_progress: F,
) -> Result<ScannedData>
where
    F: FnMut(f64, String),
{
    let roots = scan_roots(input_dir, additional_dirs);
    let mut var_files = collect_var_files_with_targets(&roots, target_var_path)?;

    if var_files.is_empty() {
        bail!("No .var files were found in: {}", display_roots(&roots));
    }

    // Captured from the full on-disk list — the cache key reflects what's on
    // disk, not what survives the blocked-creator filter, so cache entries
    // stay valid across re-runs and only invalidate when files actually
    // change. Trade-off: blocking a creator does NOT invalidate the cache;
    // the user must trigger a fresh scan to apply the new filter.
    let file_fingerprints = var_files
        .iter()
        .map(|entry| entry.fingerprint.clone())
        .collect::<Vec<_>>();

    // Resolved up front so the blocked-creator filter below can keep the
    // target VAR even when its creator is blocked — the user picked it
    // explicitly and the rest of the pipeline relies on it being present.
    let target_package_id = target_var_path
        .map(|path| {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .ok_or_else(|| anyhow!("invalid target VAR file name: {}", path.display()))
                .map(str::to_string)
        })
        .transpose()?;

    // Drop blocked-creator VARs before workers spawn so we don't open their
    // ZIPs just to throw them away. Mirrors the `flag != BLOCKED` filter the
    // DB-find query already applies (db.rs `find_crc_matches_bulk`).
    let mut pre_skipped_blocked: Vec<String> = Vec::new();
    if let Some(db) = db {
        let blocked = db
            .conn
            .lock()
            .ok()
            .and_then(|conn| db::get_blocked_creator_names(&conn).ok())
            .unwrap_or_default();
        if !blocked.is_empty() {
            var_files.retain(|entry| {
                let Some(stem) = entry.path.file_stem().and_then(|s| s.to_str()) else {
                    return true;
                };
                if Some(stem) == target_package_id.as_deref() {
                    return true;
                }
                let Some(creator) = naming::creator_from_package_id(stem) else {
                    return true;
                };
                if blocked.contains(creator) {
                    pre_skipped_blocked.push(format!("{stem} (creator blocked)"));
                    false
                } else {
                    true
                }
            });
        }
    }

    if var_files.is_empty() {
        if !pre_skipped_blocked.is_empty() {
            bail!(
                "All .var files in {} are from blocked creators",
                display_roots(&roots)
            );
        }
        bail!("No .var files were found in: {}", display_roots(&roots));
    }

    let total = var_files.len() as f64;
    let mut packages: BTreeMap<String, PreparedPackage> = BTreeMap::new();

    const SCAN_LIST_PROGRESS: f64 = 0.02;
    const SCAN_PACKAGE_PROGRESS_START: f64 = 0.04;
    const SCAN_PACKAGE_PROGRESS_END: f64 = 0.34;
    const SCAN_GROUP_PROGRESS: f64 = 0.96;

    on_progress(
        SCAN_LIST_PROGRESS,
        format!("Listing VAR packages ({})", total as usize),
    );

    // Rayon work-steals across the global thread pool, eliminating the
    // Arc<Mutex<VecDeque>> contention point that every worker previously hit
    // on each item. A driver thread runs the parallel iterator and pushes
    // ScanWorkerEvents through a bounded sync_channel; the main thread keeps
    // its rx.recv loop for ordered progress emission and outcome collection.
    let (tx, rx) = mpsc::sync_channel::<ScanWorkerEvent>(64);
    let driver = thread::spawn(move || {
        use rayon::prelude::*;
        var_files.into_par_iter().for_each_with(tx, |tx, entry| {
            let file_name = entry
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("unknown.var")
                .to_string();
            let package = scan_package(&entry.path);
            let _ = tx.send(ScanWorkerEvent { file_name, package });
        });
    });

    let mut outcomes = Vec::with_capacity(total as usize);
    let mut fatal_error = None;
    for index in 0..(total as usize) {
        let ScanWorkerEvent { file_name, package } = rx
            .recv()
            .map_err(|_| anyhow!("scan workers terminated unexpectedly"))?;

        match package {
            Ok(package) => {
                outcomes.push(ScanOutcome::Loaded(package));
            }
            Err(err) if is_skippable_meta_error(&err) => {
                outcomes.push(ScanOutcome::Skipped {
                    file_name: file_name.clone(),
                    reason: err.to_string(),
                });
            }
            Err(err) => {
                fatal_error = Some(err);
            }
        }

        let progress = SCAN_PACKAGE_PROGRESS_START
            + (((index + 1) as f64 / total)
                * (SCAN_PACKAGE_PROGRESS_END - SCAN_PACKAGE_PROGRESS_START));
        on_progress(
            progress,
            format!(
                "Scanning package {}/{}: {file_name}",
                index + 1,
                total as usize
            ),
        );
    }

    let _ = driver.join();

    if let Some(err) = fatal_error {
        return Err(err);
    }
    // Resolve the user-designated target file once. When a target_var_path
    // is provided (single-var / Find Duplicates mode), the merge MUST keep
    // that exact file as the package_id's entry — using a non-target copy
    // would either operate on stale bytes or, worse, match the target
    // against itself and mark every file for removal. Same-package_id
    // copies anywhere else in the input folder (`backup/`, `changed/`, a
    // sibling install, a stray re-download) are dropped from the ref pool
    // for the same reason — they're not usable as references because they
    // share content with the target.
    let canonical_target = target_var_path.and_then(|p| fs::canonicalize(p).ok());
    let mut warnings = pre_skipped_blocked;
    for outcome in outcomes {
        match outcome {
            ScanOutcome::Loaded(package) => {
                let package_id = package.package_id.clone();
                if let Some(existing) = packages.get(&package_id) {
                    let canon_new = fs::canonicalize(&package.file_path).ok();
                    let canon_existing = fs::canonicalize(&existing.file_path).ok();
                    let new_is_target = canonical_target.is_some()
                        && canon_new.as_ref() == canonical_target.as_ref();
                    let existing_is_target = canonical_target.is_some()
                        && canon_existing.as_ref() == canonical_target.as_ref();
                    let (kept_path, dropped_path) =
                        if new_is_target && !existing_is_target {
                            let dropped = existing.file_path.display().to_string();
                            let kept = package.file_path.display().to_string();
                            packages.insert(package_id.clone(), package);
                            (kept, dropped)
                        } else {
                            (
                                existing.file_path.display().to_string(),
                                package.file_path.display().to_string(),
                            )
                        };
                    let msg = format!(
                        "Ignoring duplicate package id {package_id} ({dropped_path}); keeping {kept_path}"
                    );
                    eprintln!("{msg}");
                    if canonical_target.is_some() {
                        warnings.push(msg);
                    }
                    continue;
                }
                packages.insert(package_id, package);
            }
            ScanOutcome::Skipped { file_name, reason } => {
                warnings.push(format!("{file_name} (invalid meta.json)"));
                eprintln!("Skipped {file_name}: {reason}");
            }
        }
    }

    if packages.is_empty() {
        bail!("No valid .var files were found in: {}", input_dir.display());
    }

    if let Some(target_package_id) = target_package_id.as_deref() {
        if !packages.contains_key(target_package_id) {
            bail!("Target package was not found in scan result: {target_package_id}");
        }
    }

    if index_only {
        on_progress(
            SCAN_GROUP_PROGRESS,
            "Skipping duplicate analysis (index-only mode)".to_string(),
        );

        let mut scanned = ScannedData {
            files: file_fingerprints,
            packages,
            duplicate_groups: Vec::new(),
            warnings,
            info: ScanInfo::default(),
        };

        if let Some(db) = db {
            match persist_scan_to_db(
                db,
                &scanned,
                target_package_id.as_deref(),
                |progress, message| {
                    on_progress(progress, message);
                },
            ) {
                Ok(info) => {
                    scanned.info = info;
                }
                Err(err) => {
                    eprintln!("warning: failed to persist scan to database: {err:?}");
                    scanned
                        .warnings
                        .push(format!("Database save failed: {err}"));
                }
            }
        }

        on_progress(1.0, "Scan completed".to_string());
        return Ok(scanned);
    }

    on_progress(
        SCAN_PACKAGE_PROGRESS_END,
        "Analyzing duplicate size candidates".to_string(),
    );

    // Build the size_index in parallel. Each rayon worker accumulates a
    // local HashMap over its slice of packages; the reduce step then merges
    // them. On libraries with hundreds of packages × thousands of resources
    // per package this is the dominant CPU phase between scan and grouping —
    // serial built an N-million-entry map on one core.
    let size_index: HashMap<u64, Vec<(String, usize)>> = {
        use rayon::prelude::*;
        let package_vec: Vec<&PreparedPackage> = packages.values().collect();
        package_vec
            .par_iter()
            .map(|package| {
                let mut local: HashMap<u64, Vec<(String, usize)>> = HashMap::new();
                for (resource_index_in_package, resource) in
                    package.resource_refs.iter().enumerate()
                {
                    local
                        .entry(resource.size)
                        .or_default()
                        .push((package.package_id.clone(), resource_index_in_package));
                }
                local
            })
            .reduce(HashMap::new, |mut acc, m| {
                if acc.is_empty() {
                    return m;
                }
                for (k, mut v) in m {
                    acc.entry(k).or_default().append(&mut v);
                }
                acc
            })
    };
    // size_index needs to be mutable for the retain() call in single-VAR
    // mode below.
    let mut size_index = size_index;

    if let Some(target_package_id) = target_package_id.as_deref() {
        let target_sizes = packages
            .get(target_package_id)
            .map(|package| {
                package
                    .resource_refs
                    .iter()
                    .map(|resource| resource.size)
                    .collect::<BTreeSet<_>>()
            })
            .unwrap_or_default();
        size_index.retain(|size, refs| {
            target_sizes.contains(size)
                && refs
                    .iter()
                    .any(|(package_id, _)| package_id == target_package_id)
                && refs.len() > 1
        });
    }

    on_progress(SCAN_GROUP_PROGRESS, "Building duplicate groups".to_string());

    // Group by (size, crc32). Within a same-size bucket, CRC32 collisions on
    // real VAR resources are negligible — expected false-pair count is N²/2 ×
    // 2⁻³², well under 1 for any realistic library. CRCs come from each ZIP's
    // central directory and are populated by `scan_package`, so this is free.
    let mut resource_index: HashMap<(u64, u32), Vec<(String, usize)>> = HashMap::new();
    for refs in size_index.into_values() {
        if refs.len() <= 1 {
            continue;
        }
        for (package_id, resource_index_in_package) in refs {
            if let Some(resource) = packages
                .get(&package_id)
                .and_then(|package| package.resource_refs.get(resource_index_in_package))
            {
                let Some(crc) = resource.crc32 else {
                    continue;
                };
                resource_index
                    .entry((resource.size, crc))
                    .or_default()
                    .push((package_id.clone(), resource_index_in_package));
            }
        }
    }

    // Parallel filter_map over the (size, crc) buckets. Each bucket's work
    // — collecting refs, sorting them, computing reclaim totals — is
    // independent. `packages` is a shared &BTreeMap read concurrently by
    // workers (Sync). Output order will differ from serial; the sort_by at
    // the end of this phase canonicalizes it.
    let resource_index_vec: Vec<_> = resource_index.into_iter().collect();
    let mut duplicate_groups: Vec<DuplicateGroup> = {
        use rayon::prelude::*;
        resource_index_vec
            .into_par_iter()
            .filter_map(|((size, crc), refs)| {
                if refs.len() <= 1 {
                    return None;
                }
                let mut refs = refs
                    .into_iter()
                    .filter_map(|(package_id, resource_index_in_package)| {
                        packages
                            .get(&package_id)
                            .and_then(|package| {
                                package.resource_refs.get(resource_index_in_package)
                            })
                            .cloned()
                    })
                    .collect::<Vec<_>>();
                refs.sort_by(|a, b| {
                    a.package_id
                        .cmp(&b.package_id)
                        .then_with(|| a.internal_path.cmp(&b.internal_path))
                });
                let package_ids = refs
                    .iter()
                    .map(|item| item.package_id.clone())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>();
                let removable_bytes = refs
                    .iter()
                    .map(|item| item.effective_size)
                    .sum::<u64>()
                    .saturating_sub(
                        refs.iter()
                            .map(|item| item.effective_size)
                            .min()
                            .unwrap_or(0),
                    );
                Some(DuplicateGroup {
                    key: format!("{:08x}:{}", crc, size),
                    refs,
                    removable_bytes,
                    package_ids,
                })
            })
            .collect()
    };

    // Filter out unactionable `.vab` / `.vmb` matches. These Unity binary
    // caches are sibling-loaded by VAM (found by stem next to their `.vam` /
    // `.vmi` at runtime, never JSON-referenced), so relocating one cross-
    // package is only safe when the *metadata sibling* is also being
    // relocated concurrently. Near-empty caches CRC32-collide surprisingly
    // often — without this filter the UI surfaces dozens of bogus
    // "duplicates" between unrelated hairs / morphs that the user can't act
    // on (and which the runtime guard in `prepare_package_changes` would
    // refuse to apply anyway). Drop any cache-ref whose `.vam`/`.vmi`
    // sibling in the same package is not itself participating in a
    // duplicate group; drop the whole group if that leaves <2 refs.
    let descriptor_in_any_group: BTreeSet<(String, String)> = duplicate_groups
        .iter()
        .flat_map(|group| group.refs.iter())
        .filter(|item| {
            Path::new(&item.internal_path)
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| {
                    let lower = ext.to_ascii_lowercase();
                    lower == "vam" || lower == "vmi"
                })
                .unwrap_or(false)
        })
        .map(|item| (item.package_id.clone(), item.internal_path.clone()))
        .collect();
    duplicate_groups.retain_mut(|group| {
        let group_is_cache_kind = group
            .refs
            .iter()
            .any(|item| binary_cache_sibling_descriptor(&item.internal_path).is_some());
        if !group_is_cache_kind {
            return true;
        }
        group.refs.retain(|item| {
            let Some(sibling_path) = binary_cache_sibling_descriptor(&item.internal_path)
            else {
                return true;
            };
            descriptor_in_any_group.contains(&(item.package_id.clone(), sibling_path))
        });
        if group.refs.len() < 2 {
            return false;
        }
        group.package_ids = group
            .refs
            .iter()
            .map(|item| item.package_id.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        group.removable_bytes = group
            .refs
            .iter()
            .map(|item| item.effective_size)
            .sum::<u64>()
            .saturating_sub(
                group
                    .refs
                    .iter()
                    .map(|item| item.effective_size)
                    .min()
                    .unwrap_or(0),
            );
        true
    });

    // Drop refs whose package is missing the ENTIRE bundle for this parent —
    // e.g. a `.vam` whose `.vaj`, `.vab`, and all referenced textures are
    // absent from the ZIP. CRC matches on the `.vam` alone don't make this a
    // safe redirect target: the bundle cascade would rewrite SELF:/sibling
    // refs to a package that can't fulfill them, producing runtime texture-
    // load errors. Refs with only *some* missing siblings stay in the group
    // and are flagged by the UI via `bundle_missing_index` — the user sees
    // them with a warning but can't pick them as keep. "Required" counts use
    // the same heuristic-exclusion rule as `compute_missing_siblings`, so the
    // ratio is meaningful (heuristic same-stem .png/.jpeg never counts toward
    // either side).
    let bundle_missing_per_package: BTreeMap<String, BTreeMap<String, BTreeSet<String>>> = packages
        .iter()
        .map(|(pid, pkg)| (pid.clone(), compute_missing_siblings(pkg)))
        .collect();
    duplicate_groups.retain_mut(|group| {
        group.refs.retain(|item| {
            let Some(pkg) = packages.get(&item.package_id) else {
                return true;
            };
            let Some(expected) = pkg.support_paths.get(&item.internal_path) else {
                return true;
            };
            let required_count = expected
                .iter()
                .filter(|s| !is_heuristic_same_stem_texture(&item.internal_path, s))
                .count();
            if required_count == 0 {
                return true;
            }
            let missing_len = bundle_missing_per_package
                .get(&item.package_id)
                .and_then(|map| map.get(&item.internal_path))
                .map(|set| set.len())
                .unwrap_or(0);
            missing_len < required_count
        });
        if group.refs.len() < 2 {
            return false;
        }
        group.package_ids = group
            .refs
            .iter()
            .map(|item| item.package_id.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        group.removable_bytes = group
            .refs
            .iter()
            .map(|item| item.effective_size)
            .sum::<u64>()
            .saturating_sub(
                group
                    .refs
                    .iter()
                    .map(|item| item.effective_size)
                    .min()
                    .unwrap_or(0),
            );
        true
    });

    duplicate_groups.sort_by(|a, b| {
        b.removable_bytes.cmp(&a.removable_bytes).then_with(|| {
            a.refs
                .first()
                .map(|item| item.internal_path.to_lowercase())
                .cmp(&b.refs.first().map(|item| item.internal_path.to_lowercase()))
        })
    });

    let mut scanned = ScannedData {
        files: file_fingerprints,
        packages,
        duplicate_groups,
        warnings,
        info: ScanInfo::default(),
    };

    if let Some(db) = db {
        match persist_scan_to_db(
            db,
            &scanned,
            target_package_id.as_deref(),
            |progress, message| {
                on_progress(progress, message);
            },
        ) {
            Ok(info) => {
                scanned.info = info;
            }
            Err(err) => {
                eprintln!("warning: failed to persist scan to database: {err:?}");
                scanned
                    .warnings
                    .push(format!("Database save failed: {err}"));
            }
        }
    }

    on_progress(1.0, "Scan completed".to_string());

    Ok(scanned)
}

fn read_fingerprint_from_disk(path: &Path) -> Option<(u64, u128)> {
    let metadata = fs::metadata(path).ok()?;
    let size = metadata.len();
    let modified_ns = metadata
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some((size, modified_ns))
}

fn persist_scan_to_db<F>(
    db: &Db,
    scanned: &ScannedData,
    target_package_id: Option<&str>,
    mut on_progress: F,
) -> Result<ScanInfo>
where
    F: FnMut(f64, String),
{
    const DB_PROGRESS_START: f64 = 0.97;
    const DB_PROGRESS_END: f64 = 0.995;

    let mut conn = db
        .conn
        .lock()
        .map_err(|_| anyhow!("database connection poisoned"))?;

    let targets: Vec<(&String, &PreparedPackage)> = match target_package_id {
        Some(target) => scanned
            .packages
            .iter()
            .filter(|(package_id, _)| package_id.as_str() == target)
            .collect(),
        None => scanned.packages.iter().collect(),
    };

    let total = targets.len().max(1) as f64;
    let mut packages_persisted = 0usize;
    let mut resources_persisted = 0usize;

    on_progress(
        DB_PROGRESS_START,
        format!("Saving to database (0/{})", targets.len()),
    );

    let category_ids = db.categories.clone();

    // Pre-stat every target file in parallel. `read_fingerprint_from_disk` is
    // an fs::metadata() call — I/O-bound and embarrassingly parallel. Doing
    // it sequentially under the writer mutex was the dominant wall-time cost
    // on libraries with thousands of packages; rayon cuts it to ~CPU/disk
    // queue depth. Order of `targets` is preserved so the insert loop below
    // emits progress for the same package indices as before.
    let fingerprints: Vec<Option<(u64, u128)>> = {
        use rayon::prelude::*;
        targets
            .par_iter()
            .map(|(_, package)| read_fingerprint_from_disk(&package.file_path))
            .collect()
    };

    let tx = conn
        .transaction()
        .context("failed to start scan persistence tx")?;
    let mut creator_cache: std::collections::HashMap<String, i64> =
        std::collections::HashMap::new();
    {
        let mut pkg_stmt = tx
            .prepare(db::PACKAGE_UPSERT_SQL)
            .context("failed to prepare package upsert")?;
        let mut res_stmt = tx
            .prepare(db::RESOURCE_UPSERT_SQL)
            .context("failed to prepare resource upsert")?;

        for (index, (package_id, package)) in targets.iter().enumerate() {
            let file_path_str = package.file_path.display().to_string();
            let (size, modified_ns) = match fingerprints[index] {
                Some(fp) => fp,
                None => continue,
            };

            let creator_id = match crate::naming::creator_from_package_id(package_id) {
                Some(name) => match creator_cache.get(name) {
                    Some(id) => Some(*id),
                    None => {
                        let id = db::ensure_creator_id(&tx, name)?;
                        creator_cache.insert(name.to_string(), id);
                        Some(id)
                    }
                },
                None => None,
            };

            db::execute_package_upsert(
                &mut pkg_stmt,
                package_id,
                &file_path_str,
                size,
                modified_ns,
                None,
                creator_id,
            )?;
            for resource in &package.resource_refs {
                db::execute_resource_upsert(&mut res_stmt, resource, &category_ids)?;
            }

            packages_persisted += 1;
            resources_persisted += package.resource_refs.len();

            let progress = DB_PROGRESS_START
                + (((index + 1) as f64 / total) * (DB_PROGRESS_END - DB_PROGRESS_START));
            on_progress(
                progress,
                format!(
                    "Saving to database ({}/{}): {}",
                    index + 1,
                    targets.len(),
                    package_id
                ),
            );
        }
    }

    // Scans never delete rows. Settings → Clear Database is the only path
    // that removes data.
    let packages_pruned = 0usize;

    tx.commit().context("failed to commit scan persistence")?;

    Ok(ScanInfo {
        packages_persisted,
        resources_persisted,
        packages_pruned,
    })
}

fn scan_package(file_path: &Path) -> Result<PreparedPackage> {
    let package_id = file_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| anyhow!("invalid package file name: {}", file_path.display()))?
        .to_string();
    let package_file = file_path.display().to_string();

    let file = fs::File::open(file_path)
        .with_context(|| format!("failed to open {}", file_path.display()))?;
    let mut archive = ZipArchive::new(file)
        .with_context(|| format!("failed to open zip archive {}", file_path.display()))?;

    let mut name_map = HashMap::new();
    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        name_map.insert(normalize_zip_path(entry.name()), entry.name().to_string());
    }

    let meta_name = name_map
        .get(META_PATH)
        .ok_or_else(|| anyhow!("{} is missing meta.json", file_path.display()))?
        .clone();

    let meta_bytes = {
        let mut meta_file = archive.by_name(&meta_name)?;
        let mut buf = Vec::new();
        meta_file.read_to_end(&mut buf)?;
        buf
    };

    let meta = read_json_bytes(&meta_bytes, &format!("{}:{META_PATH}", file_path.display()))?;
    let content_list = meta
        .get("contentList")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(normalize_zip_path)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let dependencies = meta
        .get("dependencies")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let license_type = meta
        .get("licenseType")
        .and_then(Value::as_str)
        .unwrap_or("UNKNOWN")
        .to_string();

    let mut resource_refs = Vec::with_capacity(archive.len().saturating_sub(1));
    let mut support_paths: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        if entry.is_dir() {
            continue;
        }
        let internal_path = normalize_zip_path(entry.name());
        if internal_path == META_PATH {
            continue;
        }
        if internal_path
            .rsplit('.')
            .next()
            .map(|ext| ext.eq_ignore_ascii_case("vaj"))
            .unwrap_or(false)
        {
            let mut raw = Vec::new();
            entry.read_to_end(&mut raw)?;
            let vam_path = Path::new(&internal_path)
                .with_extension("vam")
                .to_string_lossy()
                .replace('\\', "/");
            let mut family_support_paths = BTreeSet::from([internal_path.clone()]);
            family_support_paths.extend(extract_vaj_support_paths(&internal_path, &raw));
            support_paths
                .entry(vam_path)
                .or_default()
                .extend(family_support_paths);
        }
        if internal_path
            .rsplit('.')
            .next()
            .map(|ext| ext.eq_ignore_ascii_case("vam"))
            .unwrap_or(false)
        {
            let entry = support_paths.entry(internal_path.clone()).or_default();
            entry.extend(same_stem_texture_paths(&internal_path));
            // VAM/RG loads the `.vab` Unity binary cache by stem next to its
            // `.vam`. Registering it here lets the .vam's effective_size
            // reflect the full bundle (so the UI shows bundled bytes, not
            // .vam-only) and lets the Everything-mode UI hide the .vab as
            // a managed bundle member.
            let vab_path = Path::new(&internal_path)
                .with_extension("vab")
                .to_string_lossy()
                .replace('\\', "/");
            entry.insert(vab_path);
        }
        if internal_path
            .rsplit('.')
            .next()
            .map(|ext| ext.eq_ignore_ascii_case("vmi"))
            .unwrap_or(false)
        {
            // Mirror of the .vam → .vab pairing for morph bundles: .vmb is
            // the stem-loaded binary cache for a .vmi.
            let entry = support_paths.entry(internal_path.clone()).or_default();
            let vmb_path = Path::new(&internal_path)
                .with_extension("vmb")
                .to_string_lossy()
                .replace('\\', "/");
            entry.insert(vmb_path);
        }
        let crc32 = entry.crc32();
        resource_refs.push(ResourceRef {
            package_id: package_id.clone(),
            package_file: package_file.clone(),
            internal_path,
            crc32: Some(crc32),
            size: entry.size(),
            effective_size: entry.size(),
        });
    }
    apply_effective_resource_sizes(&mut resource_refs, &support_paths);

    let sibling_owners = build_sibling_owners(&support_paths);

    Ok(PreparedPackage {
        file_path: file_path.to_path_buf(),
        package_id,
        meta,
        content_list,
        dependencies,
        license_type,
        resource_refs,
        support_paths,
        sibling_owners,
        removed_paths: BTreeSet::new(),
        required_dependencies: BTreeSet::new(),
        replacement_map: BTreeMap::new(),
    })
}

/// True for siblings added by `same_stem_texture_paths` as a runtime guess
/// (parent has same stem and one of the `.png` / `.jpg` / `.jpeg` extensions).
/// These paths are added to `support_paths` regardless of whether the .vaj
/// actually references them, so their absence from a ZIP is normal — not
/// evidence of an incomplete bundle. The required-bundle members (.vab/.vmb
/// load-by-stem caches, .vaj-referenced textures) have distinct extensions
/// or distinct paths and survive this filter.
fn is_heuristic_same_stem_texture(parent: &str, sibling: &str) -> bool {
    let parent_stem = Path::new(parent).file_stem().and_then(|s| s.to_str());
    let sibling_path = Path::new(sibling);
    let sibling_stem = sibling_path.file_stem().and_then(|s| s.to_str());
    match (parent_stem, sibling_stem) {
        (Some(p), Some(s)) if p == s => {}
        _ => return false,
    }
    sibling_path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            let lower = e.to_ascii_lowercase();
            lower == "png" || lower == "jpg" || lower == "jpeg"
        })
        .unwrap_or(false)
}

/// For each bundle parent in `package.support_paths`, returns the subset of
/// expected siblings that are NOT actually present in the package's ZIP
/// (i.e. listed in `support_paths` but not in `resource_refs`). Heuristic
/// same-stem `.png/.jpg/.jpeg` entries are excluded — their absence is
/// normal, and including them produces false positives (e.g. flagging a
/// well-formed VAR that just doesn't ship a preview thumbnail). Empty
/// entries are filtered out, so a parent appears in the result only when at
/// least one *real* sibling (.vaj-referenced or .vab/.vmb cache) is missing.
fn compute_missing_siblings(
    package: &PreparedPackage,
) -> BTreeMap<String, BTreeSet<String>> {
    let existing: BTreeSet<&str> = package
        .resource_refs
        .iter()
        .map(|r| r.internal_path.as_str())
        .collect();
    package
        .support_paths
        .iter()
        .filter_map(|(parent, siblings)| {
            let missing: BTreeSet<String> = siblings
                .iter()
                .filter(|sibling| !existing.contains(sibling.as_str()))
                .filter(|sibling| !is_heuristic_same_stem_texture(parent, sibling))
                .cloned()
                .collect();
            if missing.is_empty() {
                None
            } else {
                Some((parent.clone(), missing))
            }
        })
        .collect()
}

/// Inverts `support_paths` into a `sibling → owners` map. The shared/exclusive
/// classification the dedup cascade needs (does any other `.vam`/`.vmi` in
/// this package still pull this sibling in?) is implicit in the owner set —
/// `len > 1` means shared. Computed once per package at scan time so the
/// cascade is a constant-time lookup, not a re-classification.
fn build_sibling_owners(
    support_paths: &BTreeMap<String, BTreeSet<String>>,
) -> BTreeMap<String, BTreeSet<String>> {
    let mut sibling_owners: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (parent, siblings) in support_paths {
        for sibling in siblings {
            sibling_owners
                .entry(sibling.clone())
                .or_default()
                .insert(parent.clone());
        }
    }
    sibling_owners
}

pub(crate) fn build_default_keep_map(
    duplicate_groups: &[DuplicateGroup],
) -> BTreeMap<String, String> {
    duplicate_groups
        .iter()
        .map(|group| (group.key.clone(), KEEP_ALL_VALUE.to_string()))
        .collect()
}

pub(crate) fn build_scan_response(scanned: &ScannedData) -> ScanResponse {
    let reclaimable_bytes = scanned
        .duplicate_groups
        .iter()
        .map(|item| item.removable_bytes)
        .sum();
    let groups = scanned
        .duplicate_groups
        .iter()
        .map(|group| ScanDuplicateGroup {
            key: group.key.clone(),
            refs: group
                .refs
                .iter()
                .map(|item| ScanResourceRef {
                    package_id: item.package_id.clone(),
                    internal_path: item.internal_path.clone(),
                    crc32: item.crc32,
                    size: item.size,
                    effective_size: item.effective_size,
                })
                .collect(),
            removable_bytes: group.removable_bytes,
            package_ids: group.package_ids.clone(),
        })
        .collect();
    let package_files = scanned
        .packages
        .iter()
        .map(|(package_id, package)| (package_id.clone(), package.file_path.display().to_string()))
        .collect();
    let package_sizes = scanned
        .packages
        .iter()
        .map(|(package_id, package)| {
            let size = fs::metadata(&package.file_path)
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            (package_id.clone(), size)
        })
        .collect();

    let bundle_index = scanned
        .packages
        .iter()
        .filter_map(|(package_id, package)| {
            if package.support_paths.is_empty() {
                None
            } else {
                Some((package_id.clone(), package.support_paths.clone()))
            }
        })
        .collect();

    let bundle_missing_index = scanned
        .packages
        .iter()
        .filter_map(|(package_id, package)| {
            let missing = compute_missing_siblings(package);
            if missing.is_empty() {
                None
            } else {
                Some((package_id.clone(), missing))
            }
        })
        .collect();

    ScanResponse {
        summary: ScanSummary {
            packages: scanned.packages.len(),
            duplicate_groups: scanned.duplicate_groups.len(),
            reclaimable_bytes,
        },
        groups,
        package_files,
        package_sizes,
        default_keep_map: build_default_keep_map(&scanned.duplicate_groups),
        warnings: scanned.warnings.clone(),
        info: scanned.info.clone(),
        bundle_index,
        bundle_missing_index,
    }
}

pub(crate) fn cache_scan_result(
    scan_cache: &Arc<Mutex<HashMap<String, CachedScan>>>,
    input_dir: &Path,
    additional_dirs: &[PathBuf],
    target_var_path: Option<&Path>,
    scanned: &ScannedData,
) -> Result<()> {
    let roots = scan_roots(input_dir, additional_dirs);
    let key = scan_cache_key_multi(&root_refs(&roots), target_var_path);
    let mut guard = scan_cache
        .lock()
        .map_err(|_| anyhow!("scan cache poisoned"))?;
    guard.insert(
        key,
        CachedScan {
            files: scanned.files.clone(),
            scanned: scanned.clone(),
        },
    );
    Ok(())
}

pub(crate) fn load_cached_scan(
    scan_cache: &Arc<Mutex<HashMap<String, CachedScan>>>,
    input_dir: &Path,
    additional_dirs: &[PathBuf],
    target_var_path: Option<&Path>,
) -> Result<Option<ScannedData>> {
    let roots = scan_roots(input_dir, additional_dirs);
    let key = scan_cache_key_multi(&root_refs(&roots), target_var_path);
    let cached = {
        let guard = scan_cache
            .lock()
            .map_err(|_| anyhow!("scan cache poisoned"))?;
        guard.get(&key).cloned()
    };

    let Some(cached) = cached else {
        return Ok(None);
    };

    let current_files = collect_var_files_with_targets(&roots, target_var_path)?
        .into_iter()
        .map(|entry| entry.fingerprint)
        .collect::<Vec<_>>();

    if current_files == cached.files {
        Ok(Some(cached.scanned))
    } else {
        Ok(None)
    }
}

pub(crate) fn load_cached_scan_with_target(
    scan_cache: &Arc<Mutex<HashMap<String, CachedScan>>>,
    input_dir: &Path,
    additional_dirs: &[PathBuf],
    target_var_path: Option<&Path>,
) -> Result<Option<ScannedData>> {
    load_cached_scan(scan_cache, input_dir, additional_dirs, target_var_path)
}

fn root_refs(roots: &[PathBuf]) -> Vec<&Path> {
    roots.iter().map(PathBuf::as_path).collect()
}
