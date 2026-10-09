//! Internalize Resources — the inverse of dedup.
//!
//! `apply_fix_var` rewrites broken `Pkg:/path` refs to point at a *different*
//! var that already holds the bytes. This module does something different:
//! it takes a *valid* external ref (target's text says `OtherPkg:/path`,
//! `OtherPkg` is on disk, the path is present inside `OtherPkg.var`) and
//! copies the referenced bundle — `.vam` + `.vaj` + `.vab` + same-stem image +
//! every `customTexture_*` discovered inside the `.vaj` — directly into the
//! target var, then rewrites the refs to `SELF:/path` so the user can delete
//! the source var entirely.
//!
//! Fully local — no DB reads, no DB writes (honors the read-only invariant
//! from the dedup execute path). Bundles always come along whole so a `.vab`
//! cache never gets stranded without its `.vam` sibling.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
    path::Path,
};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use zip::{write::SimpleFileOptions, CompressionMethod, System, ZipArchive, ZipWriter};

use crate::{
    execute::{binary_cache_sibling_descriptor, paired_support_paths, rewrite_external_text_payload},
    fix_var::{
        collect_pkg_refs, free_backup_path, is_text_path, resolve_pkg_entry, resolve_ref_status,
        RefStatus,
    },
    naming,
    models::{
        BrokenRef, ExternalRef, ExternalRefGroup, FillSource, InternalizeReport,
        InternalizeSelection, RefFill, ScannedData, META_PATH,
    },
    scan::{extract_vaj_self_paths, extract_vaj_support_paths},
    utils::{decode_text, dump_json_bytes, normalize_zip_path, read_json_bytes},
};

/// Walks the target's text payloads, harvests every external `Pkg:/path` ref,
/// drops the ones that are broken (those belong on the Missing Resources page)
/// and the ones that point back at the target itself, then groups the rest by
/// source package. For each ref, opens the source var to grab the entry's
/// CRC32 + size and expand the bundle (siblings + `.vaj` textures) — exactly
/// what we need to display the row and what we'd copy if the user picks it.
pub(crate) fn scan_target_var_for_external_refs(
    target_path: &Path,
    scan: &ScannedData,
) -> Result<Vec<ExternalRefGroup>> {
    let file = fs::File::open(target_path)
        .with_context(|| format!("failed to open {}", target_path.display()))?;
    let mut archive = ZipArchive::new(file)
        .with_context(|| format!("failed to open zip archive {}", target_path.display()))?;

    let target_pkg_id = target_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(|s| s.to_string())
        .unwrap_or_default();

    // (source_pkg_id, ref_path) → ExternalRef. Same path showing up across
    // multiple text payloads collapses into one row, with each referencing
    // file appended to source_files_in_target.
    let mut refs: BTreeMap<(String, String), ExternalRef> = BTreeMap::new();
    // What the target already holds, to tell a bundle member that's already
    // inside from one that would clash with a different file.
    let mut target_crcs: BTreeMap<String, u32> = BTreeMap::new();
    // family (lowercase) → references to it that can't be copied in.
    let mut unusable: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    // family (lowercase) → (the package as named, references to its scripts).
    let mut plugins: BTreeMap<String, (String, BTreeSet<String>)> = BTreeMap::new();

    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .with_context(|| format!("failed to read entry #{index}"))?;
        if entry.is_dir() {
            continue;
        }
        let internal_path = normalize_zip_path(entry.name());
        target_crcs.insert(internal_path.clone(), entry.crc32());
        if internal_path == META_PATH {
            continue;
        }
        if !is_text_path(&internal_path) {
            continue;
        }

        let mut raw = Vec::new();
        entry.read_to_end(&mut raw)?;
        let Some((text, _)) = decode_text(&raw) else {
            continue;
        };

        for (pkg, path) in collect_pkg_refs(&text) {
            if pkg == target_pkg_id {
                continue;
            }
            let path_norm = normalize_zip_path(&path);
            if is_script_path(&path_norm) {
                plugins
                    .entry(family_of(&pkg))
                    .or_insert_with(|| (pkg.clone(), BTreeSet::new()))
                    .1
                    .insert(format!("{pkg}:/{path_norm}"));
                continue;
            }
            // A file not in the package as named (or the package isn't
            // installed): it can only come from an exact copy elsewhere.
            if !matches!(resolve_ref_status(&pkg, &path, scan), RefStatus::Healthy) {
                unusable
                    .entry(family_of(&pkg))
                    .or_default()
                    .insert(format!("{pkg}:/{path_norm}"));
                continue;
            }
            let key = (pkg.clone(), path_norm.clone());
            let entry = refs.entry(key).or_insert_with(|| ExternalRef {
                ref_path: path_norm.clone(),
                crc32: 0,
                size: 0,
                bundle_paths: Vec::new(),
                bundle_total_size: 0,
                source_files_in_target: Vec::new(),
                already_inside: Vec::new(),
                conflicts: Vec::new(),
            });
            if !entry.source_files_in_target.contains(&internal_path) {
                entry.source_files_in_target.push(internal_path.clone());
            }
        }
    }
    drop(archive);

    // Resolve source-var bytes per group, opening each source archive once.
    // The scan was already partitioned so the same pkg_id with multiple file
    // paths is legitimate; resolve_pkg_entry picks the canonical one.
    type RefsByPkg = BTreeMap<String, Vec<((String, String), ExternalRef)>>;
    let mut by_pkg: RefsByPkg = BTreeMap::new();
    for (key, ext_ref) in refs {
        by_pkg.entry(key.0.clone()).or_default().push((key, ext_ref));
    }

    let mut groups: Vec<ExternalRefGroup> = Vec::new();
    for (pkg, mut items) in by_pkg {
        let Some(prepared) = resolve_pkg_entry(&pkg, scan) else {
            for (key, _) in &items {
                unusable.entry(family_of(&pkg)).or_default().insert(format!("{}:/{}", key.0, key.1));
            }
            continue;
        };
        let source_file = match fs::File::open(&prepared.file_path) {
            Ok(file) => file,
            Err(_) => continue,
        };
        let Ok(mut source_archive) = ZipArchive::new(source_file) else {
            continue;
        };
        let source_var_size = fs::metadata(&prepared.file_path)
            .map(|m| m.len())
            .unwrap_or(0);

        // Pre-index the source archive's entry sizes by normalized path. The
        // zip crate's by_name is O(n) under the hood, but we'll need lookups
        // by every bundle member, so a flat map saves time on large vars.
        let mut entry_sizes: BTreeMap<String, (u64, u32)> = BTreeMap::new();
        for index in 0..source_archive.len() {
            let entry = match source_archive.by_index(index) {
                Ok(e) => e,
                Err(_) => continue,
            };
            if entry.is_dir() {
                continue;
            }
            let path = normalize_zip_path(entry.name());
            entry_sizes.insert(path, (entry.size(), entry.crc32()));
        }

        let mut group_bundle: BTreeSet<String> = BTreeSet::new();
        let mut refs_out: Vec<ExternalRef> = Vec::new();
        for (_, mut ext_ref) in items.drain(..) {
            let (size, crc) = match entry_sizes.get(&ext_ref.ref_path) {
                Some(pair) => *pair,
                None => {
                    unusable
                        .entry(family_of(&pkg))
                        .or_default()
                        .insert(format!("{pkg}:/{}", ext_ref.ref_path));
                    continue;
                }
            };
            ext_ref.size = size;
            ext_ref.crc32 = crc;

            let bundle = expand_bundle_for_ref(&ext_ref.ref_path, &entry_sizes, &mut source_archive);
            let mut total: u64 = 0;
            for member in &bundle {
                if let Some((sz, crc)) = entry_sizes.get(member) {
                    total += *sz;
                    match target_crcs.get(member) {
                        Some(own) if own == crc => ext_ref.already_inside.push(member.clone()),
                        Some(_) => ext_ref.conflicts.push(member.clone()),
                        None => {}
                    }
                }
                group_bundle.insert(member.clone());
            }
            ext_ref.bundle_paths = bundle;
            ext_ref.bundle_total_size = total;
            refs_out.push(ext_ref);
        }

        if refs_out.is_empty() {
            continue;
        }

        let total_bundle_bytes: u64 = group_bundle
            .iter()
            .filter_map(|p| entry_sizes.get(p).map(|(sz, _)| *sz))
            .sum();

        // Stable: sort refs by path so the UI list is deterministic.
        refs_out.sort_by(|a, b| a.ref_path.cmp(&b.ref_path));

        groups.push(ExternalRefGroup {
            source_pkg_id: pkg,
            installed: true,
            source_var_path: prepared.file_path.to_string_lossy().to_string(),
            source_var_size,
            refs: refs_out,
            total_bundle_bytes,
            used_by_others: None,
            other_users: Vec::new(),
            other_refs: Vec::new(),
            fills: Vec::new(),
            plugin_refs: Vec::new(),
        });
    }

    // Every other package the target names still shows, with nothing to copy
    // from it directly: one it only uses as a plugin (the page says why it
    // stays), and one that isn't installed (exact copies of its files
    // elsewhere may still make the target whole).
    let mut named: BTreeMap<String, String> = BTreeMap::new();
    for (family, (pkg, _)) in &plugins {
        named.entry(family.clone()).or_insert_with(|| pkg.clone());
    }
    for (family, refs) in &unusable {
        if let Some(first) = refs.iter().next() {
            named
                .entry(family.clone())
                .or_insert_with(|| first.split(":/").next().unwrap_or_default().to_string());
        }
    }
    for (family, pkg) in &named {
        if groups.iter().any(|g| family_of(&g.source_pkg_id) == *family) {
            continue;
        }
        let prepared = resolve_pkg_entry(pkg, scan);
        groups.push(ExternalRefGroup {
            source_pkg_id: pkg.clone(),
            installed: prepared.is_some(),
            source_var_path: prepared.map(|p| p.file_path.to_string_lossy().to_string()).unwrap_or_default(),
            source_var_size: prepared.and_then(|p| fs::metadata(&p.file_path).ok()).map(|m| m.len()).unwrap_or(0),
            refs: Vec::new(),
            total_bundle_bytes: 0,
            used_by_others: None,
            other_users: Vec::new(),
            other_refs: Vec::new(),
            fills: Vec::new(),
            plugin_refs: Vec::new(),
        });
    }
    for group in &mut groups {
        let family = family_of(&group.source_pkg_id);
        if let Some(refs) = unusable.get(&family) {
            group.other_refs = refs.iter().cloned().collect();
        }
        if let Some((_, refs)) = plugins.get(&family) {
            group.plugin_refs = refs.iter().cloned().collect();
        }
    }

    // Stable order: largest reclaim-potential first, then alphabetical so the
    // UI's "biggest win" is always at the top.
    groups.sort_by(|a, b| {
        b.total_bundle_bytes
            .cmp(&a.total_bundle_bytes)
            .then_with(|| a.source_pkg_id.cmp(&b.source_pkg_id))
    });

    Ok(groups)
}

/// Expands a single internal path into its full bundle set inside the source
/// archive. `.vam` pulls its paired `.vaj/.vab/.png/.jpg/.jpeg` plus every
/// `customTexture_*` path declared inside the `.vaj`. `.vmi` pulls its `.vmb`
/// cache. Other extensions stand alone. Members are only included if they
/// actually exist in the source archive — the user might have a partial bundle
/// upstream and we don't want to lie about what we'd copy.
pub(crate) fn expand_bundle_for_ref(
    ref_path: &str,
    entry_sizes: &BTreeMap<String, (u64, u32)>,
    source_archive: &mut ZipArchive<fs::File>,
) -> Vec<String> {
    let mut members: BTreeSet<String> = BTreeSet::new();
    members.insert(ref_path.to_string());

    let lower = ref_path.to_ascii_lowercase();
    if lower.ends_with(".vam") || lower.ends_with(".vmi") {
        for sibling in paired_support_paths(ref_path) {
            let norm = normalize_zip_path(&sibling);
            if entry_sizes.contains_key(&norm) {
                members.insert(norm);
            }
        }
        // Walk every .vaj sibling we picked up and harvest its textures.
        let vaj_paths: Vec<String> = members
            .iter()
            .filter(|p| p.to_ascii_lowercase().ends_with(".vaj"))
            .cloned()
            .collect();
        for vaj_path in vaj_paths {
            if let Ok(mut entry) = source_archive.by_name(&vaj_path) {
                let mut raw = Vec::new();
                if entry.read_to_end(&mut raw).is_ok() {
                    for tex in extract_vaj_support_paths(&vaj_path, &raw) {
                        let norm = normalize_zip_path(&tex);
                        if entry_sizes.contains_key(&norm) {
                            members.insert(norm);
                        }
                    }
                    // SELF:/ texture refs in a source .vaj point at files
                    // inside the source archive. Pull them along so the
                    // .vaj's SELF:/ paths still resolve after the bundle
                    // lands in the target — otherwise a .vaj from a package
                    // whose layout differs from the target's existing copy
                    // (e.g. flat vs. `tex/<subfolder>/`) ends up with broken
                    // refs the game can't load.
                    for tex in extract_vaj_self_paths(&vaj_path, &raw) {
                        let norm = normalize_zip_path(&tex);
                        if entry_sizes.contains_key(&norm) {
                            members.insert(norm);
                        }
                    }
                }
            }
        }
    } else if lower.ends_with(".vab") || lower.ends_with(".vmb") {
        // A direct .vab/.vmb ref is unusual, but if it happens we still want
        // its descriptor sibling along for the ride.
        if let Some(sibling) = binary_cache_sibling_descriptor(ref_path) {
            let norm = normalize_zip_path(&sibling);
            if entry_sizes.contains_key(&norm) {
                members.insert(norm);
            }
        }
    }

    let mut out: Vec<String> = members.into_iter().collect();
    out.sort();
    out
}

/// Applies the user's internalization selections to the target var. For every
/// selected `(source_pkg, ref_path)`:
///
///   1. Expand to the full bundle (same logic as the scan).
///   2. Read the source-var bytes for each bundle member into memory.
///   3. Stream-rewrite the target var: pass binary entries through verbatim,
///      decode/text-replace `SourcePkg:/x` → `SELF:/x` for every bundle path,
///      buffer meta.json for finalization, then append the cross-copied
///      entries that weren't already in the target.
///   4. Finalize meta.json: drop source-pkg deps whose `SourcePkg:/` text
///      references all rewrote (mirrors apply_fix_var's still_references_pkg
///      check), and extend contentList with the newly added paths.
pub(crate) fn apply_internalize(
    target_path: &Path,
    output_path: Option<&Path>,
    scan: &ScannedData,
    selections: &[InternalizeSelection],
    backup_root: Option<&Path>,
) -> Result<InternalizeReport> {
    let mut report = InternalizeReport::default();
    if selections.is_empty() {
        return Ok(report);
    }

    let target_id = target_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string();

    // Build the cross-copy set: internal_path → bytes. Same path picked up
    // from two source vars / two refs (e.g. a shared texture) is read once.
    let mut copy_bytes: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    // SourcePkg:/path → SELF:/path replacements for every bundle member.
    let mut replacements: BTreeMap<String, String> = BTreeMap::new();
    // Track which selected source pkgs we actually copied something from, so
    // meta.json can decide whether to drop them.
    let mut touched_source_pkgs: BTreeSet<String> = BTreeSet::new();

    // Group the reads by the package they come from, so each archive opens
    // once: (path to read, path it lands at, package the reference names).
    // A file the source package lacks comes from the copy the scan found; one
    // the target already has is only pointed at.
    let mut by_pkg: BTreeMap<String, Vec<(String, String, String)>> = BTreeMap::new();
    for sel in selections {
        let ref_path = normalize_zip_path(&sel.ref_path);
        match (sel.from_pkg.as_deref(), sel.from_path.as_deref()) {
            (Some(from), Some(from_path)) if from == "SELF" || from == target_id => {
                touched_source_pkgs.insert(sel.source_pkg_id.clone());
                replacements.insert(
                    format!("{}:/{}", sel.source_pkg_id, ref_path),
                    format!("SELF:/{}", normalize_zip_path(from_path)),
                );
            }
            (Some(from), Some(from_path)) => by_pkg.entry(from.to_string()).or_default().push((
                normalize_zip_path(from_path),
                ref_path,
                sel.source_pkg_id.clone(),
            )),
            _ => by_pkg
                .entry(sel.source_pkg_id.clone())
                .or_default()
                .push((ref_path.clone(), ref_path, sel.source_pkg_id.clone())),
        }
    }

    for (pkg, mut paths) in by_pkg.into_iter() {
        let Some(prepared) = resolve_pkg_entry(&pkg, scan) else {
            report
                .errors
                .push(format!("source package not found locally: {pkg}"));
            continue;
        };
        let source_file = fs::File::open(&prepared.file_path).with_context(|| {
            format!("failed to open source var {}", prepared.file_path.display())
        })?;
        let mut source_archive = ZipArchive::new(source_file).with_context(|| {
            format!("failed to open zip {}", prepared.file_path.display())
        })?;

        // Pre-index sizes for the bundle expansion.
        let mut entry_sizes: BTreeMap<String, (u64, u32)> = BTreeMap::new();
        for index in 0..source_archive.len() {
            let entry = match source_archive.by_index(index) {
                Ok(e) => e,
                Err(_) => continue,
            };
            if entry.is_dir() {
                continue;
            }
            entry_sizes.insert(normalize_zip_path(entry.name()), (entry.size(), entry.crc32()));
        }

        paths.sort();
        paths.dedup();

        for (read_path, dest_path, ref_pkg) in &paths {
            if !entry_sizes.contains_key(read_path) {
                report
                    .errors
                    .push(format!("source pkg {pkg} has no entry at {read_path}"));
                continue;
            }
            touched_source_pkgs.insert(ref_pkg.clone());

            // A copy at another path comes alone, landing where the reference
            // points (the scan offers only single files for that).
            if read_path != dest_path {
                if !copy_bytes.contains_key(dest_path) {
                    let mut entry = source_archive
                        .by_name(read_path)
                        .with_context(|| format!("missing {read_path} in {pkg}"))?;
                    let mut raw = Vec::new();
                    entry.read_to_end(&mut raw)?;
                    copy_bytes.insert(dest_path.clone(), raw);
                }
                replacements.insert(format!("{ref_pkg}:/{dest_path}"), format!("SELF:/{dest_path}"));
                continue;
            }

            let bundle = expand_bundle_for_ref(read_path, &entry_sizes, &mut source_archive);
            for member in &bundle {
                if !copy_bytes.contains_key(member) {
                    let mut entry = source_archive
                        .by_name(member)
                        .with_context(|| format!("missing bundle member {member} in {pkg}"))?;
                    let mut raw = Vec::new();
                    entry.read_to_end(&mut raw)?;
                    copy_bytes.insert(member.clone(), raw);
                }
                // Any text payload — in the target, in the source, in a freshly
                // copied .vap/.vaj — might say `SourcePkg:/member`. After we
                // copy member in as SELF, every such reference should resolve
                // to SELF too. Bundle members beyond the explicit ref aren't
                // selections themselves, but their `Pkg:/path` form may still
                // appear (e.g. inside a .vap that references textures by full
                // package-qualified path); list the swap so we don't leave
                // half-rewritten refs behind.
                let old_ref = format!("{}:/{}", ref_pkg, member);
                let new_ref = format!("SELF:/{}", member);
                replacements.insert(old_ref, new_ref);
            }
        }
    }

    // If nothing actually needs copying or rewriting, bail before touching
    // the target file at all.
    if copy_bytes.is_empty() {
        return Ok(report);
    }

    if let Some(backup_root) = backup_root {
        fs::create_dir_all(backup_root)
            .with_context(|| format!("failed to create backup dir {}", backup_root.display()))?;
        let file_name = target_path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| anyhow!("invalid target file name"))?;
        // Never over an earlier backup: that one may be the only good copy.
        let backup_path = free_backup_path(backup_root, file_name);
        fs::copy(target_path, &backup_path).with_context(|| {
            format!(
                "failed to back up {} → {}",
                target_path.display(),
                backup_path.display()
            )
        })?;
        report.backup_path = Some(backup_path.to_string_lossy().to_string());
    }

    let reader = fs::File::open(target_path)
        .with_context(|| format!("failed to open {}", target_path.display()))?;
    let mut source = ZipArchive::new(reader)
        .with_context(|| format!("failed to read zip archive {}", target_path.display()))?;

    let file_name = target_path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow!("invalid target file name {}", target_path.display()))?;

    let (write_to, atomic_rename_target) = if let Some(dest) = output_path {
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create output dir {}", parent.display()))?;
        }
        (dest.to_path_buf(), None)
    } else {
        let staging = target_path.with_file_name(format!("{file_name}.internalize-tmp"));
        (staging, Some(target_path.to_path_buf()))
    };

    // First pass: stream source → target, applying text replacements. Track
    // which paths are already present in the target (collision = skip the
    // cross-copy for that path) and which source-pkg refs still appear after
    // rewriting (so meta.json knows whether to drop the dep).
    let mut existing_paths: BTreeSet<String> = BTreeSet::new();
    let touched_families: BTreeSet<String> = touched_source_pkgs.iter().map(|p| family_of(p)).collect();
    let mut still_references: BTreeSet<String> = BTreeSet::new();
    let mut meta_entry: Option<(String, Vec<u8>)> = None;

    {
        let writer = fs::File::create(&write_to)
            .with_context(|| format!("failed to create output file {}", write_to.display()))?;
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
            let raw_name = entry.name().to_string();
            let internal_path = normalize_zip_path(&raw_name);
            existing_paths.insert(internal_path.clone());

            if internal_path == META_PATH {
                let mut raw = Vec::new();
                entry.read_to_end(&mut raw)?;
                meta_entry = Some((raw_name, raw));
                continue;
            }

            if is_text_path(&internal_path) {
                let mut raw = Vec::new();
                entry.read_to_end(&mut raw)?;
                if let Some(updated) = rewrite_external_text_payload(&raw, &replacements) {
                    raw = updated;
                    report.files_rewritten += 1;
                }
                if let Some((text, _)) = decode_text(&raw) {
                    for (pkg, _) in collect_pkg_refs(&text) {
                        let family = family_of(&pkg);
                        if touched_families.contains(&family) {
                            still_references.insert(family);
                        }
                    }
                }
                target.start_file(&raw_name, options)?;
                target.write_all(&raw)?;
            } else {
                target.raw_copy_file(entry)?;
            }
        }

        // Append cross-copied entries that weren't already in the target.
        // Apply the same `Pkg:/` → `SELF:/` rewrite to text members coming
        // from the source (e.g. a .vaj that references its textures by full
        // package-qualified path).
        let mut added_paths: Vec<String> = Vec::new();
        for (path, bytes) in &copy_bytes {
            if existing_paths.contains(path) {
                report.entries_skipped_collision.push(path.clone());
                continue;
            }
            let final_bytes: Vec<u8> = if is_text_path(path) {
                rewrite_external_text_payload(bytes, &replacements).unwrap_or_else(|| bytes.clone())
            } else {
                bytes.clone()
            };
            target.start_file(path, options)?;
            target.write_all(&final_bytes)?;
            report.bytes_added += final_bytes.len() as u64;
            report.entries_copied += 1;
            added_paths.push(path.clone());
        }

        // Finalize meta.json: extend contentList with newly added paths, then
        // drop any source-pkg dep whose `SourcePkg:/` references all got
        // rewritten (i.e. the user internalized every ref into that pkg).
        if let Some((raw_name, raw)) = meta_entry.take() {
            let mut meta = read_json_bytes(&raw, "meta.json")?;

            // contentList: union of existing entries and added_paths.
            let mut content_list: BTreeSet<String> = match meta.get("contentList") {
                Some(Value::Array(arr)) => arr
                    .iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect(),
                _ => BTreeSet::new(),
            };
            for path in &added_paths {
                content_list.insert(path.clone());
            }
            meta["contentList"] = Value::Array(
                content_list
                    .into_iter()
                    .map(Value::String)
                    .collect::<Vec<_>>(),
            );

            // dependencies: drop only the ones with no surviving refs.
            let mut deps_obj = match meta.get("dependencies").cloned() {
                Some(Value::Object(m)) => m,
                _ => serde_json::Map::new(),
            };
            let gone: Vec<String> = deps_obj
                .keys()
                .filter(|key| {
                    let family = family_of(key);
                    touched_families.contains(&family) && !still_references.contains(&family)
                })
                .cloned()
                .collect();
            for key in gone {
                deps_obj.remove(&key);
                report.dependencies_removed.push(key);
            }
            meta["dependencies"] = Value::Object(deps_obj);

            let new_raw = dump_json_bytes(&meta)?;
            report.files_rewritten += 1;
            target.start_file(&raw_name, options)?;
            target.write_all(&new_raw)?;
        } else {
            // No meta.json in the target — odd but not strictly fatal. The
            // copy still works; we just can't record the dep change.
            report
                .errors
                .push("target var has no meta.json; dependencies left unchanged".to_string());
        }

        target.finish()?;
    }
    drop(source);

    let final_path = if let Some(final_target) = atomic_rename_target {
        if final_target.exists() {
            fs::remove_file(&final_target)
                .with_context(|| format!("failed to remove old {}", final_target.display()))?;
        }
        fs::rename(&write_to, &final_target).with_context(|| {
            format!(
                "failed to rename {} → {}",
                write_to.display(),
                final_target.display()
            )
        })?;
        final_target
    } else {
        write_to
    };

    report.refs_rewritten = selections.len() as u32;
    report.output_path = Some(final_path.to_string_lossy().to_string());

    // Sanity-check the report so the UI doesn't claim "copied X entries" if
    // every single one collided with an existing entry. Not an error per se
    // (replacements still get applied), but worth flagging.
    if report.entries_copied == 0 && !report.entries_skipped_collision.is_empty() {
        report.errors.push(
            "every selected bundle member already existed in the target — only refs were rewritten"
                .to_string(),
        );
    }

    // Belt-and-suspenders: the target should now be a valid zip.
    if let Err(err) = fs::File::open(&final_path).map_err(anyhow::Error::from).and_then(|f| {
        ZipArchive::new(f)
            .map(|_| ())
            .map_err(anyhow::Error::from)
    }) {
        bail!("output var failed to re-open as zip: {err}");
    }

    Ok(report)
}

/// A package's version-less family, lowercase: `Creator.Pack.latest` and
/// `Creator.Pack.3` are the same dependency.
fn family_of(pkg: &str) -> String {
    naming::package_base(pkg).to_ascii_lowercase()
}

/// For each source package's references it can't copy (the file isn't in it),
/// an exact copy elsewhere, from the missing-file check (`broken`): one the
/// target already holds, else another package's. Only an exact copy (same
/// contents) counts; at another path only a single file, since a .vam's
/// siblings and textures would land at the wrong paths. A file the target
/// holds with other contents at that path clashes and gets none.
pub(crate) fn fill_from_copies(groups: &mut [ExternalRefGroup], broken: &[BrokenRef], target_path: &Path) -> Result<()> {
    let target_id = target_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string();
    let file = fs::File::open(target_path)
        .with_context(|| format!("failed to open {}", target_path.display()))?;
    let mut archive = ZipArchive::new(file)?;
    let mut target_crcs: BTreeMap<String, u32> = BTreeMap::new();
    for index in 0..archive.len() {
        if let Ok(e) = archive.by_index(index) {
            target_crcs.insert(normalize_zip_path(e.name()), e.crc32());
        }
    }
    for group in groups.iter_mut() {
        if group.other_refs.is_empty() {
            continue;
        }
        let wanted: BTreeSet<&String> = group.other_refs.iter().collect();
        for b in broken {
            let (Some(path), Some(crc)) = (b.ref_path.as_deref(), b.expected_crc32) else {
                continue;
            };
            let ref_path = normalize_zip_path(path);
            let key = format!("{}:/{}", b.ref_pkg, ref_path);
            if !wanted.contains(&key) {
                continue;
            }
            match target_crcs.get(&ref_path) {
                Some(own) if *own == crc => {
                    let own = FillSource {
                        from_pkg: "SELF".into(),
                        from_path: ref_path.clone(),
                        size: b.expected_size.unwrap_or(0),
                        from_self: true,
                    };
                    group.fills.push(RefFill {
                        ref_pkg: b.ref_pkg.clone(),
                        ref_path: ref_path.clone(),
                        from_pkg: own.from_pkg.clone(),
                        from_path: own.from_path.clone(),
                        size: own.size,
                        from_self: true,
                        alternatives: vec![own],
                    });
                    continue;
                }
                Some(_) => continue,
                None => {}
            }
            let single = |p: &str| {
                let lower = p.to_ascii_lowercase();
                ![".vam", ".vmi", ".vaj", ".vab", ".vmb"].iter().any(|ext| lower.ends_with(ext))
            };
            let exact = b.local_candidates.iter().map(|c| &c.resource).filter(|r| {
                r.crc32 == Some(crc)
                    && b.expected_size.is_none_or(|s| s == r.size)
                    && (normalize_zip_path(&r.internal_path) == ref_path || single(&r.internal_path))
            });
            // Every exact copy, the target's own first (nothing to read).
            let mut sources: Vec<FillSource> = exact
                .map(|r| {
                    let from_self = r.package_id.eq_ignore_ascii_case(&target_id);
                    FillSource {
                        from_pkg: if from_self { "SELF".into() } else { r.package_id.clone() },
                        from_path: normalize_zip_path(&r.internal_path),
                        size: r.size,
                        from_self,
                    }
                })
                .collect();
            sources.sort_by_key(|s| !s.from_self);
            if let Some(first) = sources.first().cloned() {
                group.fills.push(RefFill {
                    ref_pkg: b.ref_pkg.clone(),
                    ref_path,
                    from_pkg: first.from_pkg,
                    from_path: first.from_path,
                    size: first.size,
                    from_self: first.from_self,
                    alternatives: sources,
                });
            }
        }
    }
    Ok(())
}

/// A plugin's scripts: a .cslist (a list of scripts), .cs or .dll.
fn is_script_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".cslist") || lower.ends_with(".cs") || lower.ends_with(".dll")
}
