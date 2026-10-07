//! VAR Packages maintenance: Clean Duplicates + Organize by Creator.
//!
//! Both features are the same shape — walk the roots, decide per file "do X or
//! leave it alone" — so they are two read-only planners feeding one shared
//! executor. Planners never touch the disk beyond reading; every mutation goes
//! through `FileOps`, which is also the seam that keeps the real Recycle Bin out
//! of the test suite.
//!
//! Unlike `execute.rs` (which rewrites the *inside* of a `.var`), everything
//! here operates on whole `.var` files. The `.vab`/`.vmb` sibling invariant is
//! therefore out of scope: it governs paths inside an archive, and moving or
//! recycling a whole package can never split a cache from its descriptor.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    thread,
};

use zip::ZipArchive;

use crate::{
    models::{
        PackageAction, PackageCandidate, PackageOpResponse, RecycleSupport, VarRefs, META_PATH,
    },
    naming,
    utils::{decode_text, normalize_zip_path, read_json_bytes, ScanDepth},
};

/// Directories this app creates *inside* the user's library, holding same-stem
/// byte-copies of live packages.
///
/// `execute.rs` writes `backup/` and `changed/` under the output folder;
/// `fix_var` and `internalize` fall back to `<target's own folder>/fix-var-backup`
/// and `internalize-backup` when no output folder is configured — i.e. straight
/// into the scanned library.
///
/// These must be invisible to both planners. A `fix-var-backup/Pkg.3.var` is a
/// byte-identical copy of the original with the *same stem*, so it looks exactly
/// like a duplicate: keep-the-larger would keep the 400 MB pre-optimization
/// backup and recycle the 120 MB live file, silently undoing this app's own core
/// feature. Organize is worse — it would rename backups into the live library,
/// destroying the backup/live distinction with no Recycle Bin and no undo.
pub(crate) const APP_MANAGED_DIRS: &[&str] =
    &["backup", "changed", "fix-var-backup", "internalize-backup"];

/// True when any path component is one of `APP_MANAGED_DIRS` (case-insensitive).
pub(crate) fn is_app_managed_path(path: &Path) -> bool {
    path.components().any(|c| {
        let name = c.as_os_str().to_string_lossy();
        APP_MANAGED_DIRS
            .iter()
            .any(|dir| name.eq_ignore_ascii_case(dir))
    })
}

/// True when any path component is `AddonPackages` — the folder VAM actually
/// scans. Used to break exact-copy ties toward the copy VAM really loads.
pub(crate) fn is_in_addon_packages(path: &Path) -> bool {
    path.components()
        .any(|c| c.as_os_str().to_string_lossy().eq_ignore_ascii_case("AddonPackages"))
}

// ----------------------------------------------------------------------------
// Reading refs
// ----------------------------------------------------------------------------

/// Opens a `.var` once and harvests both kinds of package reference.
///
/// `meta_keys` are the **top-level** `meta.json` `dependencies` keys only. The
/// recursive walk (`tasks::collect_meta_dependency_ids`) is deliberately not
/// reused: it reports VAM's build-time snapshot of the whole dependency tree,
/// naming versions the user never installed, which would protect rows that can
/// then never be freed. `tasks::inspect_var_for_dependency` sets the precedent
/// of reading `deps.keys()` without recursion.
///
/// `payload_refs` are `Creator.Pkg.N:/path` ids found in text payloads, only
/// when `deep` is set.
///
/// Returns `Err` for anything unreadable — a locked file (VAM holds every `.var`
/// open), a truncated download, a missing/invalid `meta.json`, or a `meta.json`
/// stored with a method this build's `zip` lacks (Cargo.toml enables `deflate`
/// only). Callers must preserve the `Err`: "could not read" is not "declares no
/// dependencies".
pub(crate) fn read_var_refs(path: &Path, deep: bool) -> Result<VarRefs, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("could not open: {e}"))?;
    let mut archive = ZipArchive::new(file).map_err(|e| format!("not a readable VAR: {e}"))?;

    let mut refs = VarRefs::default();

    // meta.json — required. Its absence means this is not a usable VAR.
    let meta_index = (0..archive.len()).find(|i| {
        archive
            .by_index_raw(*i)
            .map(|e| normalize_zip_path(e.name()).eq_ignore_ascii_case(META_PATH))
            .unwrap_or(false)
    });
    let Some(meta_index) = meta_index else {
        return Err("not a readable VAR: meta.json not found".to_string());
    };

    {
        use std::io::Read;
        let mut entry = archive
            .by_index(meta_index)
            .map_err(|e| format!("not a readable VAR: meta.json unreadable: {e}"))?;
        let mut buf = Vec::new();
        entry
            .read_to_end(&mut buf)
            .map_err(|e| format!("not a readable VAR: meta.json unreadable: {e}"))?;
        drop(entry);

        let value = read_json_bytes(&buf, "meta.json").map_err(|e| format!("not a readable VAR: {e}"))?;
        if let Some(deps) = value.get("dependencies").and_then(|d| d.as_object()) {
            for key in deps.keys() {
                let trimmed = key.trim();
                if !trimmed.is_empty() {
                    refs.meta_keys.insert(trimmed.to_string());
                }
            }
        }
    }

    if !deep {
        return Ok(refs);
    }

    // Text payloads — harvest every `pkg:/path` ref in one pass. The planner
    // intersects these against the candidate set, so the cost is independent of
    // how many candidates there are (unlike tasks.rs's per-target needle scan).
    for i in 0..archive.len() {
        let name = match archive.by_index_raw(i) {
            Ok(e) => {
                if e.is_dir() {
                    continue;
                }
                normalize_zip_path(e.name())
            }
            Err(_) => continue,
        };
        if !crate::fix_var::is_text_path(&name) {
            continue;
        }
        let Ok(mut entry) = archive.by_index(i) else {
            continue;
        };
        use std::io::Read;
        let mut buf = Vec::new();
        if entry.read_to_end(&mut buf).is_err() {
            continue;
        }
        drop(entry);
        let Some((text, _)) = decode_text(&buf) else {
            continue;
        };
        for (pkg, _path) in crate::fix_var::collect_pkg_refs(&text) {
            refs.payload_refs.insert(pkg);
        }
    }

    Ok(refs)
}

/// Builds a candidate for one `.var` on disk.
///
/// `readable` is false when `read_var_refs` returned `Err`, and that forces
/// `version` to `None`. Since a `None`-version file can never be a family's max,
/// this is what stops a truncated download — a half-copied `Pkg.6.var` from a
/// cancelled NAS transfer — from evicting the working `Pkg.5.var`. Validation is
/// free here because the ref pass already had to open every file.
pub(crate) fn build_candidate(
    base_dir: &Path,
    path: &Path,
    size: u64,
    modified_ns: u128,
    indexed: bool,
    readable: bool,
    in_scope: bool,
) -> PackageCandidate {
    let package_id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string();
    let version = if readable {
        naming::package_version(&package_id)
    } else {
        None
    };
    PackageCandidate {
        base_dir: base_dir.to_path_buf(),
        id_lc: package_id.to_ascii_lowercase(),
        base_lc: naming::package_base(&package_id).to_ascii_lowercase(),
        package_id,
        version,
        size,
        modified_ns,
        in_addon_packages: is_in_addon_packages(path),
        disabled: disabled_sidecar(path).exists(),
        indexed,
        in_scope,
        path: path.to_path_buf(),
    }
}

/// The disclosure shown when a Normal scan gathered packages it will not touch.
/// `None` when everything gathered was in scope.
pub(crate) fn out_of_scope_note(out_of_scope: usize) -> Option<String> {
    if out_of_scope == 0 {
        return None;
    }
    Some(format!(
        "Normal scan: {out_of_scope} package(s) in subfolders were left alone. \
         They are still checked for dependencies, so nothing they rely on gets removed.",
    ))
}

/// Whether the chosen depth lets the planners act on `path`.
///
/// Deep allows everything. Normal allows only `.var` files sitting directly in
/// their own root — the same set the page's Normal listing shows.
pub(crate) fn scope_allows(depth: ScanDepth, root: &Path, path: &Path) -> bool {
    match depth {
        ScanDepth::Recursive => true,
        ScanDepth::TopLevelOnly => path.parent() == Some(root),
    }
}

// ----------------------------------------------------------------------------
// Protection
// ----------------------------------------------------------------------------

/// Recomputes which removals must be held back because a *surviving* package
/// still references that exact version.
///
/// Returns `(removed, protected)` where `protected` maps an index to the
/// package_id of the survivor that references it.
///
/// Three things are load-bearing:
///
/// 1. **Survivors only.** Building the index over every package would let a
///    referrer that is itself being trashed protect an old version forever — the
///    library would never converge and re-running would do nothing.
/// 2. **The fixpoint.** `Scene.2 -> Morphs.1` protects `Morphs.1`, but
///    `Morphs.1 -> Tex.1` would still lose `Tex.1` and break the very file we
///    just protected. Each pass only moves indices removed -> protected, so it
///    is monotonic and terminates in at most N passes.
/// 3. **A removal needs the check iff no surviving file keeps its id.**
///    Recycling one of two identical copies leaves the id on disk, so no ref can
///    break. Recycling an older version makes that stem vanish, so it must be
///    checked. This falls out of the survivor set rather than being hardcoded
///    per reason.
pub(crate) fn compute_protection(
    candidates: &[PackageCandidate],
    refs: &HashMap<PathBuf, Result<VarRefs, String>>,
    initial_removed: HashSet<usize>,
) -> (HashSet<usize>, HashMap<usize, String>) {
    let mut removed = initial_removed;
    let mut protected: HashMap<usize, String> = HashMap::new();

    loop {
        let surviving_ids: HashSet<&str> = candidates
            .iter()
            .enumerate()
            .filter(|(i, _)| !removed.contains(i))
            .map(|(_, c)| c.id_lc.as_str())
            .collect();

        // dep id (lowercased) -> package_id of the first survivor referencing it.
        let mut ref_index: HashMap<String, String> = HashMap::new();
        for (i, cand) in candidates.iter().enumerate() {
            if removed.contains(&i) {
                continue;
            }
            let Some(Ok(var_refs)) = refs.get(&cand.path) else {
                continue;
            };
            for dep in var_refs.meta_keys.iter().chain(var_refs.payload_refs.iter()) {
                let dep_lc = dep.to_ascii_lowercase();
                // Exclude only this file's own exact id — never its family.
                // `tasks::run_dependency_check_task` skips family-wide, which
                // would hide the most important case there is:
                // `Creator.Pkg.3` declaring `Creator.Pkg.2`.
                if dep_lc == cand.id_lc {
                    continue;
                }
                // `.latest` can never protect: the version pass only removes
                // non-max versions and the copy pass always leaves a twin, so
                // no removal we make can change what `.latest` resolves to.
                if naming::package_version(&dep_lc).is_none() {
                    continue;
                }
                ref_index
                    .entry(dep_lc)
                    .or_insert_with(|| cand.package_id.clone());
            }
        }

        let newly: Vec<(usize, String)> = removed
            .iter()
            .copied()
            .filter(|i| !surviving_ids.contains(candidates[*i].id_lc.as_str()))
            .filter_map(|i| {
                ref_index
                    .get(&candidates[i].id_lc)
                    .map(|referrer| (i, referrer.clone()))
            })
            .collect();

        if newly.is_empty() {
            break;
        }
        for (i, referrer) in newly {
            removed.remove(&i);
            protected.insert(i, referrer);
        }
    }

    (removed, protected)
}

// ----------------------------------------------------------------------------
// Planner: Clean Duplicates
// ----------------------------------------------------------------------------

/// Sort key for choosing which of several identical ids survives.
///
/// Location outranks size deliberately. For true copies the sizes are almost
/// always *equal* — nothing re-zips a download — so size carries no signal and a
/// path-order tie-break would be a coin flip against VAM's load path. Preferring
/// the copy under `AddonPackages` keeps the one VAM actually loads and recycles
/// the staging copy. Size still decides when it differs, and the UI shows both
/// sizes so an unequal pair is obvious rather than silent.
fn exact_copy_rank(c: &PackageCandidate) -> (bool, u64) {
    (c.in_addon_packages, c.size)
}

pub(crate) fn plan_clean_duplicates(
    candidates: &[PackageCandidate],
    refs: &HashMap<PathBuf, Result<VarRefs, String>>,
    recycle_support: &dyn Fn(&Path) -> RecycleSupport,
) -> PackageOpResponse {
    let mut notes: Vec<String> = Vec::new();
    // index -> (reason, keeper index)
    let mut removals: HashMap<usize, (&'static str, usize)> = HashMap::new();

    // Passes 1 and 2 consider ONLY in-scope packages: in Normal mode the user
    // asked to work with the top-level files, so a subfolder copy must not pull
    // a top-level file into a duplicate group. Out-of-scope packages still reach
    // `compute_protection` below, because they are never in `removed` and so are
    // always survivors contributing to the ref index.
    let in_scope: Vec<usize> = candidates
        .iter()
        .enumerate()
        .filter(|(_, c)| c.in_scope)
        .map(|(i, _)| i)
        .collect();

    // --- Pass 1: exact copies (same id => same version by construction) ------
    let mut by_id: HashMap<&str, Vec<usize>> = HashMap::new();
    for &i in &in_scope {
        by_id.entry(candidates[i].id_lc.as_str()).or_default().push(i);
    }
    for group in by_id.values() {
        if group.len() < 2 {
            continue;
        }
        let mut ordered = group.clone();
        // Descending by rank; candidates are already path-sorted, so equal
        // ranks keep a deterministic path-ascending order.
        ordered.sort_by(|a, b| exact_copy_rank(&candidates[*b]).cmp(&exact_copy_rank(&candidates[*a])));
        let keeper = ordered[0];
        for &loser in &ordered[1..] {
            removals.insert(loser, ("duplicate_copy", keeper));
        }
    }

    // --- Pass 2: older versions, over in-scope pass-1 survivors --------------
    let mut by_family: HashMap<&str, Vec<usize>> = HashMap::new();
    for &i in &in_scope {
        if removals.contains_key(&i) {
            continue;
        }
        by_family
            .entry(candidates[i].base_lc.as_str())
            .or_default()
            .push(i);
    }

    let mut disabled_protected: HashMap<usize, String> = HashMap::new();

    for (family, members) in &by_family {
        let versioned: Vec<usize> = members
            .iter()
            .copied()
            .filter(|i| candidates[*i].version.is_some())
            .collect();
        if versioned.len() < 2 {
            continue;
        }

        // Leading-zero guard: `Pkg.7` and `Pkg.007` both parse to 7 but are
        // DIFFERENT ids to VAM — a ref to `.7` will not resolve to `.007`. The
        // confirmed keep rule covers "same id AND version", which these are not,
        // so rather than invent semantics we leave the whole family alone.
        let mut seen_versions: HashMap<u64, &str> = HashMap::new();
        let mut ambiguous = false;
        for &i in &versioned {
            let v = candidates[i].version.expect("filtered to Some above");
            match seen_versions.get(&v) {
                Some(other) if *other != candidates[i].id_lc.as_str() => {
                    ambiguous = true;
                    notes.push(format!(
                        "{}: '{}' and '{}' are different packages with the same version number — \
                         skipped this family to avoid guessing.",
                        family, other, candidates[i].package_id,
                    ));
                    break;
                }
                _ => {
                    seen_versions.insert(v, candidates[i].id_lc.as_str());
                }
            }
        }
        if ambiguous {
            continue;
        }

        let max_v = versioned
            .iter()
            .filter_map(|i| candidates[*i].version)
            .max()
            .expect("versioned is non-empty");

        // Disabled-keeper guard: VAM disables a package with a
        // `<file>.var.disabled` sidecar while the `.var` itself stays on disk
        // and visible to the walk. Recycling an *enabled* older version while
        // keeping a *disabled* newer one leaves nothing loadable.
        let keeper_disabled = versioned
            .iter()
            .filter(|i| candidates[**i].version == Some(max_v))
            .all(|i| candidates[*i].disabled);
        // Same failure, other cause: a newer version that is only outside
        // AddonPackages (offloaded, or in some extra folder) is not loaded
        // either, so the older one inside AddonPackages is still the live copy.
        let keeper_outside = versioned
            .iter()
            .filter(|i| candidates[**i].version == Some(max_v))
            .all(|i| !candidates[*i].in_addon_packages);

        let keeper = versioned
            .iter()
            .copied()
            .find(|i| candidates[*i].version == Some(max_v))
            .expect("max exists");

        for &i in &versioned {
            let v = candidates[i].version.expect("filtered above");
            if v >= max_v {
                continue;
            }
            if keeper_disabled && !candidates[i].disabled {
                disabled_protected.insert(
                    i,
                    format!(
                        "the newer version {} is disabled, so this one is still what VAM loads",
                        candidates[keeper].package_id,
                    ),
                );
                continue;
            }
            if keeper_outside && candidates[i].in_addon_packages && !candidates[i].disabled {
                disabled_protected.insert(
                    i,
                    format!(
                        "the newer version {} is outside AddonPackages (offloaded?), so this one \
                         is still what VAM loads",
                        candidates[keeper].package_id,
                    ),
                );
                continue;
            }
            removals.insert(i, ("older_version", keeper));
        }
    }

    // --- Protection ---------------------------------------------------------
    let initial: HashSet<usize> = removals.keys().copied().collect();
    let (removed, protected) = compute_protection(candidates, refs, initial);

    // Protection is only as complete as our ability to read the survivors —
    // including out-of-scope ones, since those are exactly the subfolder
    // packages whose refs we are relying on in Normal mode.
    let mut unreadable = 0usize;
    for (i, cand) in candidates.iter().enumerate() {
        if removed.contains(&i) {
            continue;
        }
        if let Some(Err(err)) = refs.get(&cand.path) {
            unreadable += 1;
            notes.push(format!("{}: {}", cand.package_id, err));
        }
    }
    let protection_complete = unreadable == 0;

    // --- Build actions ------------------------------------------------------
    let mut recycle_cache: HashMap<PathBuf, RecycleSupport> = HashMap::new();
    let mut actions: Vec<PackageAction> = Vec::new();
    let mut reclaimable = 0u64;

    for (&i, &(reason, keeper)) in removals.iter() {
        let cand = &candidates[i];
        let keep = &candidates[keeper];

        let (status, detail) = if let Some(referrer) = protected.get(&i) {
            (
                "protected",
                format!("still referenced by {referrer} — tick to remove anyway"),
            )
        } else if let Some(why) = disabled_protected.get(&i) {
            ("protected", why.clone())
        } else if !protection_complete && reason == "older_version" {
            // Not hard-protected: one locked file in a 10k library must not be
            // a total feature outage. Rendered unchecked, so removal still
            // requires an explicit opt-in.
            (
                "unverified",
                format!(
                    "{unreadable} package(s) could not be read for dependency info — \
                     protection is incomplete"
                ),
            )
        } else {
            ("pending", String::new())
        };

        // Recycle Bin availability is a property of the volume, probed once per
        // volume. `trash::delete` reports success on volumes that permanently
        // delete, so this is the only thing keeping the confirmed
        // "never a permanent delete" decision honest.
        let volume = volume_root(&cand.path);
        let support = *recycle_cache
            .entry(volume)
            .or_insert_with(|| recycle_support(&cand.path));

        let (status, detail) = if support == RecycleSupport::Unsupported {
            (
                "blocked".to_string(),
                "Recycle Bin unavailable on this volume — skipped to avoid permanent deletion"
                    .to_string(),
            )
        } else {
            (status.to_string(), detail)
        };

        if status == "pending" {
            reclaimable += cand.size;
        }

        actions.push(PackageAction {
            package_id: cand.package_id.clone(),
            file_path: cand.path.display().to_string(),
            dest_path: String::new(),
            size_bytes: cand.size,
            op: "trash".to_string(),
            reason: reason.to_string(),
            kept_path: keep.path.display().to_string(),
            kept_size_bytes: keep.size,
            detail,
            status,
            indexed: cand.indexed,
            disabled: cand.disabled,
            modified_ns: cand.modified_ns,
        });
    }

    // Add the disabled-guard protections that were never removals.
    for (&i, why) in disabled_protected.iter() {
        if removals.contains_key(&i) {
            continue;
        }
        let cand = &candidates[i];
        actions.push(PackageAction {
            package_id: cand.package_id.clone(),
            file_path: cand.path.display().to_string(),
            size_bytes: cand.size,
            op: "trash".to_string(),
            reason: "older_version".to_string(),
            detail: why.clone(),
            status: "protected".to_string(),
            indexed: cand.indexed,
            disabled: cand.disabled,
            modified_ns: cand.modified_ns,
            ..Default::default()
        });
    }

    // Stable output: path order, matching the walk.
    actions.sort_by(|a, b| a.file_path.cmp(&b.file_path));

    // Counts describe what the user asked to work with, not the full recursive
    // walk — out-of-scope packages were only read for their references.
    let scanned = in_scope.len() as u64;
    // Each candidate yields at most one action today; saturating_sub so a future
    // second action per candidate degrades the count instead of panicking on
    // underflow (or wrapping to 1.8e19 in release).
    let unchanged = scanned.saturating_sub(actions.len() as u64);

    PackageOpResponse {
        plan_id: 0,
        kind: "clean_duplicates".to_string(),
        actions,
        scanned,
        unchanged,
        notes,
        reclaimable_bytes: reclaimable,
        deep_scan_used: false,
        protection_complete,
        was_cancelled: false,
    }
}

// ----------------------------------------------------------------------------
// Planner: Organize by Creator
// ----------------------------------------------------------------------------

/// Where a file should be regrouped: the nearest ancestor named `AddonPackages`
/// at or below `root`, else `root` itself.
///
/// The anchor is what stops this feature blanking a VAM install. Nothing else in
/// the Rust source knows what `AddonPackages` is, and the UI field is a generic
/// "VAR Folder" (placeholder `D:\Vars`). A user who points it at `D:\VAM` and
/// regroups "under the root" would get `D:\VAM\<Creator>\...` — a directory VAM
/// never scans. Every package would vanish on next launch, with nothing deleted
/// and nothing in the Recycle Bin to restore.
///
/// The at-or-below clamp matters too: someone who deliberately scans
/// `...\AddonPackages\Downloads` must not have their files scattered up into
/// `...\AddonPackages\<Creator>\`, outside the folder they picked.
pub(crate) fn organize_base(root: &Path, path: &Path) -> PathBuf {
    for ancestor in path.ancestors().skip(1) {
        if !ancestor.starts_with(root) {
            break;
        }
        let is_addon = ancestor
            .file_name()
            .map(|n| n.to_string_lossy().eq_ignore_ascii_case("AddonPackages"))
            .unwrap_or(false);
        if is_addon {
            return ancestor.to_path_buf();
        }
    }
    root.to_path_buf()
}

/// Win32 refuses paths at/over MAX_PATH unless they are `\\?\`-prefixed, and
/// `CreateDirectoryW` reserves 12 more for an 8.3 name.
pub(crate) const MAX_PATH_UTF16: usize = 260;
pub(crate) const MAX_DIR_UTF16: usize = 248;

/// Length in UTF-16 code units — what Win32 actually counts.
///
/// `OsStr::len()` would be WTF-8 *bytes*, which over-counts roughly 3x for the
/// CJK creator names that are common in VAM and would spuriously skip valid moves.
#[cfg(windows)]
pub(crate) fn wide_len(path: &Path) -> usize {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str().encode_wide().count()
}

#[cfg(not(windows))]
pub(crate) fn wide_len(path: &Path) -> usize {
    path.as_os_str().len()
}

#[cfg(test)]
pub(crate) fn plan_organize_by_creator(candidates: &[PackageCandidate]) -> PackageOpResponse {
    plan_organize_by_creator_into(candidates, None)
}

/// `dest_root`, when set, is the single directory every creator folder is
/// created under (the Settings "VAR library folder / AddonPackages root"),
/// instead of each file's own AddonPackages ancestor. Loose top-level packages
/// still move; ones already at `<dest_root>/<Creator>/name.var` are unchanged.
pub(crate) fn plan_organize_by_creator_into(
    candidates: &[PackageCandidate],
    dest_root: Option<&Path>,
) -> PackageOpResponse {
    let mut notes: Vec<String> = Vec::new();
    let mut actions: Vec<PackageAction> = Vec::new();
    let mut unchanged = 0u64;

    // Per base, creator_lc -> the folder name already on disk, so `qing.*` lands
    // in an existing `Qing/` instead of creating a case-colliding sibling.
    let mut casing: HashMap<PathBuf, HashMap<String, String>> = HashMap::new();
    // Lowercased, because NTFS is case-insensitive without a `\\?\` prefix:
    // `qing.hair.1.var` and `Qing.Hair.1.var` collide on disk.
    let mut claimed_dests: HashMap<String, PathBuf> = HashMap::new();

    for cand in candidates {
        // Normal mode files the loose top-level packages into creator folders
        // and leaves existing subfolders untouched. Organize reads no
        // dependencies, so out-of-scope packages are simply not its business —
        // unlike Clean Duplicates, nothing here needs their references.
        if !cand.in_scope {
            continue;
        }
        let base = match dest_root {
            Some(root) => root.to_path_buf(),
            None => organize_base(&cand.base_dir, &cand.path),
        };

        let Some(creator) = naming::creator_from_package_id(&cand.package_id) else {
            notes.push(format!(
                "{}: no creator in the package name — left where it is.",
                cand.package_id,
            ));
            continue;
        };
        let Some(folder) = naming::sanitize_creator_folder(creator) else {
            notes.push(format!(
                "{}: creator '{creator}' is not usable as a folder name — left where it is.",
                cand.package_id,
            ));
            continue;
        };

        let map = casing.entry(base.clone()).or_insert_with(|| {
            let mut seen: HashMap<String, String> = HashMap::new();
            if let Ok(entries) = std::fs::read_dir(&base) {
                for entry in entries.flatten() {
                    if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        let name = entry.file_name().to_string_lossy().to_string();
                        seen.entry(name.to_ascii_lowercase()).or_insert(name);
                    }
                }
            }
            seen
        });
        let folder = map
            .entry(folder.to_ascii_lowercase())
            .or_insert(folder)
            .clone();

        let file_name = match cand.path.file_name() {
            Some(n) => n.to_os_string(),
            None => continue,
        };
        let dir = base.join(&folder);
        let dest = dir.join(&file_name);

        if dest == cand.path {
            unchanged += 1;
            continue;
        }

        let dest_key = dest.to_string_lossy().to_ascii_lowercase();

        // `dest.exists()` only sees the pre-apply disk, so it is structurally
        // blind to collisions this plan itself creates: two same-stem files in
        // different folders would both render as moves to the identical path.
        if let Some(other) = claimed_dests.get(&dest_key) {
            notes.push(format!(
                "{}: another package ({}) is already moving to {} — left where it is. \
                 Run Clean Duplicates first.",
                cand.package_id,
                other.display(),
                dest.display(),
            ));
            continue;
        }
        if dest.exists() {
            notes.push(format!(
                "{}: {} already exists — left where it is. Run Clean Duplicates first.",
                cand.package_id,
                dest.display(),
            ));
            continue;
        }
        // Organize LENGTHENS root-level paths, and Rust will not report the
        // failure: fs::rename/create_dir_all auto-prepend `\\?\`, so the move
        // succeeds and the walk still lists the file — only VAM (Mono, not
        // long-path aware) silently fails to load it.
        if wide_len(&dest) >= MAX_PATH_UTF16 || wide_len(&dir) >= MAX_DIR_UTF16 {
            notes.push(format!(
                "{}: the destination path would be too long for VAM to load — left where it is.",
                cand.package_id,
            ));
            continue;
        }

        claimed_dests.insert(dest_key, cand.path.clone());
        actions.push(PackageAction {
            package_id: cand.package_id.clone(),
            file_path: cand.path.display().to_string(),
            dest_path: dest.display().to_string(),
            size_bytes: cand.size,
            op: "move".to_string(),
            reason: "organize".to_string(),
            status: "pending".to_string(),
            indexed: cand.indexed,
            disabled: cand.disabled,
            modified_ns: cand.modified_ns,
            ..Default::default()
        });
    }

    actions.sort_by(|a, b| a.file_path.cmp(&b.file_path));

    PackageOpResponse {
        plan_id: 0,
        kind: "organize_by_creator".to_string(),
        actions,
        scanned: candidates.iter().filter(|c| c.in_scope).count() as u64,
        unchanged,
        notes,
        reclaimable_bytes: 0,
        deep_scan_used: false,
        protection_complete: true,
        was_cancelled: false,
    }
}

// ----------------------------------------------------------------------------
// FileOps — the mutation seam
// ----------------------------------------------------------------------------

/// The mutation seam. Deliberately does NOT cover the Recycle-Bin probe: that is
/// a per-volume property cached on `AppState` for the session and injected into
/// the planner as a closure, so the executor never needs to ask.
pub(crate) trait FileOps: Send + Sync {
    fn recycle(&self, path: &Path) -> Result<(), String>;
    fn rename(&self, src: &Path, dest: &Path) -> Result<(), String>;
}

/// The volume a path lives on, e.g. `D:\`. Recycle Bin availability is a
/// property of the volume, not of how the path is spelled.
pub(crate) fn volume_root(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut comps = path.components();
    match comps.next() {
        Some(Component::Prefix(prefix)) => {
            let mut s = prefix.as_os_str().to_os_string();
            s.push("\\");
            PathBuf::from(s)
        }
        _ => PathBuf::from("/"),
    }
}

pub(crate) struct RealFileOps;

impl FileOps for RealFileOps {
    fn recycle(&self, path: &Path) -> Result<(), String> {
        // Per-file, never `trash::delete_all`: one failure must not abort the
        // batch. And never a `fs::remove_file` fallback — a failed recycle has
        // to stay failed rather than quietly become a permanent delete.
        trash::delete(path).map_err(|e| friendly_trash_error(&e))
    }

    fn rename(&self, src: &Path, dest: &Path) -> Result<(), String> {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Couldn't create {}: {e}", parent.display()))?;
        }
        std::fs::rename(src, dest).map_err(|e| friendly_io_error(&e, "move"))
    }
}

const IN_USE_MESSAGE: &str =
    "In use — close VAM (and let Windows Defender finish scanning), then re-run.";

/// ERROR_SHARING_VIOLATION / ERROR_LOCK_VIOLATION, as raw Win32 codes and as the
/// HRESULT forms an IFileOperation failure surfaces as.
const WIN32_SHARING_VIOLATION: i32 = 32;
const WIN32_LOCK_VIOLATION: i32 = 33;
const HRESULT_SHARING_VIOLATION: i32 = 0x8007_0020u32 as i32;
const HRESULT_LOCK_VIOLATION: i32 = 0x8007_0021u32 as i32;

fn friendly_io_error(err: &std::io::Error, verb: &str) -> String {
    match err.raw_os_error() {
        Some(WIN32_SHARING_VIOLATION) | Some(WIN32_LOCK_VIOLATION) => IN_USE_MESSAGE.to_string(),
        _ => format!("Couldn't {verb}: {err}"),
    }
}

/// `trash::Error` is its own enum with no `raw_os_error()` and no `io::Error`
/// conversion, and the crate does not guarantee which variant an IFileOperation
/// failure lands in — so check the structured code *and* the description.
fn friendly_trash_error(err: &trash::Error) -> String {
    if let trash::Error::Os { code, .. } = err {
        if matches!(
            *code,
            WIN32_SHARING_VIOLATION
                | WIN32_LOCK_VIOLATION
                | HRESULT_SHARING_VIOLATION
                | HRESULT_LOCK_VIOLATION
        ) {
            return IN_USE_MESSAGE.to_string();
        }
    }
    let text = format!("{err:?}");
    if text.contains("0x80070020") || text.contains("0x80070021") {
        return IN_USE_MESSAGE.to_string();
    }
    format!("Couldn't recycle: {err}")
}

/// Empirically determine whether deletions on this path's volume really reach
/// the Recycle Bin.
///
/// `trash::delete` drives IFileOperation with FOF_ALLOWUNDO, and on a volume
/// with no Recycle Bin (exFAT/FAT32 removables — common for VAM libraries — or
/// NukeOnDelete) the shell permanently deletes and still returns `Ok(())`. A
/// UNC/`\\`-prefix string guard cannot substitute: `Path::starts_with` matches
/// whole components (so it never fires), and mapped drives (`net use Z:`),
/// `subst`, and FAT32 removables all present as a plain `Z:\`.
pub(crate) fn probe_recycle_support(path: &Path) -> RecycleSupport {
    #[cfg(windows)]
    {
        use windows::{core::PCWSTR, Win32::Storage::FileSystem::GetDriveTypeW};

        // GetDriveTypeW returns a bare u32 and the windows crate does not bind
        // the DRIVE_* values (they are plain C #defines), so spell it out.
        const DRIVE_FIXED: u32 = 3;

        let volume = volume_root(path);
        let mut wide: Vec<u16> = {
            use std::os::windows::ffi::OsStrExt;
            volume.as_os_str().encode_wide().collect()
        };
        wide.push(0);
        // Cheap first gate: anything but a fixed disk (removable, network,
        // CD-ROM, RAM disk) is refused without touching the filesystem.
        let drive_type = unsafe { GetDriveTypeW(PCWSTR(wide.as_ptr())) };
        if drive_type != DRIVE_FIXED {
            return RecycleSupport::Unsupported;
        }

        // A fixed disk can still have NukeOnDelete set, so prove it. The probe
        // file goes in the folder we are about to mutate — provably writable —
        // rather than the volume root, whose default DACL denies a
        // non-elevated user file creation and would refuse every candidate on C:.
        let dir = if path.is_dir() {
            path.to_path_buf()
        } else {
            match path.parent() {
                Some(p) => p.to_path_buf(),
                None => return RecycleSupport::Unsupported,
            }
        };
        let probe = dir.join(format!(
            ".vamvd-recycle-probe-{}.tmp",
            std::process::id()
        ));
        if std::fs::write(&probe, b"probe").is_err() {
            // Can't prove it recycles, so don't risk it.
            return RecycleSupport::Unsupported;
        }
        if trash::delete(&probe).is_err() {
            let _ = std::fs::remove_file(&probe);
            return RecycleSupport::Unsupported;
        }
        // Ok(()) is NOT proof — only presence in the bin is.
        let probe_name = probe
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        match trash::os_limited::list() {
            Ok(items) => {
                let found: Vec<_> = items
                    .into_iter()
                    .filter(|i| i.name.to_string_lossy() == probe_name)
                    .collect();
                if found.is_empty() {
                    RecycleSupport::Unsupported
                } else {
                    let _ = trash::os_limited::purge_all(found);
                    RecycleSupport::Supported
                }
            }
            Err(_) => RecycleSupport::Unsupported,
        }
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        RecycleSupport::Supported
    }
}

// ----------------------------------------------------------------------------
// Executor
// ----------------------------------------------------------------------------

/// Applies a plan. Each file is atomic on its own, so a failure part-way needs
/// no rollback — the remaining actions simply report what happened.
pub(crate) fn run_apply_package_actions(
    actions: &mut [PackageAction],
    ops: &dyn FileOps,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(f64, &str),
) -> (u64, bool) {
    let total = actions.len().max(1);
    let mut reclaimed = 0u64;
    let mut cancelled = false;

    for (index, action) in actions.iter_mut().enumerate() {
        if cancel.load(Ordering::SeqCst) {
            cancelled = true;
            break;
        }
        progress(
            index as f64 / total as f64,
            &format!("{} ({}/{})", action.package_id, index + 1, total),
        );

        // Already resolved at plan/recompute time (protected, blocked, ...).
        if action.status != "pending" {
            continue;
        }

        let src = PathBuf::from(&action.file_path);

        // Cheap invariants against a planner bug. Note `.var.disabled` is
        // deliberately NOT accepted: package_id is the file stem, so
        // `Creator.Pkg.1.var` as a stem would make "var" the version segment and
        // inject a phantom into a dedup family. Sidecars are only ever reached
        // by stat'ing `<var name>.disabled`, never enumerated.
        let is_var = src
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("var"))
            .unwrap_or(false);
        if !is_var || !matches!(action.op.as_str(), "trash" | "move") {
            action.status = "failed".to_string();
            action.detail = "refusing to act on a non-.var path".to_string();
            continue;
        }

        if !src.exists() {
            action.status = "skipped".to_string();
            action.detail = "already gone".to_string();
            continue;
        }

        // TOCTOU: the preview is a snapshot; anything that changed under it is
        // no longer the file the user approved.
        match std::fs::metadata(&src) {
            Ok(meta) => {
                let modified_ns = meta
                    .modified()
                    .ok()
                    .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos())
                    .unwrap_or(action.modified_ns);
                if meta.len() != action.size_bytes || modified_ns != action.modified_ns {
                    action.status = "skipped".to_string();
                    action.detail = "changed since preview".to_string();
                    continue;
                }
            }
            Err(err) => {
                action.status = "failed".to_string();
                action.detail = format!("Couldn't read: {err}");
                continue;
            }
        }

        match action.op.as_str() {
            "trash" => {
                // `collect_var_files_multi` dedupes by `fs::canonicalize` with a
                // silent raw-path fallback. On a sharing violation `metadata`
                // succeeds (it falls back to FindFirstFileW) while
                // `canonicalize` fails, so one physical file reachable through
                // two roots can enter the walk twice with the same id AND size —
                // and the exact-copy rule would then trash "the copy" that is in
                // fact the only copy. Fail closed.
                if action.reason == "duplicate_copy" {
                    let keep = PathBuf::from(&action.kept_path);
                    match (std::fs::canonicalize(&src), std::fs::canonicalize(&keep)) {
                        (Ok(a), Ok(b)) if a == b => {
                            action.status = "skipped".to_string();
                            action.detail = "same file as the copy being kept".to_string();
                            continue;
                        }
                        (Err(_), _) | (_, Err(_)) => {
                            action.status = "skipped".to_string();
                            action.detail =
                                "couldn't confirm this is a different file from the one being kept"
                                    .to_string();
                            continue;
                        }
                        _ => {}
                    }
                }

                match ops.recycle(&src) {
                    Ok(()) => {
                        reclaimed += action.size_bytes;
                        action.status = "done".to_string();
                        action.detail = "moved to the Recycle Bin".to_string();
                        // A `.disabled` sidecar left behind would be orphaned.
                        let sidecar = disabled_sidecar(&src);
                        if sidecar.exists() {
                            if let Err(err) = ops.recycle(&sidecar) {
                                action.detail =
                                    format!("{} (its .disabled marker stayed: {err})", action.detail);
                            }
                        }
                        // A loose preview image describing a now-gone VAR is
                        // just orphan clutter — send it along too.
                        for img in image_sidecars(&src) {
                            if let Err(err) = ops.recycle(&img) {
                                action.detail =
                                    format!("{} (its preview image stayed: {err})", action.detail);
                            }
                        }
                    }
                    Err(err) => {
                        action.status = "failed".to_string();
                        action.detail = err;
                    }
                }
            }
            "move" => {
                let dest = PathBuf::from(&action.dest_path);

                // Structural backstop against a planner bug that would move a
                // package out of the folder VAM scans.
                if is_in_addon_packages(&src) && !is_in_addon_packages(&dest) {
                    action.status = "failed".to_string();
                    action.detail =
                        "refusing to move a package out of AddonPackages".to_string();
                    continue;
                }

                // LOAD-BEARING, not stylistic: fs::rename is MoveFileExW with
                // MOVEFILE_REPLACE_EXISTING and SILENTLY replaces the
                // destination on Windows. This check is the only thing between
                // organize and data loss. Do not remove it.
                if dest.exists() {
                    action.status = "skipped".to_string();
                    action.detail = "something is already at the destination".to_string();
                    continue;
                }

                match ops.rename(&src, &dest) {
                    Ok(()) => {
                        action.status = "done".to_string();
                        action.detail = format!("moved to {}", dest.display());
                        // Without its marker VAM would silently re-enable a
                        // package the user deliberately turned off.
                        let sidecar = disabled_sidecar(&src);
                        if sidecar.exists() {
                            let sidecar_dest = disabled_sidecar(&dest);
                            if let Err(err) = ops.rename(&sidecar, &sidecar_dest) {
                                action.detail = format!(
                                    "{} (its .disabled marker stayed behind: {err})",
                                    action.detail,
                                );
                            }
                        }
                        // Move the loose preview image so it stays beside its VAR.
                        // Same stem, new folder. Skip (never overwrite) if an
                        // image is already there — fs::rename would silently
                        // clobber it, mirroring the VAR's own dest-exists caution.
                        if let Some(dest_dir) = dest.parent() {
                            for img in image_sidecars(&src) {
                                let img_dest = match img.file_name() {
                                    Some(name) => dest_dir.join(name),
                                    None => continue,
                                };
                                if img_dest.exists() {
                                    action.detail = format!(
                                        "{} (its preview image was left behind — one already exists at the destination)",
                                        action.detail,
                                    );
                                    continue;
                                }
                                if let Err(err) = ops.rename(&img, &img_dest) {
                                    action.detail = format!(
                                        "{} (its preview image stayed behind: {err})",
                                        action.detail,
                                    );
                                }
                            }
                        }
                    }
                    Err(err) => {
                        action.status = "failed".to_string();
                        action.detail = err;
                    }
                }
            }
            _ => unreachable!("op validated above"),
        }
    }

    progress(1.0, "Finished");
    (reclaimed, cancelled)
}

/// `<file>.var` -> `<file>.var.disabled`, VAM's "this package is off" marker.
pub(crate) fn disabled_sidecar(var_path: &Path) -> PathBuf {
    let mut name = var_path.as_os_str().to_os_string();
    name.push(".disabled");
    PathBuf::from(name)
}

const SIDE_CAR_IMAGE_EXTS: &[&str] = &["jpg", "jpeg", "png"];

/// Existing `<var stem>.<img ext>` files sitting beside a `.var` — the loose
/// preview images the Export Scene Images feature writes. So the picture stays
/// glued to its VAR, these travel with it on move/recycle/delete.
pub(crate) fn image_sidecars(var_path: &Path) -> Vec<PathBuf> {
    let (Some(dir), Some(stem)) = (
        var_path.parent(),
        var_path.file_stem().and_then(|s| s.to_str()),
    ) else {
        return Vec::new();
    };
    SIDE_CAR_IMAGE_EXTS
        .iter()
        .map(|ext| dir.join(format!("{stem}.{ext}")))
        .filter(|p| p.exists())
        .collect()
}

// ----------------------------------------------------------------------------
// Tauri commands + task wiring
//
// Both planners and the apply run on a worker thread and report through the
// shared task registry, following `start_backfill_sizes_task`. All three
// register a cancel flag: `cancel_task` is documented as "a no-op for tasks
// without a cancel flag", so without one the Cancel button would silently do
// nothing.
// ----------------------------------------------------------------------------

use std::sync::{Arc, Mutex};

use rayon::prelude::*;
use tauri::State;

use crate::{
    db::Db,
    models::{AppState, PackagePlan, ProgressPayload, TaskHandle},
    tasks::{load_known_package_ids, new_progress_payload, set_task_progress},
};

/// Registers a task + its cancel flag and hands back the pieces a worker needs.
pub(crate) fn begin_task(
    state: &AppState,
    phase: &str,
    message: &str,
) -> Result<
    (
        u64,
        Arc<Mutex<HashMap<u64, ProgressPayload>>>,
        Arc<AtomicBool>,
    ),
    String,
> {
    let task_id = state.next_task_id.fetch_add(1, Ordering::SeqCst) + 1;
    state
        .tasks
        .lock()
        .map_err(|_| "task state poisoned".to_string())?
        .insert(task_id, new_progress_payload(phase, message));

    let cancel = Arc::new(AtomicBool::new(false));
    state
        .cancellations
        .lock()
        .map_err(|_| "cancellation state poisoned".to_string())?
        .insert(task_id, Arc::clone(&cancel));

    Ok((task_id, Arc::clone(&state.tasks), cancel))
}

fn finish_task(
    tasks: &Arc<Mutex<HashMap<u64, ProgressPayload>>>,
    task_id: u64,
    phase_stem: &str,
    result: Result<PackageOpResponse, String>,
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
                format!("{phase_stem}_cancelled")
            } else {
                format!("{phase_stem}_complete")
            };
            task.message = if payload.was_cancelled {
                "Cancelled".to_string()
            } else {
                format!("{} package(s) to review", payload.actions.len())
            };
            task.error = None;
            task.package_op_result = Some(payload);
        }
        Err(err) => {
            task.phase = format!("{phase_stem}_failed");
            task.message = "Failed".to_string();
            task.error = Some(err);
        }
    }
}

/// Probe result for this path's volume, computed once per volume per session.
/// Takes the cache handle rather than `AppState` so a worker thread can use it
/// after the `State` guard is gone.
fn recycle_support_for(
    cache: &Arc<Mutex<HashMap<PathBuf, RecycleSupport>>>,
    path: &Path,
) -> RecycleSupport {
    let volume = volume_root(path);
    if let Ok(guard) = cache.lock() {
        if let Some(hit) = guard.get(&volume) {
            return *hit;
        }
    }
    // Probe outside the lock: it writes a file and enumerates the Recycle Bin,
    // and holding the mutex across that would serialize every caller.
    let support = probe_recycle_support(path);
    if let Ok(mut guard) = cache.lock() {
        guard.insert(volume, support);
    }
    support
}

/// Drops every cached view of the library. Called after any mutation — both
/// caches hold absolute paths, so either one can otherwise serve rows for files
/// that are now in the Recycle Bin.
fn invalidate_var_packages_cache(state: &AppState) {
    state
        .var_packages_cache_generation
        .fetch_add(1, Ordering::SeqCst);
    if let Ok(mut cache) = state.var_packages_folder_cache.lock() {
        *cache = None;
    }
}

/// Sends one `.var` to the Recycle Bin.
///
/// Deliberately synchronous and un-checked: this is the user pointing at one
/// specific package and saying "remove that". Unlike Clean Duplicates it does
/// not scan the library for dependents — that would be a multi-minute read on a
/// large collection for a single click — so the confirm text warns instead, and
/// the Recycle Bin is what makes the mistake recoverable.
///
/// Returns the bytes freed.
#[tauri::command]
pub(crate) fn delete_var_package(
    file_path: String,
    state: State<'_, AppState>,
) -> Result<u64, String> {
    let path = PathBuf::from(&file_path);

    // Same guard as the executor: package identity is the file stem, so this
    // must never be pointed at a `.var.disabled` sidecar or anything else.
    let is_var = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("var"))
        .unwrap_or(false);
    if !is_var {
        return Err("Not a .var file.".to_string());
    }
    if !path.is_file() {
        return Err("That package is already gone.".to_string());
    }

    // Refuse rather than silently permanently delete on a volume with no bin.
    if recycle_support_for(&state.recycle_support, &path) == RecycleSupport::Unsupported {
        return Err(
            "This volume has no Recycle Bin, so deleting would be permanent — refusing."
                .to_string(),
        );
    }

    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    let ops = RealFileOps;
    ops.recycle(&path)?;

    // A `.disabled` marker or loose preview image left behind would be orphaned;
    // failing to recycle them is not worth failing the delete over.
    let sidecar = disabled_sidecar(&path);
    if sidecar.exists() {
        let _ = ops.recycle(&sidecar);
    }
    for img in image_sidecars(&path) {
        let _ = ops.recycle(&img);
    }

    invalidate_var_packages_cache(&state);
    Ok(size)
}

/// Finds an existing case-insensitive `<root>/<folder>` directory (so `qing.*`
/// reuses an existing `Qing/`), else returns `<root>/<folder>` to be created.
pub(crate) fn existing_creator_dir(root: &Path, folder: &str) -> PathBuf {
    if let Ok(entries) = std::fs::read_dir(root) {
        let want = folder.to_ascii_lowercase();
        for entry in entries.flatten() {
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false)
                && entry.file_name().to_string_lossy().to_ascii_lowercase() == want
            {
                return root.join(entry.file_name());
            }
        }
    }
    root.join(folder)
}

/// Where "Move to creator folder" files a package: `<root_dir>/<Creator>/<file
/// name>`, reusing an existing creator folder whatever its casing. `root_dir`
/// is the Settings VAR library folder; additional folders are intentionally
/// not used.
///
/// Validates everything up front and touches nothing, so Collect Dependencies
/// can learn the destination (to show it, and to write `deps/` beside it)
/// before anything moves.
pub(crate) fn creator_folder_destination(src: &Path, root_dir: &str) -> Result<PathBuf, String> {
    let is_var = src
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("var"))
        .unwrap_or(false);
    if !is_var {
        return Err("Not a .var file.".to_string());
    }
    if !src.is_file() {
        return Err("That package is already gone.".to_string());
    }

    let root = PathBuf::from(root_dir.trim());
    if root.as_os_str().is_empty() {
        return Err(
            "Set your VAR library (AddonPackages root) folder in Settings first.".to_string(),
        );
    }
    if !root.is_dir() {
        return Err(format!(
            "The VAR library folder does not exist: {}",
            root.display()
        ));
    }

    let stem = src
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| "Could not read the package name.".to_string())?;
    let creator = naming::creator_from_package_id(stem)
        .ok_or_else(|| "No creator in the package name — nothing to organize.".to_string())?;
    let folder = naming::sanitize_creator_folder(creator)
        .ok_or_else(|| format!("Creator '{creator}' can't be used as a folder name."))?;

    let file_name = src
        .file_name()
        .ok_or_else(|| "Bad file name.".to_string())?;
    Ok(existing_creator_dir(&root, &folder).join(file_name))
}

/// True when two spellings name the same file. A path from a disk walk and one
/// built from the Settings root string can differ in separator or
/// directory-name case for the very same NTFS file, so `==` is not enough.
pub(crate) fn same_path_spelling(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().replace('/', "\\").to_ascii_lowercase();
    norm(a) == norm(b)
}

/// Same backstop the bulk executor uses: never relocate a package out of the
/// folder VAM scans. A misconfigured library root — e.g. the AddonPackages
/// parent instead of AddonPackages itself — would otherwise silently make VAM
/// stop loading it, with nothing in the Recycle Bin to restore.
pub(crate) const OUT_OF_ADDON_PACKAGES: &str =
    "Refusing to move this package out of AddonPackages — VAM would stop loading it. \
     Point your VAR library folder at your AddonPackages folder in Settings.";

/// Moves one `.var` to `dest`, creating the folder if missing. Its `.disabled`
/// marker and loose preview images travel with it. Refuses to overwrite an
/// existing destination.
///
/// The package itself either moves or the call fails; a sidecar that cannot
/// follow is not worth failing over, so each one comes back as a note.
pub(crate) fn move_package_with_sidecars(
    ops: &dyn FileOps,
    src: &Path,
    dest: &Path,
) -> Result<Vec<String>, String> {
    if is_in_addon_packages(src) && !is_in_addon_packages(dest) {
        return Err(OUT_OF_ADDON_PACKAGES.to_string());
    }

    // fs::rename is MoveFileExW with REPLACE_EXISTING — never overwrite.
    if dest.exists() {
        return Err("Something is already at the destination.".to_string());
    }

    // rename() create_dir_all's the parent, so the creator folder is made here.
    ops.rename(src, dest)?;

    let mut notes = Vec::new();
    let sidecar = disabled_sidecar(src);
    if sidecar.exists() {
        if let Err(err) = ops.rename(&sidecar, &disabled_sidecar(dest)) {
            notes.push(format!("its .disabled marker stayed behind: {err}"));
        }
    }
    if let Some(dest_parent) = dest.parent() {
        for img in image_sidecars(src) {
            let Some(name) = img.file_name() else { continue };
            let img_dest = dest_parent.join(name);
            if img_dest.exists() {
                notes.push(
                    "its preview image was left behind — one already exists at the destination"
                        .to_string(),
                );
                continue;
            }
            if let Err(err) = ops.rename(&img, &img_dest) {
                notes.push(format!("its preview image stayed behind: {err}"));
            }
        }
    }
    Ok(notes)
}

/// Moves a single `.var` into `<root_dir>/<Creator>/`. Returns the new path.
#[tauri::command]
pub(crate) fn move_var_to_creator_folder(
    file_path: String,
    root_dir: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let src = PathBuf::from(&file_path);
    let dest = creator_folder_destination(&src, &root_dir)?;

    // Already filed under its creator folder — nothing to do.
    if same_path_spelling(&dest, &src) {
        return Ok(dest.to_string_lossy().to_string());
    }

    move_package_with_sidecars(&RealFileOps, &src, &dest)?;

    invalidate_var_packages_cache(&state);
    Ok(dest.to_string_lossy().to_string())
}

fn resolve_roots(input_dir: &str, additional: Option<Vec<String>>) -> Result<Vec<PathBuf>, String> {
    let dir = Path::new(input_dir);
    if !dir.is_dir() {
        return Err(format!("not a directory: {}", dir.display()));
    }
    let extra: Vec<PathBuf> = additional
        .unwrap_or_default()
        .iter()
        .map(|d| d.trim())
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .collect();
    Ok(crate::scan::scan_roots(dir, &extra))
}

/// Walks the roots and builds candidates, skipping this app's own backup trees.
///
/// `read_refs` is only set for Clean Duplicates: Organize needs no archive
/// reads at all, and a full-library decompress is minutes on a big collection.
fn gather_candidates(
    roots: &[PathBuf],
    known_ids: &HashSet<String>,
    read_refs: Option<bool>,
    depth: ScanDepth,
    tasks: &Arc<Mutex<HashMap<u64, ProgressPayload>>>,
    task_id: u64,
    phase: &str,
    cancel: &Arc<AtomicBool>,
) -> Result<
    (
        Vec<PackageCandidate>,
        HashMap<PathBuf, Result<VarRefs, String>>,
        Vec<String>,
    ),
    String,
> {
    // ALWAYS recursive, whatever the depth. Normal mode narrows what may be
    // ACTED ON (`in_scope` below), never what the dependency check can see —
    // VAM loads a subfolder scene regardless of how the user chose to browse, so
    // its reference to a top-level older version must still protect it.
    let found = crate::utils::collect_var_files_by_root(roots).map_err(|e| e.to_string())?;

    let mut skipped_managed = 0usize;
    let kept: Vec<(PathBuf, crate::models::VarFileEntry)> = found
        .into_iter()
        .filter(|(_root, entry)| {
            if is_app_managed_path(&entry.path) {
                skipped_managed += 1;
                false
            } else {
                true
            }
        })
        .collect();

    let mut notes = Vec::new();
    if skipped_managed > 0 {
        notes.push(format!(
            "Ignored {skipped_managed} file(s) inside this app's own backup folders ({}).",
            APP_MANAGED_DIRS.join(", "),
        ));
    }

    let out_of_scope = kept
        .iter()
        .filter(|(root, entry)| !scope_allows(depth, root, &entry.path))
        .count();
    if let Some(note) = out_of_scope_note(out_of_scope) {
        notes.push(note);
    }

    let mut refs: HashMap<PathBuf, Result<VarRefs, String>> = HashMap::new();
    if let Some(deep) = read_refs {
        set_task_progress(tasks, task_id, phase, 0.05, "Reading package dependencies");
        let done = AtomicUsize::new(0);
        let total = kept.len().max(1);
        let collected: Vec<(PathBuf, Result<VarRefs, String>)> = kept
            .par_iter()
            .map(|(_root, entry)| {
                if cancel.load(Ordering::SeqCst) {
                    return (entry.path.clone(), Err("cancelled".to_string()));
                }
                let result = read_var_refs(&entry.path, deep);
                let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                if n % 25 == 0 || n == total {
                    set_task_progress(
                        tasks,
                        task_id,
                        phase,
                        0.05 + 0.85 * (n as f64 / total as f64),
                        format!("Reading dependencies ({n}/{total})"),
                    );
                }
                (entry.path.clone(), result)
            })
            .collect();
        refs.extend(collected);
    }

    let candidates = kept
        .iter()
        .map(|(root, entry)| {
            let readable = refs
                .get(&entry.path)
                .map(|r| r.is_ok())
                // Organize does not read refs, so nothing is "unreadable" there.
                .unwrap_or(true);
            build_candidate(
                root,
                &entry.path,
                entry.fingerprint.size,
                entry.fingerprint.modified_ns,
                known_ids.contains(
                    entry
                        .path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or_default(),
                ),
                readable,
                scope_allows(depth, root, &entry.path),
            )
        })
        .collect();

    Ok((candidates, refs, notes))
}

/// Stores the plan server-side and stamps it with the id the frontend echoes
/// back on apply.
fn store_plan(
    state_plan: &Arc<Mutex<Option<PackagePlan>>>,
    plan_id: u64,
    mut response: PackageOpResponse,
    candidates: Vec<PackageCandidate>,
    refs: HashMap<PathBuf, Result<VarRefs, String>>,
) -> Result<PackageOpResponse, String> {
    response.plan_id = plan_id;
    let plan = PackagePlan {
        plan_id,
        actions: response.actions.clone(),
        candidates,
        refs,
    };
    *state_plan
        .lock()
        .map_err(|_| "plan store poisoned".to_string())? = Some(plan);
    Ok(response)
}

#[tauri::command]
pub(crate) fn start_plan_clean_duplicates_task(
    input_dir: String,
    additional_input_dirs: Option<Vec<String>>,
    deep_payload_scan: Option<bool>,
    deep_scan: Option<bool>,
    state: State<'_, AppState>,
    db: State<'_, Db>,
) -> Result<TaskHandle, String> {
    let roots = resolve_roots(&input_dir, additional_input_dirs)?;
    let (task_id, tasks, cancel) =
        begin_task(&state, "clean_dupes_starting", "Scanning for duplicates")?;
    let plan_store = Arc::clone(&state.package_plan);
    let probe = Arc::clone(&state.recycle_support);
    let db = db.inner().clone();
    // Two independent "deep" knobs, easy to confuse:
    //  - deep_payload_scan: read text payloads inside each .var for refs.
    //  - deep_scan:         the page's folder depth (walk subfolders or not).
    let deep_payload = deep_payload_scan.unwrap_or(true);
    let depth = ScanDepth::from_deep(deep_scan.unwrap_or(true));

    thread::spawn(move || {
        let result = (|| -> Result<PackageOpResponse, String> {
            let known_ids = {
                let conn = db.read().map_err(|e| e.to_string())?;
                load_known_package_ids(&conn)?
            };
            let (candidates, refs, mut notes) = gather_candidates(
                &roots,
                &known_ids,
                Some(deep_payload),
                depth,
                &tasks,
                task_id,
                "clean_dupes_reading",
                &cancel,
            )?;
            if cancel.load(Ordering::SeqCst) {
                return Ok(PackageOpResponse {
                    kind: "clean_duplicates".to_string(),
                    was_cancelled: true,
                    ..Default::default()
                });
            }

            set_task_progress(&tasks, task_id, "clean_dupes_planning", 0.92, "Planning");
            let mut response =
                plan_clean_duplicates(&candidates, &refs, &|p| recycle_support_for(&probe, p));
            notes.append(&mut response.notes);
            response.notes = notes;
            response.deep_scan_used = deep_payload;

            store_plan(&plan_store, task_id, response, candidates, refs)
        })();
        finish_task(&tasks, task_id, "clean_dupes", result);
    });

    Ok(TaskHandle { id: task_id })
}

#[tauri::command]
pub(crate) fn start_plan_organize_by_creator_task(
    input_dir: String,
    additional_input_dirs: Option<Vec<String>>,
    deep_scan: Option<bool>,
    dest_root: Option<String>,
    state: State<'_, AppState>,
    db: State<'_, Db>,
) -> Result<TaskHandle, String> {
    let roots = resolve_roots(&input_dir, additional_input_dirs)?;
    // When set (the Settings library / AddonPackages root), every creator folder
    // is created directly under it instead of each file's own AddonPackages
    // ancestor. Empty → keep the original per-file behavior.
    let dest_root: Option<PathBuf> = dest_root
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from);
    let (task_id, tasks, cancel) =
        begin_task(&state, "organize_starting", "Scanning packages")?;
    let plan_store = Arc::clone(&state.package_plan);
    let db = db.inner().clone();
    let depth = ScanDepth::from_deep(deep_scan.unwrap_or(true));

    thread::spawn(move || {
        let result = (|| -> Result<PackageOpResponse, String> {
            let known_ids = {
                let conn = db.read().map_err(|e| e.to_string())?;
                load_known_package_ids(&conn)?
            };
            let (candidates, refs, mut notes) = gather_candidates(
                &roots,
                &known_ids,
                None,
                depth,
                &tasks,
                task_id,
                "organize_reading",
                &cancel,
            )?;
            if cancel.load(Ordering::SeqCst) {
                return Ok(PackageOpResponse {
                    kind: "organize_by_creator".to_string(),
                    was_cancelled: true,
                    ..Default::default()
                });
            }

            set_task_progress(&tasks, task_id, "organize_planning", 0.9, "Planning");
            let mut response = plan_organize_by_creator_into(&candidates, dest_root.as_deref());
            notes.append(&mut response.notes);
            response.notes = notes;

            store_plan(&plan_store, task_id, response, candidates, refs)
        })();
        finish_task(&tasks, task_id, "organize", result);
    });

    Ok(TaskHandle { id: task_id })
}

/// Applies the stored plan.
///
/// Deliberately takes **no `db` handle**. The project rule is that this path is
/// read-only against the database, and omitting the handle makes a later
/// careless edit unable to violate it. Leaving a row whose file was recycled is
/// correct: `package_id` is the PRIMARY KEY and *is* the file stem, so the row
/// remains the missing-resource recovery reference the rule exists to protect —
/// deleting it would destroy exactly what the rule guards. A move likewise
/// preserves identity; only `file_path` goes stale, and the UI offers a rebuild.
#[tauri::command]
pub(crate) fn start_apply_package_plan_task(
    plan_id: u64,
    selected: Vec<usize>,
    forced: Vec<usize>,
    state: State<'_, AppState>,
) -> Result<TaskHandle, String> {
    let (task_id, tasks, cancel) = begin_task(&state, "apply_plan_starting", "Applying")?;
    let plan_store = Arc::clone(&state.package_plan);
    let folder_cache = Arc::clone(&state.var_packages_folder_cache);
    let generation = Arc::clone(&state.var_packages_cache_generation);

    thread::spawn(move || {
        let result = run_apply_plan(
            &plan_store,
            plan_id,
            &selected,
            &forced,
            &tasks,
            task_id,
            &cancel,
        );

        // Every exit path — success, cancel, and error — must invalidate, or the
        // grid keeps rendering files that are now in the Recycle Bin. (Inlined
        // rather than calling invalidate_var_packages_cache: AppState cannot
        // cross the thread boundary, so the worker holds the two Arcs directly.)
        generation.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut guard) = folder_cache.lock() {
            *guard = None;
        }
        // NOTE: deliberately NOT clearing state.scan_cache. It is self-validating
        // (load_cached_scan re-walks and compares fingerprints, which both trash
        // and organize necessarily perturb), and its only production writer is
        // the Overview scan — so clearing it here could only throw away another
        // page's minutes of hashing for no benefit.
        if let Ok(mut guard) = plan_store.lock() {
            *guard = None;
        }

        finish_task(&tasks, task_id, "apply_plan", result);
    });

    Ok(TaskHandle { id: task_id })
}

fn run_apply_plan(
    plan_store: &Arc<Mutex<Option<PackagePlan>>>,
    plan_id: u64,
    selected: &[usize],
    forced: &[usize],
    tasks: &Arc<Mutex<HashMap<u64, ProgressPayload>>>,
    task_id: u64,
    cancel: &Arc<AtomicBool>,
) -> Result<PackageOpResponse, String> {
    let plan = {
        let mut guard = plan_store.lock().map_err(|_| "plan store poisoned".to_string())?;
        guard.take().ok_or_else(|| {
            "That plan is no longer available — re-run the scan.".to_string()
        })?
    };
    if plan.plan_id != plan_id {
        return Err("That plan is out of date — re-run the scan.".to_string());
    }
    if let Some(bad) = selected.iter().find(|i| **i >= plan.actions.len()) {
        return Err(format!("invalid selection index {bad}"));
    }

    let selected_set: HashSet<usize> = selected.iter().copied().collect();
    let forced_set: HashSet<usize> = forced.iter().copied().collect();

    // action.file_path -> candidate index, built once. A linear scan per action
    // would be O(actions x candidates) — ~100M comparisons for an organize over
    // a 10k-package library, all before the first file moves.
    let index_of: HashMap<&str, usize> = plan
        .candidates
        .iter()
        .enumerate()
        .filter_map(|(pos, c)| c.path.to_str().map(|p| (p, pos)))
        .collect();

    // Recompute protection against what the user ACTUALLY kept. Unticking a row
    // changes the survivor set, so a plan computed against the original
    // selection can be wrong: leaving `Old.1` (which needs `Tex.1`) while still
    // recycling `Tex.1` would break the very package they chose to keep. This is
    // pure set math over data already in memory — no IO.
    let removed_now: HashSet<usize> = selected_set
        .iter()
        .filter(|i| plan.actions[**i].op == "trash")
        .filter_map(|i| index_of.get(plan.actions[*i].file_path.as_str()).copied())
        .collect();

    // Only `newly_protected` is consulted below: the returned removal set is the
    // complement of it over `removed_now`, and the per-action loop has to walk
    // every action anyway to report the ones the user left unticked.
    let (_removed, newly_protected) =
        compute_protection(&plan.candidates, &plan.refs, removed_now);

    let mut actions: Vec<PackageAction> = Vec::new();
    for (i, action) in plan.actions.iter().enumerate() {
        let mut action = action.clone();
        if !selected_set.contains(&i) {
            action.status = "skipped".to_string();
            action.detail = "not selected".to_string();
            actions.push(action);
            continue;
        }
        if let Some(pos) = index_of.get(action.file_path.as_str()) {
            if let Some(referrer) = newly_protected.get(pos) {
                if !forced_set.contains(&i) {
                    action.status = "skipped".to_string();
                    action.detail =
                        format!("now referenced by {referrer}, which you kept");
                    actions.push(action);
                    continue;
                }
            }
        }
        // Selecting a row IS the opt-in, whatever the planner labelled it.
        //
        // Normalize every selected survivor to "pending", not just `forced`
        // ones: the executor gates on `status != "pending"`, so leaving a row as
        // "unverified" (or "protected" that the recompute has since cleared)
        // would make ticking it a silent no-op. `blocked` is the one status the
        // user cannot override — recycling there would be a permanent delete —
        // and the checkbox is disabled for it, so this is belt and braces.
        if action.status != "blocked" {
            action.status = "pending".to_string();
        }
        actions.push(action);
    }

    let mut progress = |fraction: f64, message: &str| {
        set_task_progress(tasks, task_id, "apply_plan_running", fraction, message);
    };
    let ops = RealFileOps;
    let (reclaimed, was_cancelled) =
        run_apply_package_actions(&mut actions, &ops, cancel, &mut progress);

    Ok(PackageOpResponse {
        plan_id,
        kind: "apply".to_string(),
        scanned: plan.actions.len() as u64,
        unchanged: 0,
        notes: Vec::new(),
        reclaimable_bytes: reclaimed,
        deep_scan_used: false,
        protection_complete: true,
        was_cancelled,
        actions,
    })
}
