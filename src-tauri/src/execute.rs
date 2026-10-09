use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use walkdir::WalkDir;
use zip::{write::SimpleFileOptions, CompressionMethod, System, ZipArchive, ZipWriter};

use crate::{
    db::{self, Db},
    models::{
        ExecuteRequest, ExecuteResponse, ExecutionStats, PreparedPackage, ResourceRef,
        ScannedData, KEEP_ALL_VALUE, META_PATH, TEXT_EXTENSIONS,
    },
    scan::scan_directory_with_target_with_progress,
    utils::{decode_text, dump_json_bytes, encode_text, normalize_zip_path},
};

pub(crate) fn paired_support_paths(path: &str) -> Vec<String> {
    let source = Path::new(path);
    let Some(extension) = source.extension().and_then(|ext| ext.to_str()) else {
        return Vec::new();
    };

    let normalized_extension = extension.to_ascii_lowercase();
    let support_extensions: &[&str] = match normalized_extension.as_str() {
        "vam" => &["vaj", "vab"],
        "vmi" => &["vmb"],
        _ => return Vec::new(),
    };

    let Some(stem) = source.file_stem().and_then(|stem| stem.to_str()) else {
        return Vec::new();
    };
    let parent = source.parent();
    let build = |new_ext: &str| {
        let file_name = format!("{stem}.{new_ext}");
        match parent {
            Some(parent) if !parent.as_os_str().is_empty() => parent.join(file_name),
            _ => PathBuf::from(file_name),
        }
        .to_string_lossy()
        .replace('\\', "/")
    };

    support_extensions
        .iter()
        .map(|new_ext| build(new_ext))
        .chain(
            (normalized_extension == "vam")
                .then_some(["png", "jpg", "jpeg"])
                .into_iter()
                .flatten()
                .map(build),
        )
        .collect()
}

/// Path of the metadata sibling a Unity binary cache attaches to:
/// `<stem>.vab` → `<stem>.vam`, `<stem>.vmb` → `<stem>.vmi`. Returns `None`
/// for any other extension. VAM/RG loads `.vab`/`.vmb` by stem-matching next
/// to their descriptor at runtime — there's no JSON reference to redirect,
/// so removing one when its sibling stays in source silently breaks loading.
pub(crate) fn binary_cache_sibling_descriptor(path: &str) -> Option<String> {
    let p = Path::new(path);
    let ext = p.extension().and_then(|e| e.to_str())?.to_ascii_lowercase();
    let sibling_ext = match ext.as_str() {
        "vab" => "vam",
        "vmb" => "vmi",
        _ => return None,
    };
    let stem = p.file_stem().and_then(|s| s.to_str())?;
    let parent = p.parent();
    let file_name = format!("{stem}.{sibling_ext}");
    let out = match parent {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(file_name),
        _ => PathBuf::from(file_name),
    };
    Some(out.to_string_lossy().replace('\\', "/"))
}

fn add_removed_path_if_present(package: &mut PreparedPackage, internal_path: &str) {
    let exists = package
        .resource_refs
        .iter()
        .any(|resource| resource.internal_path == internal_path);
    if exists {
        package.removed_paths.insert(internal_path.to_string());
    }
}

// Splits a keep_map value of the form "package_id:internal_path" into its
// components. Returns None for the "keep all" sentinel or malformed inputs.
fn parse_keep_key(keep_key: &str) -> Option<(&str, &str)> {
    if keep_key == KEEP_ALL_VALUE {
        return None;
    }
    keep_key.split_once(':')
}

// Resolves a `keep_value` to a `ResourceRef` *by trusting the user's pick*.
// Lookup order is best-effort enrichment, not validation:
//   1. `ref_index` — a hit gives the full local `ResourceRef` (correct
//      `package_file`, `crc32`, `size`), which makes the equality check in
//      the main loop work for "keep the local copy from VAR X" choices.
//   2. The indexed catalog — a hit gives the DB-recorded `package_file` /
//      `crc32` / `size`.
//   3. Stub built from `parse_keep_key` — last resort. Carries only
//      `package_id` + `internal_path`, which is everything the dedup
//      transformation actually needs (replacement_map → `package_ref()`
//      drives the text rewrite; `package_id` populates meta.json deps).
//
// Existence is the scanner's / DB-search's responsibility, not this step's.
// If the user picked something that doesn't materialize anywhere, that's a
// surface-layer bug and not something we should turn into a fatal dedup
// failure here — we trust the pick and let the rewrite proceed.
// Mutates `ref_index` so the same keep_value seen again is a free lookup.
// Returns None only when `keep_value` is unparseable (no `:` separator, or
// the `KEEP_ALL` sentinel — those callers already screen out).
fn resolve_keep_value(
    keep_value: &str,
    ref_index: &mut HashMap<String, ResourceRef>,
    db: Option<&Db>,
) -> Option<ResourceRef> {
    if let Some(r) = ref_index.get(keep_value).cloned() {
        return Some(r);
    }
    let (pid, raw_path) = parse_keep_key(keep_value)?;
    let path = raw_path.trim_start_matches('/');
    let from_db = db
        .and_then(|db| db.read().ok())
        .and_then(|conn| {
            db::load_resource_by_pid_and_path(&conn, pid, path)
                .ok()
                .flatten()
        });
    let resolved = from_db.unwrap_or_else(|| ResourceRef {
        package_id: pid.to_string(),
        package_file: String::new(),
        internal_path: path.to_string(),
        crc32: None,
        size: 0,
        effective_size: 0,
    });
    ref_index.insert(keep_value.to_string(), resolved.clone());
    Some(resolved)
}

pub(crate) fn prepare_package_changes(
    scanned: &mut ScannedData,
    keep_map: &BTreeMap<String, String>,
    target_package_id: Option<&str>,
    db: Option<&Db>,
) -> Result<usize> {
    for package in scanned.packages.values_mut() {
        package.removed_paths.clear();
        package.required_dependencies.clear();
        package.replacement_map.clear();
    }

    if let Some(target_package_id) = target_package_id {
        if !scanned.packages.contains_key(target_package_id) {
            bail!("Target package was not found in scan result: {target_package_id}");
        }
    }

    let mut ref_index = HashMap::new();
    for group in &scanned.duplicate_groups {
        for item in &group.refs {
            ref_index.insert(item.display_name(), item.clone());
            ref_index.insert(item.package_ref(), item.clone());
        }
    }

    // First pass: walk every group, resolve keep choices, and collect the
    // *intended* per-package removals as a flat plan. We don't apply anything
    // to `scanned.packages` yet — the second pass needs cross-removal context
    // (whether a sibling .vam/.vmi is also being removed) to decide what's
    // actually safe to apply.
    let mut applicable_groups = 0;
    let mut plan: HashMap<String, Vec<(String, ResourceRef)>> = HashMap::new();
    for group in &scanned.duplicate_groups {
        if let Some(target_package_id) = target_package_id {
            if !group
                .refs
                .iter()
                .any(|item| item.package_id == target_package_id)
            {
                continue;
            }
        }
        applicable_groups += 1;

        let keep_key = keep_map
            .get(&group.key)
            .cloned()
            .or_else(|| group.refs.first().map(ResourceRef::display_name))
            .ok_or_else(|| anyhow!("duplicate group {} has no resources", group.key))?;

        if keep_key == KEEP_ALL_VALUE {
            continue;
        }

        // Trust the user's selection. The dedup step is a pure transformation
        // — it removes the resource from this VAR and rewrites refs to point
        // at whatever the user picked. Existence/identity of the chosen ref
        // is the scanner's and DB-search's responsibility, not this step's.
        // resolve_keep_value falls back to a stub when neither ref_index nor
        // the DB know the pick, so this only returns None for malformed input
        // (no `:` separator) — that's a UI bug and we surface it as such.
        let short_key = group.key.as_str();
        let kept_ref = resolve_keep_value(&keep_key, &mut ref_index, db).ok_or_else(|| {
            anyhow!(
                "Malformed keep choice for duplicate group {}: {}",
                short_key,
                keep_key
            )
        })?;

        for item in &group.refs {
            if let Some(target_package_id) = target_package_id {
                if item.package_id != target_package_id {
                    continue;
                }
            }
            // Compare on identity (package_id + internal_path), not full
            // structural equality — when the user picks "the local copy
            // already in this VAR", resolve_keep_value may have given us a
            // stub (no crc/size) while the group's `item` carries the full
            // scan-time ResourceRef. Structural `==` would miss the match
            // and we'd nuke the user's keep.
            if item.package_id == kept_ref.package_id
                && item.internal_path == kept_ref.internal_path
            {
                continue;
            }
            plan
                .entry(item.package_id.clone())
                .or_default()
                .push((item.internal_path.clone(), kept_ref.clone()));
        }
    }

    // Synthetic dbfind pass: the Find Duplicates page (DB mode) injects a
    // virtual group for every target-VAR resource that the local scan didn't
    // already cluster as a duplicate, keyed `dbfind|<target_pid>|<path>`.
    // These groups exist only in the frontend's state, so they never appear
    // in `scanned.duplicate_groups` above — but their keep_map entries do
    // ride along in the execute request. If the user picked a DB row as the
    // keep, we need to fold the relocation into the same `plan` map so the
    // second/third passes (sibling suppression, bundle cascade) handle it
    // identically to a real-group relocation.
    for (keep_key, keep_value) in keep_map {
        if !keep_key.starts_with("dbfind|") {
            continue;
        }
        if keep_value == KEEP_ALL_VALUE {
            continue;
        }
        // splitn(3) so `internal_path` may itself contain `|`.
        let mut parts = keep_key.splitn(3, '|');
        let _prefix = parts.next();
        let Some(synthetic_pid) = parts.next() else {
            continue;
        };
        let Some(synthetic_path) = parts.next() else {
            continue;
        };
        // Default keep value for a synthetic group is the target VAR's own
        // ref (`<pid>:<path>` from getRefValue in app.js) — that's "leave it
        // alone", no removal required.
        let default_value = format!("{synthetic_pid}:{synthetic_path}");
        if keep_value == &default_value {
            continue;
        }
        if let Some(target_package_id) = target_package_id {
            if synthetic_pid != target_package_id {
                continue;
            }
        }
        let kept_ref =
            resolve_keep_value(keep_value, &mut ref_index, db).ok_or_else(|| {
                anyhow!(
                    "Malformed keep choice for duplicate group {}: {}",
                    keep_key,
                    keep_value
                )
            })?;
        applicable_groups += 1;
        plan.entry(synthetic_pid.to_string())
            .or_default()
            .push((synthetic_path.to_string(), kept_ref));
    }

    // Second pass: apply the plan, but suppress any `.vab`/`.vmb` removal
    // whose sibling `.vam`/`.vmi` still exists in the source package and is
    // NOT itself in the removal plan. These caches are sibling-loaded by VAM
    // at runtime (no JSON reference to redirect) — orphaning them from a
    // local `.vam` that stays put silently breaks hair/morph loading. Common
    // failure mode: a near-empty `.vab` happens to share a CRC32 with some
    // unrelated package's `.vab`, the user picks that DB target as a Find
    // Duplicates relocation, and their local `.vam`'s cache vanishes.
    //
    // No cascade-removal in the other direction either: paired support files
    // (`.vaj`, textures, etc.) are never auto-removed because the parent
    // `.vam` was deduped. The user's invariant is "only remove what I
    // explicitly picked".
    for (package_id, items) in plan {
        let removal_set: BTreeSet<String> = items.iter().map(|(p, _)| p.clone()).collect();
        let Some(package) = scanned.packages.get_mut(&package_id) else {
            continue;
        };
        for (item_path, kept_ref) in items {
            if let Some(sibling_path) = binary_cache_sibling_descriptor(&item_path) {
                let sibling_present = package
                    .resource_refs
                    .iter()
                    .any(|r| r.internal_path == sibling_path);
                let sibling_being_removed = removal_set.contains(&sibling_path);
                if sibling_present && !sibling_being_removed {
                    // Leave the cache alone — its metadata sibling stays in
                    // this VAR and would lose its Unity binary at runtime.
                    continue;
                }
            }
            add_removed_path_if_present(package, &item_path);
            package
                .replacement_map
                .insert(item_path, kept_ref.clone());
            if kept_ref.package_id != package_id {
                package
                    .required_dependencies
                    .insert(kept_ref.package_id.clone());
            }
        }
    }

    // Third pass: cascade `.vam`/`.vmi` removal to the rest of its asset
    // bundle. The scene only names the `.vam` directly; its
    // `.vaj`/`.vab`/same-stem `.png/.jpg/.jpeg` are stem-loaded by VAM, and
    // any `customTexture_*` paths inside the `.vaj` are resolved relative to
    // the `.vaj`'s folder. Once we redirect the `.vam` ref to a source VAR,
    // those sibling files are dead bytes in the target and must follow it
    // out. Reference-counted against surviving `.vam`/`.vmi` in the same
    // package so a shared texture stays.
    //
    // Also enforces the symmetric atomicity rule: a bundle member can only
    // be removed if its parent `.vam`/`.vmi` is also being removed. If the
    // first/second pass marked a `.vaj` alone for removal (e.g. user picked
    // a non-.vam keep target from a multi-extension duplicate group), we
    // drop it here.
    apply_bundle_cascade(&mut scanned.packages);

    Ok(applicable_groups)
}

/// Cascade rule for the asset-bundle invariant.
///
/// Reads the scan-time `support_paths` + `sibling_owners` maps off each
/// `PreparedPackage` and does two passes on `removed_paths`:
///   1. For each `.vam`/`.vmi` already queued for removal, add its bundle
///      siblings — but only when *every* parent that pulls each sibling in
///      is also being removed. Shared textures (siblings with multiple
///      parents, e.g. an alpha map used by two different clothing items in
///      the same VAR) stay if any parent stays. The shared/exclusive
///      classification lives in `sibling_owners`, built at scan time, so no
///      classification work happens here.
///   2. Atomicity: drop any bundle-member path already in `removed_paths`
///      whose parent `.vam`/`.vmi` is present and not being removed. This is
///      the `feedback_vab_sibling_invariant` rule extended from `.vab/.vmb`
///      to the whole bundle — never strand a parent's siblings.
fn apply_bundle_cascade(packages: &mut BTreeMap<String, PreparedPackage>) {
    for package in packages.values_mut() {
        if package.removed_paths.is_empty() {
            continue;
        }

        let present_paths: BTreeSet<String> = package
            .resource_refs
            .iter()
            .map(|r| r.internal_path.clone())
            .collect();

        // (1) Cascade siblings of removed parents into the removal set.
        let removed_parents: Vec<String> = package
            .removed_paths
            .iter()
            .filter(|path| is_bundle_parent_path(path))
            .cloned()
            .collect();
        let package_id = package.package_id.clone();
        for parent in &removed_parents {
            let siblings = match package.support_paths.get(parent) {
                Some(siblings) => siblings.clone(),
                None => continue,
            };
            // The kept_ref for the parent .vam was inserted by pass 2. Reuse
            // its package_id for each cascaded sibling so scene-level refs
            // (e.g. `customTexture_AlphaTex: "SELF:/.../lipsA.png"`) get
            // redirected to the source VAR alongside the .vam ref itself.
            let parent_kept_ref = package.replacement_map.get(parent).cloned();
            for sibling in &siblings {
                if !present_paths.contains(sibling) {
                    continue;
                }
                if package.removed_paths.contains(sibling) {
                    continue;
                }
                // Reference-count via scan-time inverse map: keep the
                // sibling if any of its owners is staying put.
                let kept_owner_exists = package
                    .sibling_owners
                    .get(sibling)
                    .map(|owners| {
                        owners.iter().any(|owner| {
                            present_paths.contains(owner)
                                && !package.removed_paths.contains(owner)
                        })
                    })
                    .unwrap_or(false);
                if kept_owner_exists {
                    continue;
                }
                package.removed_paths.insert(sibling.clone());
                // Synthesize a redirect to the same source package, but at
                // the sibling's own path. Sizes/crc are unused by
                // `rewrite_text_payload` (it only calls `package_ref()`), so
                // leave them zeroed — VAM's stem/relative-path loader
                // resolves the file from the source VAR's matching folder.
                if let Some(parent_kept_ref) = &parent_kept_ref {
                    let sibling_kept_ref = ResourceRef {
                        package_id: parent_kept_ref.package_id.clone(),
                        package_file: parent_kept_ref.package_file.clone(),
                        internal_path: sibling.clone(),
                        crc32: None,
                        size: 0,
                        effective_size: 0,
                    };
                    package
                        .replacement_map
                        .insert(sibling.clone(), sibling_kept_ref.clone());
                    if sibling_kept_ref.package_id != package_id {
                        package
                            .required_dependencies
                            .insert(sibling_kept_ref.package_id);
                    }
                }
            }
        }

        // (2) Atomicity: drop bundle members whose parent stays.
        let candidates: Vec<String> = package
            .removed_paths
            .iter()
            .filter(|path| !is_bundle_parent_path(path))
            .cloned()
            .collect();
        let mut atomicity_dropped = false;
        for member in candidates {
            let Some(parents) = package.sibling_owners.get(&member) else {
                continue;
            };
            let any_parent_present_and_kept = parents.iter().any(|parent| {
                present_paths.contains(parent) && !package.removed_paths.contains(parent)
            });
            if any_parent_present_and_kept {
                package.removed_paths.remove(&member);
                // The replacement_map entry (if any) would have rewritten
                // this sibling's textual refs in scene.json / .vap files.
                // The file stays put, so drop that rewrite directive too.
                package.replacement_map.remove(&member);
                atomicity_dropped = true;
            }
        }

        // Pass 2 added kept_ref.package_id to required_dependencies for
        // every entry it wrote into replacement_map. If atomicity just
        // removed some of those replacement entries, recompute the
        // dependency set from the surviving replacements — otherwise we
        // leave a stale meta.json dep and rewrite a package that didn't
        // actually change content.
        if atomicity_dropped {
            let surviving: BTreeSet<String> = package
                .replacement_map
                .values()
                .filter(|kept| kept.package_id != package.package_id)
                .map(|kept| kept.package_id.clone())
                .collect();
            package.required_dependencies.retain(|dep| surviving.contains(dep));
        }
    }
}

fn is_bundle_parent_path(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| {
            let lower = ext.to_ascii_lowercase();
            lower == "vam" || lower == "vmi"
        })
        .unwrap_or(false)
}

// VAM accepts `Creator.Name.latest:` as a self-reference that resolves to the
// highest installed version at runtime. We rewrite that form alongside the
// explicit `Creator.Name.<n>:` form — otherwise a scene that references its
// own textures through `.latest:` still points at files we just removed.
fn latest_package_id(package_id: &str) -> Option<String> {
    let last_dot = package_id.rfind('.')?;
    if last_dot == 0 || last_dot + 1 >= package_id.len() {
        return None;
    }
    let prefix = &package_id[..last_dot];
    let version = &package_id[last_dot + 1..];
    if version == "latest" {
        return None;
    }
    Some(format!("{prefix}.latest"))
}

fn rewrite_text_payload(package: &PreparedPackage, raw: &[u8]) -> Vec<u8> {
    let Some((text, encoding)) = decode_text(raw) else {
        return raw.to_vec();
    };

    let latest_id = latest_package_id(&package.package_id);
    let mut updated = text.clone();
    for (old_path, kept_ref) in &package.replacement_map {
        let old_self = format!("SELF:/{old_path}");
        let old_explicit = format!("{}:/{}", package.package_id, old_path);
        let new_ref = kept_ref.package_ref();
        updated = updated.replace(&old_self, &new_ref);
        updated = updated.replace(&old_explicit, &new_ref);
        if let Some(latest_id) = &latest_id {
            let old_latest = format!("{}:/{}", latest_id, old_path);
            updated = updated.replace(&old_latest, &new_ref);
        }
    }

    if updated == text {
        return raw.to_vec();
    }

    encode_text(&updated, &encoding)
}

fn build_external_replacement_map(scanned: &ScannedData) -> BTreeMap<String, String> {
    let mut replacements = BTreeMap::new();
    for package in scanned.packages.values() {
        let latest_id = latest_package_id(&package.package_id);
        for (old_path, kept_ref) in &package.replacement_map {
            let new_ref = kept_ref.package_ref();
            let old_explicit = format!("{}:/{}", package.package_id, old_path);
            if old_explicit != new_ref {
                replacements.insert(old_explicit, new_ref.clone());
            }
            if let Some(latest_id) = &latest_id {
                let old_latest = format!("{}:/{}", latest_id, old_path);
                if old_latest != new_ref {
                    replacements.insert(old_latest, new_ref);
                }
            }
        }
    }
    replacements
}

pub(crate) fn rewrite_external_text_payload(
    raw: &[u8],
    replacements: &BTreeMap<String, String>,
) -> Option<Vec<u8>> {
    let (text, encoding) = decode_text(raw)?;

    let mut updated = text.clone();
    for (old_ref, new_ref) in replacements {
        updated = updated.replace(old_ref, new_ref);
    }

    if updated == text {
        None
    } else {
        Some(encode_text(&updated, &encoding))
    }
}

fn is_vap_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("vap"))
        .unwrap_or(false)
}

fn rewrite_vap_directory(
    vap_dir: &Path,
    replacements: &BTreeMap<String, String>,
    output_root: &Path,
    backup_root: Option<&Path>,
    replace_in_place: bool,
    stats: &mut ExecutionStats,
    report_vap_files: &mut Vec<Value>,
) -> Result<()> {
    if !vap_dir.exists() {
        bail!("VAP folder does not exist: {}", vap_dir.display());
    }
    if !vap_dir.is_dir() {
        bail!("VAP path is not a folder: {}", vap_dir.display());
    }
    if replacements.is_empty() {
        return Ok(());
    }

    for entry in WalkDir::new(vap_dir)
        .into_iter()
        .filter_map(|entry| entry.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if !is_vap_file(path) {
            continue;
        }

        stats.vap_files_scanned += 1;
        let raw = fs::read(path)?;
        if let Some(updated) = rewrite_external_text_payload(&raw, replacements) {
            let relative_path = path.strip_prefix(vap_dir).map_err(|_| {
                anyhow!("failed to derive VAP relative path for {}", path.display())
            })?;

            if let Some(backup_root) = backup_root {
                let backup_path = backup_root.join("vap").join(relative_path);
                if let Some(parent) = backup_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(&backup_path, &raw)?;
            }

            let target_path = if replace_in_place {
                path.to_path_buf()
            } else {
                output_root.join("vap").join(relative_path)
            };
            if let Some(parent) = target_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&target_path, updated)?;
            stats.vap_files_rewritten += 1;
            report_vap_files.push(json!({
                "source": path.display().to_string(),
                "output": target_path.display().to_string(),
                "mode": if replace_in_place { "replace" } else { "copy" }
            }));
        }
    }

    Ok(())
}

pub(crate) fn rewrite_package(
    scanned: &ScannedData,
    package: &PreparedPackage,
    target_path: &Path,
) -> Result<()> {
    let reader = fs::File::open(&package.file_path)?;
    let mut source = ZipArchive::new(reader)?;
    let staging_path = if target_path == package.file_path {
        let file_name = target_path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| anyhow!("invalid file name {}", target_path.display()))?;
        Some(target_path.with_file_name(format!("{file_name}.dedupe-tmp")))
    } else {
        None
    };
    let write_path = staging_path.as_deref().unwrap_or(target_path);
    let writer = fs::File::create(write_path)?;
    let mut target = ZipWriter::new(writer);
    // Unix, as zip 2 always wrote: since zip 7 the default is the platform the
    // app runs on, which would change every new entry's "made by" byte.
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .system(System::Unix);

    for index in 0..source.len() {
        let mut entry = source.by_index(index)?;
        if entry.is_dir() {
            continue;
        }
        let internal_path = normalize_zip_path(entry.name());
        if package.removed_paths.contains(&internal_path) {
            continue;
        }

        let is_meta = internal_path == META_PATH;
        let is_text = Path::new(&internal_path)
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| format!(".{}", ext.to_ascii_lowercase()))
            .as_deref()
            .map(|ext| TEXT_EXTENSIONS.contains(&ext))
            .unwrap_or(false);

        // Only meta and text payloads ever need to be re-encoded; for every
        // other entry pass the compressed bytes through verbatim instead of
        // paying decompress+recompress for a result identical to the input.
        if !is_meta && !is_text {
            target.raw_copy_file(entry)?;
            continue;
        }

        let entry_name = entry.name().to_string();
        let mut raw = Vec::new();
        entry.read_to_end(&mut raw)?;

        if is_meta {
            let mut meta = package.meta.clone();
            meta["contentList"] = Value::Array(
                package
                    .content_list
                    .iter()
                    .filter(|item| !package.removed_paths.contains(*item))
                    .map(|item| Value::String(item.clone()))
                    .collect(),
            );

            let mut dependencies = package.dependencies.clone();
            if !dependencies.is_object() {
                dependencies = json!({});
            }

            let deps_obj = dependencies
                .as_object_mut()
                .expect("dependencies object ensured");
            for dependency_id in &package.required_dependencies {
                // For local relocations we copy the kept package's licenseType
                // into the target's deps. For DB-only relocations (Find
                // Duplicates page → catalog package) the dep isn't present in
                // `scanned.packages`; we declare the dep with a default
                // licenseType so the rewritten meta.json stays well-formed
                // and references like `Catalog:/path` still resolve at
                // runtime once the user has Catalog installed.
                let license_type = scanned
                    .packages
                    .get(dependency_id)
                    .map(|p| p.license_type.clone())
                    .unwrap_or_else(|| "PC EA".to_string());
                deps_obj.insert(
                    dependency_id.clone(),
                    json!({
                        "licenseType": license_type,
                        "dependencies": {}
                    }),
                );
            }
            meta["dependencies"] = dependencies;
            raw = dump_json_bytes(&meta)?;
        } else {
            raw = rewrite_text_payload(package, &raw);
        }

        target.start_file(entry_name, options)?;
        target.write_all(&raw)?;
    }

    target.finish()?;
    drop(source);

    if let Some(staging_path) = staging_path {
        fs::remove_file(target_path)?;
        fs::rename(staging_path, target_path)?;
    }

    Ok(())
}

pub(crate) fn sum_removed_bytes(package: &PreparedPackage) -> u64 {
    let size_map = package
        .resource_refs
        .iter()
        .map(|item| (item.internal_path.clone(), item.size))
        .collect::<HashMap<_, _>>();

    package
        .removed_paths
        .iter()
        .map(|path| size_map.get(path).copied().unwrap_or(0))
        .sum()
}

pub(crate) fn execute_with_progress<F>(
    request: ExecuteRequest,
    cached_scan: Option<ScannedData>,
    db: Option<&Db>,
    mut on_progress: F,
) -> Result<ExecuteResponse>
where
    F: FnMut(f64, String) + Send,
{
    let input_dir = PathBuf::from(&request.input_dir);
    let additional_dirs: Vec<PathBuf> = request
        .additional_input_dirs
        .iter()
        .map(|dir| dir.trim())
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .collect();
    let output_dir = PathBuf::from(&request.output_dir);
    let replace_in_place = request.replace;
    let target_var_path = request
        .target_var_path
        .as_deref()
        .filter(|path| !path.trim().is_empty())
        .map(PathBuf::from);

    if !replace_in_place && fs::canonicalize(&input_dir).ok() == fs::canonicalize(&output_dir).ok()
    {
        bail!("Output folder must be different from the input folder");
    }

    let mut scanned = if let Some(scanned) = cached_scan {
        on_progress(0.50, "Using cached scan result".to_string());
        scanned
    } else {
        let scanned = scan_directory_with_target_with_progress(
            &input_dir,
            &additional_dirs,
            target_var_path.as_deref(),
            |scan_progress, message| {
                let progress = 0.05 + scan_progress * 0.45;
                on_progress(progress, message);
            },
        )?;
        scanned
    };

    on_progress(0.55, "Preparing dedupe plan".to_string());
    let applicable_groups = prepare_package_changes(
        &mut scanned,
        &request.keep_map,
        request.target_package_id.as_deref(),
        db,
    )?;

    let backup_dir = if replace_in_place && request.backup {
        fs::create_dir_all(&output_dir)?;
        let path = output_dir.join("backup");
        fs::create_dir_all(&path)?;
        Some(path)
    } else {
        None
    };

    let changed_dir = if replace_in_place {
        input_dir.clone()
    } else {
        fs::create_dir_all(&output_dir)?;
        let path = output_dir.join("changed");
        fs::create_dir_all(&path)?;
        path
    };

    let mut stats = ExecutionStats {
        scanned_packages: scanned.packages.len(),
        duplicate_groups: applicable_groups,
        ..ExecutionStats::default()
    };

    let mut report_packages = serde_json::Map::new();
    let mut report_vap_files = Vec::new();
    let total_packages = scanned.packages.len().max(1) as f64;

    // Per-package outcomes computed in parallel. Each task is independent: it
    // reads from `scanned` (shared, read-only), writes to its own ZIP output
    // (per-package target path), and accumulates a small per-package report
    // map. Aggregation into `stats` and `report_packages` happens in-order on
    // the main thread after the par_iter completes. DB stays read-only here.
    struct PackageOutcome {
        package_id: String,
        is_changed: bool,
        removed_count: usize,
        reclaimed: u64,
        report: Option<serde_json::Map<String, Value>>,
    }

    let packages_ordered: Vec<&PreparedPackage> = scanned.packages.values().collect();
    let completed = std::sync::atomic::AtomicUsize::new(0);
    let progress_mutex: Mutex<&mut F> = Mutex::new(&mut on_progress);
    let backup_dir_ref = backup_dir.as_ref();
    let changed_dir_ref = &changed_dir;
    let scanned_ref = &scanned;

    let outcomes_result: Result<Vec<PackageOutcome>> = {
        use rayon::prelude::*;
        packages_ordered
            .par_iter()
            .map(|package| -> Result<PackageOutcome> {
                let is_changed = !package.removed_paths.is_empty()
                    || !package.required_dependencies.is_empty();
                let file_name_os = package
                    .file_path
                    .file_name()
                    .ok_or_else(|| anyhow!("invalid file name {}", package.file_path.display()))?;
                let target_path = if replace_in_place {
                    Some(package.file_path.clone())
                } else if is_changed {
                    Some(changed_dir_ref.join(file_name_os))
                } else {
                    None
                };
                let file_name = package
                    .file_path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("unknown.var")
                    .to_string();

                if is_changed {
                    if let Some(backup_root) = backup_dir_ref {
                        fs::copy(&package.file_path, backup_root.join(&file_name))?;
                    }
                    rewrite_package(
                        scanned_ref,
                        package,
                        target_path
                            .as_deref()
                            .ok_or_else(|| anyhow!("missing target path for changed package"))?,
                    )?;
                }

                let done = completed.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                let progress = 0.60 + ((done as f64 / total_packages) * 0.35);
                let msg = if is_changed {
                    format!("Writing {file_name}")
                } else {
                    format!("Skipping {file_name}")
                };
                if let Ok(mut p) = progress_mutex.lock() {
                    (*p)(progress, msg);
                }

                let mut report = None;
                if is_changed {
                    let mut report_package = serde_json::Map::new();
                    report_package.insert(
                        "source".to_string(),
                        Value::String(package.file_path.display().to_string()),
                    );
                    report_package.insert(
                        "output".to_string(),
                        Value::String(
                            target_path
                                .as_ref()
                                .ok_or_else(|| anyhow!("missing target path for changed package"))?
                                .display()
                                .to_string(),
                        ),
                    );
                    report_package.insert(
                        "mode".to_string(),
                        Value::String(
                            if replace_in_place { "replace" } else { "copy" }.to_string(),
                        ),
                    );
                    if !package.removed_paths.is_empty() {
                        report_package.insert(
                            "removed_paths".to_string(),
                            json!(package.removed_paths.iter().cloned().collect::<Vec<_>>()),
                        );
                    }
                    if !package.required_dependencies.is_empty() {
                        report_package.insert(
                            "required_dependencies".to_string(),
                            json!(package
                                .required_dependencies
                                .iter()
                                .cloned()
                                .collect::<Vec<_>>()),
                        );
                    }
                    report = Some(report_package);
                }

                Ok(PackageOutcome {
                    package_id: package.package_id.clone(),
                    is_changed,
                    removed_count: if is_changed { package.removed_paths.len() } else { 0 },
                    reclaimed: if is_changed { sum_removed_bytes(package) } else { 0 },
                    report,
                })
            })
            .collect()
    };
    let outcomes = outcomes_result?;

    // Sequential fold preserves original report insertion order (BTreeMap
    // iteration is deterministic, and outcomes are collected in input order
    // by rayon's `collect`).
    for outcome in outcomes {
        if outcome.is_changed {
            stats.changed_packages += 1;
            stats.removed_files += outcome.removed_count;
            stats.reclaimed_bytes += outcome.reclaimed;
            if let Some(report_package) = outcome.report {
                report_packages.insert(outcome.package_id, Value::Object(report_package));
            }
        }
    }

    if let Some(vap_dir) = request
        .vap_dir
        .as_deref()
        .filter(|path| !path.trim().is_empty())
    {
        on_progress(0.96, "Rewriting VAP references".to_string());
        let replacements = build_external_replacement_map(&scanned);
        rewrite_vap_directory(
            Path::new(vap_dir),
            &replacements,
            &changed_dir,
            backup_dir.as_deref(),
            replace_in_place,
            &mut stats,
            &mut report_vap_files,
        )?;
    }

    on_progress(0.97, "Writing report".to_string());

    let report = json!({
        "packages": report_packages,
        "vap_files": report_vap_files,
        "summary": {
            "scanned_packages": stats.scanned_packages,
            "duplicate_groups": stats.duplicate_groups,
            "changed_packages": stats.changed_packages,
            "removed_files": stats.removed_files,
            "reclaimed_bytes": stats.reclaimed_bytes,
            "vap_files_scanned": stats.vap_files_scanned,
            "vap_files_rewritten": stats.vap_files_rewritten,
            "unchanged_packages": stats.scanned_packages.saturating_sub(stats.changed_packages)
        },
        "replace": replace_in_place,
        "backup_dir": backup_dir.as_ref().map(|path| path.display().to_string()),
        "vap_dir": request.vap_dir,
        "target_var_path": request.target_var_path,
        "target_package_id": request.target_package_id
    });

    let report_path = if replace_in_place {
        input_dir.join("dedupe-report.json")
    } else {
        output_dir.join("dedupe-report.json")
    };
    fs::write(&report_path, serde_json::to_vec_pretty(&report)?)?;
    on_progress(1.0, "Dedup completed".to_string());

    Ok(ExecuteResponse {
        stats,
        report_path: report_path.display().to_string(),
    })
}
