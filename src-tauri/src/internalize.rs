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
use zip::{write::SimpleFileOptions, CompressionMethod, ZipArchive, ZipWriter};

use crate::{
    execute::{binary_cache_sibling_descriptor, paired_support_paths, rewrite_external_text_payload},
    fix_var::{collect_pkg_refs, is_text_path, resolve_pkg_entry, resolve_ref_status, RefStatus},
    models::{
        ExternalRef, ExternalRefGroup, InternalizeReport, InternalizeSelection, ScannedData,
        META_PATH,
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

    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .with_context(|| format!("failed to read entry #{index}"))?;
        if entry.is_dir() {
            continue;
        }
        let internal_path = normalize_zip_path(entry.name());
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
            // Only valid refs (source pkg + path both resolvable locally).
            // Broken refs are handled by the Missing Resources page; surfacing
            // them here would offer a "copy" the user couldn't actually do.
            if !matches!(resolve_ref_status(&pkg, &path, scan), RefStatus::Healthy) {
                continue;
            }
            let path_norm = normalize_zip_path(&path);
            let key = (pkg.clone(), path_norm.clone());
            let entry = refs.entry(key).or_insert_with(|| ExternalRef {
                ref_path: path_norm.clone(),
                crc32: 0,
                size: 0,
                bundle_paths: Vec::new(),
                bundle_total_size: 0,
                source_files_in_target: Vec::new(),
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
    let mut by_pkg: BTreeMap<String, Vec<((String, String), ExternalRef)>> = BTreeMap::new();
    for (key, ext_ref) in refs {
        by_pkg.entry(key.0.clone()).or_default().push((key, ext_ref));
    }

    let mut groups: Vec<ExternalRefGroup> = Vec::new();
    for (pkg, mut items) in by_pkg {
        let Some(prepared) = resolve_pkg_entry(&pkg, scan) else {
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
                None => continue,
            };
            ext_ref.size = size;
            ext_ref.crc32 = crc;

            let bundle = expand_bundle_for_ref(&ext_ref.ref_path, &entry_sizes, &mut source_archive);
            let mut total: u64 = 0;
            for member in &bundle {
                if let Some((sz, _)) = entry_sizes.get(member) {
                    total += *sz;
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
            source_var_path: prepared.file_path.to_string_lossy().to_string(),
            source_var_size,
            refs: refs_out,
            total_bundle_bytes,
        });
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
fn expand_bundle_for_ref(
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

    // Group selections by source pkg so we open each source archive once.
    let mut by_pkg: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for sel in selections {
        by_pkg
            .entry(sel.source_pkg_id.clone())
            .or_default()
            .push(normalize_zip_path(&sel.ref_path));
    }

    // Build the cross-copy set: internal_path → bytes. Same path picked up
    // from two source vars / two refs (e.g. a shared texture) is read once.
    let mut copy_bytes: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    // SourcePkg:/path → SELF:/path replacements for every bundle member.
    let mut replacements: BTreeMap<String, String> = BTreeMap::new();
    // Track which selected source pkgs we actually copied something from, so
    // meta.json can decide whether to drop them.
    let mut touched_source_pkgs: BTreeSet<String> = BTreeSet::new();

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

        for ref_path in &paths {
            if !entry_sizes.contains_key(ref_path) {
                report
                    .errors
                    .push(format!("source pkg {pkg} has no entry at {ref_path}"));
                continue;
            }
            touched_source_pkgs.insert(pkg.clone());

            let bundle = expand_bundle_for_ref(ref_path, &entry_sizes, &mut source_archive);
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
                let old_ref = format!("{}:/{}", pkg, member);
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
            .ok_or_else(|| anyhow!("invalid target file name"))?;
        let backup_path = backup_root.join(file_name);
        fs::copy(target_path, &backup_path).with_context(|| {
            format!(
                "failed to back up {} → {}",
                target_path.display(),
                backup_path.display()
            )
        })?;
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
    let mut still_references_pkg: BTreeSet<String> = BTreeSet::new();
    let mut meta_entry: Option<(String, Vec<u8>)> = None;

    {
        let writer = fs::File::create(&write_to)
            .with_context(|| format!("failed to create output file {}", write_to.display()))?;
        let mut target = ZipWriter::new(writer);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

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
                    for pkg in &touched_source_pkgs {
                        if still_references_pkg.contains(pkg) {
                            continue;
                        }
                        let needle = format!("{}:/", pkg);
                        if text.contains(&needle) {
                            still_references_pkg.insert(pkg.clone());
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
            for pkg in &touched_source_pkgs {
                if still_references_pkg.contains(pkg) {
                    continue;
                }
                if deps_obj.remove(pkg).is_some() {
                    report.dependencies_removed.push(pkg.clone());
                }
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
