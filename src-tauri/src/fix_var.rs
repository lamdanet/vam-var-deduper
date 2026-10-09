use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use zip::{write::SimpleFileOptions, CompressionMethod, System, ZipArchive, ZipWriter};

use crate::{
    db::{self, Db},
    execute::rewrite_external_text_payload,
    models::{
        BrokenKind, BrokenRef, BrokenRefCandidate, FixDirective, FixReport, PreparedPackage,
        ResourceRef, ScannedData, META_PATH, TEXT_EXTENSIONS,
    },
    utils::{decode_text, dump_json_bytes, normalize_zip_path, read_json_bytes},
};

/// Cap on how many local candidates we surface per broken ref. Keeps the
/// right panel from rendering thousands of rows when a CRC32 collides with a
/// very common resource (e.g. an empty file).
const MAX_LOCAL_CANDIDATES: usize = 32;

/// Maximum number of `pkg:/path` references we'll harvest from a single text
/// payload. Pathological scenes can contain tens of thousands; the cap keeps
/// scan time bounded while still catching every real reference in normal
/// scenes.
const MAX_REFS_PER_FILE: usize = 8192;

/// Scans a target VAR for broken dependency references. Reuses the cached
/// local scan (`scan`) to resolve which packages exist locally and what they
/// still contain; reaches into the DB only for expected CRC32 lookups so the
/// frontend can rank candidates.
pub(crate) fn scan_target_var_for_broken_refs(
    target_path: &Path,
    scan: &ScannedData,
    db: &Db,
) -> Result<Vec<BrokenRef>> {
    let file = fs::File::open(target_path)
        .with_context(|| format!("failed to open {}", target_path.display()))?;
    let mut archive = ZipArchive::new(file)
        .with_context(|| format!("failed to open zip archive {}", target_path.display()))?;

    let target_pkg_id = target_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(|s| s.to_string())
        .unwrap_or_default();

    // Look up the missing resource's CRC32 in the DB regardless of mode. The
    // CRC is what lets us match the *same* resource saved under a *different*
    // path name (a content match, not a path match) — e.g. A.var references a
    // file that another local VAR ships under a renamed path. Without the
    // DB-derived CRC, candidate matching collapses to exact-path-only and
    // misses those renamed local copies. Each per-ref query below short-
    // circuits once a CRC is found, so this is "find the CRC, then stop".
    // It only reads, so it uses the read-only connections: the rest of the
    // app's database work isn't held up while it runs.

    let mut broken: HashMap<(String, Option<String>), BrokenRef> = HashMap::new();

    // Single pass: buffer every `(pkg, path)` reference we see and which file
    // it came from, so a later pass can partition sibling-missings into
    // explicit (path appears verbatim in some scene text) vs implicit (only
    // stem-loaded by VAM via the parent .vam/.vmi).
    let mut all_refs: Vec<RefOccurrence> = Vec::new();

    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .with_context(|| format!("failed to read entry #{index}"))?;
        if entry.is_dir() {
            continue;
        }
        let internal_path = normalize_zip_path(entry.name());

        if internal_path == META_PATH {
            // Only payload text refs surface as broken — the user opted out
            // of meta.json `dependencies` reporting because every dep there
            // is also captured (with full path) by the text-ref scan, so the
            // meta-only entries were noise.
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
            all_refs.push(RefOccurrence {
                pkg,
                path: normalize_zip_path(&path),
                source_file: internal_path.clone(),
            });
        }
    }

    // Collapse repeated occurrences of the same (pkg, path) before
    // classifying. `resolve_ref_status` is pure for a given (pkg, path), so a
    // scene that names the same resource thousands of times only needs one
    // status check instead of one per occurrence. First-seen order of source
    // files per ref is preserved.
    let mut unique_refs: HashMap<(String, String), Vec<String>> = HashMap::new();
    for RefOccurrence {
        pkg,
        path,
        source_file,
    } in &all_refs
    {
        let files = unique_refs
            .entry((pkg.clone(), path.clone()))
            .or_default();
        if !files.contains(source_file) {
            files.push(source_file.clone());
        }
    }

    for ((pkg, path), source_files) in unique_refs {
        let kind = match resolve_ref_status(&pkg, &path, scan) {
            // The referenced (pkg, path) is present in some local VAR — the
            // Missing Resources page only cares about resources that aren't.
            RefStatus::Healthy => continue,
            RefStatus::PkgMissing => BrokenKind::TextRef,
            RefStatus::PathMissing => BrokenKind::Transitive,
        };

        broken.insert(
            (pkg.clone(), Some(path.clone())),
            BrokenRef {
                kind,
                ref_pkg: pkg,
                ref_path: Some(path),
                expected_crc32: None,
                expected_size: None,
                source_files_in_target: source_files,
                local_candidates: Vec::new(),
            },
        );
    }

    let mut out: Vec<BrokenRef> = broken.into_values().collect();

    // Index every indexed resource by internal_path and by crc32 once, so the
    // per-broken-ref candidate search below is O(matches) instead of a fresh
    // O(packages × resources) scan for each broken ref. Buckets are filled in
    // `scan.packages` (BTreeMap) iteration order, so candidate ordering is
    // identical to the old nested scans.
    let mut by_path: HashMap<&str, Vec<(&str, &ResourceRef)>> = HashMap::new();
    let mut by_crc: HashMap<u32, Vec<(&str, &ResourceRef)>> = HashMap::new();
    for (pkg_id, package) in &scan.packages {
        for resource in &package.resource_refs {
            by_path
                .entry(resource.internal_path.as_str())
                .or_default()
                .push((pkg_id.as_str(), resource));
            if let Some(crc) = resource.crc32 {
                by_crc.entry(crc).or_default().push((pkg_id.as_str(), resource));
            }
        }
    }

    let conn = db.read()?;
    for br in out.iter_mut() {
        if let Some(path) = br.ref_path.clone() {
            // 1. Exact lookup: (broken_pkg, path) in the indexed packages
            //    table. This is the strongest signal because it tells us what
            //    the file looked like before it was removed during dedup.
            if br.expected_crc32.is_none() {
                if let Ok(Some(rref)) =
                    db::load_resource_by_pid_and_path(&conn, &br.ref_pkg, &path)
                {
                    br.expected_crc32 = rref.crc32;
                    br.expected_size = Some(rref.size);
                }
            }
            // 2. Fall back to internal_path-only lookup if (1) didn't yield
            //    a CRC. The broken_pkg may have never been indexed (catalog
            //    drift) but another package may store the same file at the
            //    same internal path — in practice that file usually has the
            //    same CRC because the resource was copy-pasted across packs
            //    by creators. The local scan here (cheap, in-memory); the DB
            //    comes in step 3, for all of them at once.
            if br.expected_crc32.is_none() {
                if let Some(&(_, resource)) = by_path.get(path.as_str()).and_then(|cands| {
                    cands
                        .iter()
                        .find(|&&(pkg_id, r)| pkg_id != br.ref_pkg.as_str() && r.crc32.is_some())
                }) {
                    br.expected_crc32 = resource.crc32;
                    br.expected_size = Some(resource.size);
                }
            }
        }
    }

    drop(conn);

    // 3. Still no CRC: the DB by path alone, for all of them in one pass.
    //    `resources` has no index on the path alone, so a query per file read
    //    every row (about 0.5 s each on a large database); the one pass is
    //    split into row ranges read at once on the read connections.
    let wanted: Vec<String> = out
        .iter()
        .filter(|br| br.expected_crc32.is_none())
        .filter_map(|br| br.ref_path.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if !wanted.is_empty() {
        if let Ok(found) = lookup_crcs_by_paths_split(db, &wanted) {
            for br in out.iter_mut().filter(|br| br.expected_crc32.is_none()) {
                if let Some(&(crc, size)) = br.ref_path.as_deref().and_then(|p| found.get(p)) {
                    br.expected_crc32 = Some(crc);
                    br.expected_size = Some(size);
                }
            }
        }
    }

    for br in out.iter_mut() {
        if let Some(path) = br.ref_path.clone() {
            br.local_candidates = find_local_candidates(
                &by_path,
                &by_crc,
                &br.ref_pkg,
                &path,
                br.expected_crc32,
                br.expected_size,
            );
        }
    }

    out.sort_by(|a, b| {
        a.ref_pkg
            .to_lowercase()
            .cmp(&b.ref_pkg.to_lowercase())
            .then_with(|| {
                a.ref_path
                    .as_deref()
                    .unwrap_or("")
                    .to_lowercase()
                    .cmp(&b.ref_path.as_deref().unwrap_or("").to_lowercase())
            })
    });

    Ok(out)
}

pub(crate) enum RefStatus {
    Healthy,
    PkgMissing,
    PathMissing,
}

struct RefOccurrence {
    pkg: String,
    path: String,
    source_file: String,
}

pub(crate) fn resolve_ref_status(pkg: &str, path: &str, scan: &ScannedData) -> RefStatus {
    let Some(pkg_state) = resolve_pkg_entry(pkg, scan) else {
        return RefStatus::PkgMissing;
    };
    let path_norm = normalize_zip_path(path);
    if pkg_contains_path(pkg_state, &path_norm) {
        RefStatus::Healthy
    } else {
        RefStatus::PathMissing
    }
}

/// Resolves a `creator.pkg.VERSION` reference to a concrete entry in
/// `scan.packages`. When VERSION is `latest`, picks the highest numeric
/// version sharing the same base (case-insensitive on the base). This
/// mirrors VAM's runtime behavior: `pkg.latest` loads whichever installed
/// version is newest, so a `.latest` ref shouldn't be flagged missing just
/// because no file literally named `pkg.latest.var` exists.
pub(crate) fn resolve_pkg_entry<'a>(pkg: &str, scan: &'a ScannedData) -> Option<&'a PreparedPackage> {
    if let Some(entry) = scan.packages.get(pkg) {
        return Some(entry);
    }
    let (base, ver) = pkg.rsplit_once('.')?;
    if !ver.eq_ignore_ascii_case("latest") {
        return None;
    }
    let base_lc = base.to_ascii_lowercase();
    let mut best: Option<(u64, &PreparedPackage)> = None;
    for (id, prepared) in &scan.packages {
        if !crate::naming::package_base(id).eq_ignore_ascii_case(&base_lc) {
            continue;
        }
        // `package_version` yields None for `.latest`/non-numeric/absent
        // versions, so those can never win the "highest installed" race.
        let Some(n) = crate::naming::package_version(id) else {
            continue;
        };
        if best.is_none_or(|(b, _)| n > b) {
            best = Some((n, prepared));
        }
    }
    best.map(|(_, p)| p)
}

fn pkg_contains_path(pkg: &PreparedPackage, path: &str) -> bool {
    pkg.content_list.iter().any(|p| p == path)
        || pkg.resource_refs.iter().any(|r| r.internal_path == path)
}

/// Path-only fallback used when `(broken_pkg, internal_path)` isn't in the
/// DB. Returns the first indexed `(crc32, size)` row whose `internal_path`
/// matches. The first hit is intentional — we only need *a* plausible CRC
/// candidate so the candidate ranker can prefer CRC-equal local matches.
/// A CRC and size for each of `paths`, from any indexed package that has a
/// file there (the first row found, like a `LIMIT 1` per path). One pass
/// over `resources` for all of them.
fn lookup_crcs_by_paths(
    conn: &rusqlite::Connection,
    paths: &[String],
) -> Result<HashMap<String, (u32, u64)>> {
    lookup_crcs_in_rows(conn, paths, i64::MIN, i64::MAX)
}

/// The same lookup, split into row ranges read at once on the read
/// connections: each reads its share of the table, so the whole takes about
/// a third of the time with three (1.2 s → 0.6 s on 4.9 million rows). Ranges
/// merge in row order, so a path's first row wins as in one query.
fn lookup_crcs_by_paths_split(db: &Db, paths: &[String]) -> Result<HashMap<String, (u32, u64)>> {
    let parts = db.read_parallelism();
    let (lo, hi): (i64, i64) = db.read()?.query_row(
        "SELECT COALESCE(MIN(rowid), 0), COALESCE(MAX(rowid), 0) FROM resources",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if parts <= 1 || hi <= lo {
        let conn = db.read()?;
        return lookup_crcs_by_paths(&conn, paths);
    }
    let step = (hi - lo) / parts as i64 + 1;
    let results: Vec<Result<HashMap<String, (u32, u64)>>> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..parts as i64)
            .map(|i| {
                let first = lo + i * step;
                s.spawn(move || {
                    let conn = db.read()?;
                    lookup_crcs_in_rows(&conn, paths, first, first + step - 1)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_else(|_| Err(anyhow!("a crc32 lookup thread failed"))))
            .collect()
    });
    let mut found = HashMap::new();
    for part in results {
        for (path, hit) in part? {
            found.entry(path).or_insert(hit);
        }
    }
    Ok(found)
}

fn lookup_crcs_in_rows(
    conn: &rusqlite::Connection,
    paths: &[String],
    first: i64,
    last: i64,
) -> Result<HashMap<String, (u32, u64)>> {
    let wanted = serde_json::to_string(paths).context("failed to encode the paths")?;
    let mut stmt = conn
        .prepare(
            "SELECT internal_path, crc32, size FROM resources
             WHERE crc32 IS NOT NULL AND internal_path IN (SELECT value FROM json_each(?1))
               AND rowid BETWEEN ?2 AND ?3",
        )
        .context("failed to prepare the crc32 lookup by internal_path")?;
    let rows = stmt
        .query_map(rusqlite::params![wanted, first, last], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?))
        })
        .context("failed to look up crc32 by internal_path")?;
    let mut found = HashMap::new();
    for row in rows {
        let (path, crc, size) = row.context("failed to read a crc32 row")?;
        found.entry(path).or_insert((crc as u32, size.max(0) as u64));
    }
    Ok(found)
}

pub(crate) fn is_text_path(internal_path: &str) -> bool {
    let Some(ext) = Path::new(internal_path)
        .extension()
        .and_then(|e| e.to_str())
    else {
        return false;
    };
    let lower = format!(".{}", ext.to_ascii_lowercase());
    TEXT_EXTENSIONS.contains(&lower.as_str())
}

fn is_pkg_id_byte(b: u8) -> bool {
    // Non-ASCII bytes (0x80+) are part of a UTF-8 multi-byte sequence —
    // real-world creator segments often include CJK chars (e.g.
    // `Anonymous.VAM灵梦-春庭雪.1`). Treat every UTF-8 lead/continuation byte
    // as a pkg-id byte so the walk-back from `:/` doesn't stop mid-character;
    // the surrounding ASCII separator (`"`, space, `,`) is what terminates
    // the walk. `str::from_utf8` later validates the slice.
    b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-' || b >= 0x80
}

/// Walks a text payload looking for `package_id:/internal/path` substrings.
/// Tolerant of arbitrary surrounding context (quoted JSON strings, free-form
/// scene-saved text, etc.). Package IDs are required to have at least two
/// dots (`Creator.Pkg.Version`) so URLs (`https://...`) and time literals
/// (`12:34`) are filtered out.
pub(crate) fn collect_pkg_refs(text: &str) -> Vec<(String, String)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] != b':' || bytes[i + 1] != b'/' {
            i += 1;
            continue;
        }

        // Walk back to find pkg start.
        let mut start = i;
        while start > 0 && is_pkg_id_byte(bytes[start - 1]) {
            start -= 1;
        }
        if start == i {
            i += 2;
            continue;
        }

        // Walk forward from `:/` to find path end. All TEXT_EXTENSIONS are
        // JSON-like (.json/.vam/.vaj/.vmi/.vap), so the path is always inside
        // a `"..."` string — paths legitimately contain spaces, dashes,
        // brackets, commas, etc. (e.g. `faceG - 23.9.7-2.jpg`). Only stop on
        // string-terminator chars (`"`, `'`) and line breaks, then strip any
        // trailing whitespace that snuck in before the closing quote.
        let mut end = i + 2;
        while end < bytes.len() {
            let c = bytes[end];
            if c == b'"' || c == b'\'' || c == b'\r' || c == b'\n' {
                break;
            }
            end += 1;
        }
        // Trim trailing whitespace (e.g. `"path/foo.jpg "` → `path/foo.jpg`).
        while end > i + 2 {
            let last = bytes[end - 1];
            if last == b' ' || last == b'\t' {
                end -= 1;
            } else {
                break;
            }
        }
        if end <= i + 2 {
            i = end.max(i + 2);
            continue;
        }

        if let (Ok(pkg), Ok(path)) = (
            std::str::from_utf8(&bytes[start..i]),
            std::str::from_utf8(&bytes[i + 2..end]),
        ) {
            if pkg.matches('.').count() >= 2 && !path.is_empty() {
                out.push((pkg.to_string(), path.to_string()));
                if out.len() >= MAX_REFS_PER_FILE {
                    return out;
                }
            }
        }
        i = end;
    }
    out
}

fn find_local_candidates(
    by_path: &HashMap<&str, Vec<(&str, &ResourceRef)>>,
    by_crc: &HashMap<u32, Vec<(&str, &ResourceRef)>>,
    broken_pkg: &str,
    broken_path: &str,
    expected_crc32: Option<u32>,
    expected_size: Option<u64>,
) -> Vec<BrokenRefCandidate> {
    let broken_path_norm = normalize_zip_path(broken_path);
    let mut crc_matches: Vec<ResourceRef> = Vec::new();
    let mut path_matches: Vec<ResourceRef> = Vec::new();

    // CRC (+ size) matches: any resource sharing the expected crc, regardless
    // of internal_path. Pulled straight from the crc index.
    if let Some(crc) = expected_crc32 {
        if let Some(cands) = by_crc.get(&crc) {
            for &(pkg_id, resource) in cands {
                if pkg_id == broken_pkg {
                    continue;
                }
                let size_match = expected_size.is_none_or(|want| want == resource.size);
                if size_match {
                    crc_matches.push(resource.clone());
                }
            }
        }
    }

    // Path matches: resources at the same internal_path that did NOT already
    // qualify as a CRC+size match (mirrors the old `else if` so a resource is
    // never listed in both buckets).
    if let Some(cands) = by_path.get(broken_path_norm.as_str()) {
        for &(pkg_id, resource) in cands {
            if pkg_id == broken_pkg {
                continue;
            }
            let is_crc_match = expected_crc32.is_some()
                && expected_crc32 == resource.crc32
                && expected_size.is_none_or(|want| want == resource.size);
            if is_crc_match {
                continue;
            }
            path_matches.push(resource.clone());
        }
    }

    // CRC matches first, then path matches.
    crc_matches.extend(path_matches);
    crc_matches.truncate(MAX_LOCAL_CANDIDATES);

    crc_matches
        .into_iter()
        .map(|resource| BrokenRefCandidate { resource })
        .collect()
}

/// The package a fix points at, as written into the rewritten reference.
/// The target itself (by its own id, or `SELF`) is written `SELF`, VaM's
/// spelling for "this package"; the bool says whether the package must be a
/// dependency (never for `SELF`, or the package would depend on itself).
fn replacement_target(target_id: &str, replacement_pkg: &str) -> (String, bool) {
    if replacement_pkg.eq_ignore_ascii_case("SELF") || replacement_pkg.eq_ignore_ascii_case(target_id) {
        ("SELF".to_string(), false)
    } else {
        (replacement_pkg.to_string(), true)
    }
}

/// Applies a batch of user-approved fixes to a target VAR. Rewrites
/// `Pkg:/path` references in all text payloads, then updates `meta.json`
/// dependencies: each replacement package is inserted; each broken package
/// is removed if no rewritten text still references it.
///
/// `output_path` controls where the rewritten .var lands:
/// - `None` → overwrite the original `target_path` (atomic staging rename).
/// - `Some(path)` → write to `path` (parent dirs are created); the original
///   `target_path` is left untouched. Used for the Missing Resources page's
///   "Output Folder" workflow so the user can review the fixed .var before
///   replacing the original.
pub(crate) fn apply_fix_var(
    target_path: &Path,
    output_path: Option<&Path>,
    fixes: &[FixDirective],
    scan: &ScannedData,
    db: &Db,
    backup_root: Option<&Path>,
) -> Result<FixReport> {
    let mut report = FixReport::default();
    if fixes.is_empty() {
        return Ok(report);
    }

    let mut replacements: BTreeMap<String, String> = BTreeMap::new();
    let mut broken_pkgs_with_path: BTreeSet<String> = BTreeSet::new();
    let mut meta_only_broken: BTreeSet<String> = BTreeSet::new();
    let mut replacement_pkgs: BTreeMap<String, Option<String>> = BTreeMap::new();
    let target_id = target_path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.trim_end_matches(".var").to_string())
        .unwrap_or_default();

    for fix in fixes {
        let (replacement_pkg, needs_dependency) = replacement_target(&target_id, &fix.replacement_pkg);
        if let Some(path) = &fix.broken_path {
            // Use the candidate's actual internal path when the user picked a
            // candidate whose path differs from the broken ref's path. Without
            // this, swapping only the package prefix produces a brand-new
            // broken reference (right pkg, wrong path).
            let new_path = fix.replacement_path.as_deref().unwrap_or(path.as_str());
            let old = format!("{}:/{}", fix.broken_pkg, path);
            let new = format!("{}:/{}", replacement_pkg, new_path);
            if old != new {
                replacements.insert(old, new);
            }
            broken_pkgs_with_path.insert(fix.broken_pkg.clone());
        } else {
            meta_only_broken.insert(fix.broken_pkg.clone());
        }
        if needs_dependency {
            replacement_pkgs
                .entry(replacement_pkg)
                .or_insert(fix.replacement_license_type.clone());
        }
    }

    if let Some(backup_root) = backup_root {
        fs::create_dir_all(backup_root)
            .with_context(|| format!("failed to create backup dir {}", backup_root.display()))?;
        let file_name = target_path
            .file_name()
            .ok_or_else(|| anyhow!("invalid target file name"))?;
        // Never over an earlier backup of the same package.
        let backup_path = free_backup_path(backup_root, &file_name.to_string_lossy());
        fs::copy(target_path, &backup_path).with_context(|| {
            format!(
                "failed to back up {} → {}",
                target_path.display(),
                backup_path.display()
            )
        })?;
        report.backup_path = Some(backup_path.display().to_string());
    }

    let reader = fs::File::open(target_path)
        .with_context(|| format!("failed to open {}", target_path.display()))?;
    let mut source = ZipArchive::new(reader)
        .with_context(|| format!("failed to read zip archive {}", target_path.display()))?;

    let file_name = target_path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow!("invalid target file name {}", target_path.display()))?;

    // Two modes:
    // - replace-in-place (output_path is None): write to a sibling .fix-tmp
    //   then atomic-rename over the original.
    // - output-folder (output_path is Some): create the destination directory,
    //   write straight to it. Leave the original untouched.
    let (write_to, atomic_rename_target) = if let Some(dest) = output_path {
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create output dir {}", parent.display()))?;
        }
        (dest.to_path_buf(), None)
    } else {
        let staging = target_path.with_file_name(format!("{file_name}.fix-tmp"));
        (staging, Some(target_path.to_path_buf()))
    };

    // Stream entries straight from source → target. Binary payloads are
    // raw-copied (no decompress/re-compress — the slow part). Only text
    // entries are decoded; only meta.json is buffered so it can be rewritten
    // *after* we know which broken pkgs survived the text rewrite.
    let mut still_references_pkg: BTreeSet<String> = BTreeSet::new();
    let mut meta_entry: Option<(String, Vec<u8>)> = None;

    {
        let writer = fs::File::create(&write_to).with_context(|| {
            format!("failed to create output file {}", write_to.display())
        })?;
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

            if internal_path == META_PATH {
                let mut raw = Vec::new();
                entry.read_to_end(&mut raw)?;
                meta_entry = Some((raw_name, raw));
                continue;
            }

            if is_text_path(&internal_path) {
                let mut raw = Vec::new();
                entry.read_to_end(&mut raw)?;
                if !replacements.is_empty() {
                    if let Some(updated) = rewrite_external_text_payload(&raw, &replacements) {
                        raw = updated;
                        report.files_rewritten += 1;
                    }
                }
                if let Some((text, _)) = decode_text(&raw) {
                    for pkg in &broken_pkgs_with_path {
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
                // Binary entry — no need to inspect; pass through raw so the
                // compressed bytes are copied verbatim. This is the major
                // win versus the previous decompress-everything loop.
                target.raw_copy_file(entry)?;
            }
        }

        // Now finalize meta.json with the dependency edits and append it.
        if let Some((raw_name, raw)) = meta_entry.take() {
            let mut meta = read_json_bytes(&raw, "meta.json")?;
            let mut deps_obj = match meta.get("dependencies").cloned() {
                Some(Value::Object(m)) => m,
                _ => serde_json::Map::new(),
            };

            for pkg in &broken_pkgs_with_path {
                if still_references_pkg.contains(pkg) {
                    continue;
                }
                if deps_obj.remove(pkg).is_some() {
                    report.dependencies_removed.push(pkg.clone());
                }
            }
            for pkg in &meta_only_broken {
                if deps_obj.remove(pkg).is_some() {
                    report.dependencies_removed.push(pkg.clone());
                }
            }
            for (pkg, license_hint) in &replacement_pkgs {
                if deps_obj.contains_key(pkg) {
                    continue;
                }
                let license_type = license_hint
                    .clone()
                    .or_else(|| scan.packages.get(pkg).map(|p| p.license_type.clone()))
                    .or_else(|| lookup_license_in_db(db, pkg).ok().flatten())
                    .unwrap_or_else(|| "PC EA".to_string());
                deps_obj.insert(
                    pkg.clone(),
                    json!({
                        "licenseType": license_type,
                        "dependencies": {}
                    }),
                );
                report.dependencies_added.push(pkg.clone());
            }
            meta["dependencies"] = Value::Object(deps_obj);
            let new_raw = dump_json_bytes(&meta)?;
            report.files_rewritten += 1;

            target.start_file(&raw_name, options)?;
            target.write_all(&new_raw)?;
        }

        target.finish()?;
    }
    drop(source);

    let final_path = if let Some(final_target) = atomic_rename_target {
        if final_target.exists() {
            fs::remove_file(&final_target).with_context(|| {
                format!("failed to remove old {}", final_target.display())
            })?;
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

    report.fixes_applied = fixes.len() as u32;
    report.output_path = Some(final_path.display().to_string());
    Ok(report)
}

fn lookup_license_in_db(db: &Db, _package_id: &str) -> Result<Option<String>> {
    let _conn = db
        .conn
        .lock()
        .map_err(|_| anyhow!("database connection poisoned"))?;
    // The DB currently doesn't persist `license_type` for packages — only
    // resources track their own metadata. The lookup is a no-op for now;
    // kept as a hook so a future migration that adds the column can fill it
    // in without touching the fix path.
    Ok(None)
}

/// `dir/name`, or `dir/<stem> (2).var` and so on when that's taken, so an
/// earlier backup is never overwritten.
pub(crate) fn free_backup_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let stem = name.strip_suffix(".var").unwrap_or(name);
    (2..)
        .map(|n| dir.join(format!("{stem} ({n}).var")))
        .find(|p| !p.exists())
        .unwrap_or(first)
}

#[cfg(test)]
mod tests {
    use super::{free_backup_path, lookup_crcs_by_paths, lookup_crcs_by_paths_split, replacement_target};
    use std::fs;

    #[test]
    fn crcs_split_over_the_read_connections_match_one_query() {
        let dir = std::env::temp_dir().join(format!("vvd_split_lookup_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("dir");
        let db = crate::db::open_file(&dir.join("t.db")).expect("open db");
        assert!(db.read_parallelism() > 1, "the file database has read connections");
        {
            let conn = db.conn.lock().expect("lock db");
            crate::db::upsert_package(&conn, "A.Pack.1", "C:/a.var", 1, 1, None).expect("a");
            crate::db::upsert_package(&conn, "B.Pack.1", "C:/b.var", 1, 1, None).expect("b");
            // Many rows, so the ranges each hold some; x.png twice with
            // different contents: the first row must win, as in one query.
            let mut sql = String::from("INSERT INTO resources (package_id, internal_path, crc32, size, effective_size) VALUES ");
            let rows: Vec<String> = (0..300)
                .map(|i| match i {
                    10 => "('A.Pack.1', 'x.png', 111, 1, 1)".to_string(),
                    250 => "('B.Pack.1', 'x.png', 222, 2, 2)".to_string(),
                    200 => "('A.Pack.1', 'y.png', 333, 3, 3)".to_string(),
                    _ => format!("('A.Pack.1', 'f{i}.json', {i}, 1, 1)"),
                })
                .collect();
            sql.push_str(&rows.join(","));
            conn.execute_batch(&sql).expect("rows");
        }
        let paths = vec!["x.png".to_string(), "y.png".to_string(), "f42.json".to_string(), "nope.png".to_string()];
        let one = lookup_crcs_by_paths(&db.read().expect("read"), &paths).expect("one");
        let split = lookup_crcs_by_paths_split(&db, &paths).expect("split");
        assert_eq!(split, one);
        assert_eq!(split.get("x.png"), Some(&(111, 1)));
        assert_eq!(split.len(), 3);
        drop(db);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn crcs_for_many_paths_in_one_pass() {
        let db = crate::db::open_in_memory().expect("open db");
        let conn = db.conn.lock().expect("lock db");
        crate::db::upsert_package(&conn, "A.Pack.1", "C:/a.var", 1, 1, None).expect("a");
        crate::db::upsert_package(&conn, "B.Pack.1", "C:/b.var", 1, 1, None).expect("b");
        conn.execute_batch(
            "INSERT INTO resources (package_id, internal_path, crc32, size, effective_size) VALUES
               ('A.Pack.1', 'Custom/x.vmi', 11, 100, 100),
               ('B.Pack.1', 'Custom/x.vmi', 11, 100, 100),
               ('B.Pack.1', 'Custom/y.png', 22, 200, 200),
               ('B.Pack.1', 'Custom/no-crc.json', NULL, 5, 5);",
        )
        .expect("seed");
        let paths = ["Custom/x.vmi", "Custom/y.png", "Custom/no-crc.json", "Custom/nowhere.cs"].map(String::from);
        let found = lookup_crcs_by_paths(&conn, &paths).expect("lookup");
        assert_eq!(found.get("Custom/x.vmi"), Some(&(11, 100)));
        assert_eq!(found.get("Custom/y.png"), Some(&(22, 200)));
        assert!(!found.contains_key("Custom/no-crc.json"));
        assert!(!found.contains_key("Custom/nowhere.cs"));
    }

    #[test]
    fn a_backup_never_overwrites_an_earlier_one() {
        let dir = std::env::temp_dir().join(format!("vam_backup_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(free_backup_path(&dir, "Abie.A35.1.var"), dir.join("Abie.A35.1.var"));
        fs::write(dir.join("Abie.A35.1.var"), b"first").unwrap();
        assert_eq!(free_backup_path(&dir, "Abie.A35.1.var"), dir.join("Abie.A35.1 (2).var"));
        fs::write(dir.join("Abie.A35.1 (2).var"), b"second").unwrap();
        assert_eq!(free_backup_path(&dir, "Abie.A35.1.var"), dir.join("Abie.A35.1 (3).var"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_copy_inside_the_target_is_a_self_reference() {
        assert_eq!(replacement_target("Abie.A35.1", "Abie.A35.1"), ("SELF".to_string(), false));
        assert_eq!(replacement_target("Abie.A35.1", "abie.a35.1"), ("SELF".to_string(), false));
        assert_eq!(replacement_target("Abie.A35.1", "SELF"), ("SELF".to_string(), false));
        assert_eq!(replacement_target("Abie.A35.1", "Other.Pack.3"), ("Other.Pack.3".to_string(), true));
    }
}

/// The missing-file check's results for the last few packages, reused while
/// nothing they depend on has changed: the package file itself, the folders'
/// packages (the scan's file list) and the database (its write counters). So
/// Check again, or going between Fix Missing and Internalize, doesn't repeat
/// it.
#[derive(Default)]
pub(crate) struct BrokenRefsCache {
    entries: Vec<(u64, Vec<BrokenRef>)>,
}

const BROKEN_REFS_CACHE_SIZE: usize = 8;

#[cfg(test)]
impl BrokenRefsCache {
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

fn broken_refs_key(target_path: &Path, scan: &ScannedData, db: &Db) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    let meta = fs::metadata(target_path).ok()?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    target_path.hash(&mut h);
    meta.len().hash(&mut h);
    meta.modified().ok().hash(&mut h);
    for f in &scan.files {
        f.relative_path.hash(&mut h);
        f.size.hash(&mut h);
        f.modified_ns.hash(&mut h);
    }
    // Writes through the app's connection, and commits by any other one.
    let conn = db.conn.lock().ok()?;
    let changes: i64 = conn.query_row("SELECT total_changes()", [], |r| r.get(0)).ok()?;
    let version: i64 = conn.query_row("PRAGMA data_version", [], |r| r.get(0)).ok()?;
    (changes, version).hash(&mut h);
    Some(h.finish())
}

pub(crate) fn scan_broken_refs_cached(
    cache: &Mutex<BrokenRefsCache>,
    target_path: &Path,
    scan: &ScannedData,
    db: &Db,
) -> Result<Vec<BrokenRef>> {
    let key = broken_refs_key(target_path, scan, db);
    if let (Some(key), Ok(guard)) = (key, cache.lock()) {
        if let Some((_, refs)) = guard.entries.iter().find(|(k, _)| *k == key) {
            return Ok(refs.clone());
        }
    }
    let refs = scan_target_var_for_broken_refs(target_path, scan, db)?;
    if let (Some(key), Ok(mut guard)) = (key, cache.lock()) {
        guard.entries.retain(|(k, _)| *k != key);
        guard.entries.push((key, refs.clone()));
        if guard.entries.len() > BROKEN_REFS_CACHE_SIZE {
            guard.entries.remove(0);
        }
    }
    Ok(refs)
}
