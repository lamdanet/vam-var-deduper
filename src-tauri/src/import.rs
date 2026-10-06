use std::{
    collections::HashMap,
    fs::File,
    io::{BufRead, BufReader},
    path::Path,
};

use anyhow::{anyhow, Context, Result};

use crate::{
    db::{self, Db},
    models::BulkImportResponse,
};

/// Owned parse result, kept for tests.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone)]
pub(crate) struct ParsedLine {
    pub(crate) package_id: String,
    pub(crate) internal_path: String,
    pub(crate) crc32: u32,
}

/// Borrowed parse result used on the hot import path. Slices into the line
/// buffer that the caller owns — zero allocations per line.
#[derive(Debug)]
struct ParsedSlice<'a> {
    package_id: &'a str,
    internal_path: &'a str,
    crc32: u32,
}

/// Parses a manifest line into slices of `line`. Returns `None` for blank or
/// malformed lines.
fn parse_manifest_line_slices(line: &str) -> Option<ParsedSlice<'_>> {
    let s = line.trim();
    if s.is_empty() {
        return None;
    }

    // Optional `[N]` prefix
    let s = if let Some(after_open) = s.strip_prefix('[') {
        let close = after_open.find(']')?;
        after_open[close + 1..].trim_start()
    } else {
        s
    };

    // CRC32: exactly 8 hex chars, terminated by whitespace
    let crc_end = s.find(char::is_whitespace)?;
    let crc_str = &s[..crc_end];
    if crc_str.len() != 8 {
        return None;
    }
    let crc32 = u32::from_str_radix(crc_str, 16).ok()?;

    let mut rest = s[crc_end..].trim_start();

    // Optional `||| display name |||`
    if let Some(after_open) = rest.strip_prefix("|||") {
        let close = after_open.find("|||")?;
        rest = after_open[close + 3..].trim_start();
    }

    // package_id : / internal_path
    let colon = rest.find(':')?;
    let package_id = rest[..colon].trim();
    // Manifest format uses forward slashes; trim leading/trailing `/` to match
    // `normalize_zip_path` output stored by the scanner.
    let internal_path = rest[colon + 1..].trim().trim_matches('/');

    if package_id.is_empty() || internal_path.is_empty() {
        return None;
    }

    Some(ParsedSlice {
        package_id,
        internal_path,
        crc32,
    })
}

/// Parses a manifest line into an owned `ParsedLine`. Allocates Strings; used
/// only by tests. The hot path uses `parse_manifest_line_slices` directly.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn parse_manifest_line(line: &str) -> Option<ParsedLine> {
    parse_manifest_line_slices(line).map(|p| ParsedLine {
        package_id: p.package_id.to_string(),
        internal_path: p.internal_path.to_string(),
        crc32: p.crc32,
    })
}

/// Emit a progress update at most once per ~0.1% of total bytes consumed.
/// Avoids hammering the task lock for very large manifests.
const PROGRESS_BUCKET_RESOLUTION: i32 = 1000;

pub(crate) fn import_manifest<F>(
    path: &Path,
    db: &Db,
    mut on_progress: F,
) -> Result<BulkImportResponse>
where
    F: FnMut(f64, String),
{
    let metadata = std::fs::metadata(path)
        .with_context(|| format!("failed to stat manifest file {}", path.display()))?;
    let total_bytes = metadata.len().max(1);

    let file = File::open(path)
        .with_context(|| format!("failed to open manifest file {}", path.display()))?;
    let mut reader = BufReader::with_capacity(64 * 1024, file);

    on_progress(0.0, "Opening manifest…".to_string());

    let category_ids = db.categories.clone();
    let mut conn = db
        .conn
        .lock()
        .map_err(|_| anyhow!("database connection poisoned"))?;
    let tx = conn
        .transaction()
        .context("failed to start manifest import tx")?;

    let mut lines_total: u64 = 0;
    let mut lines_skipped: u64 = 0;
    let mut lines_parsed: u64 = 0;
    let mut bytes_consumed: u64 = 0;
    let mut packages_inserted: u64 = 0;
    let mut packages_existing: u64 = 0;
    let mut resources_inserted: u64 = 0;
    let mut resources_skipped_existing: u64 = 0;
    let mut last_emitted_bucket = -1i32;

    // ~10 k unique packages is typical for a 4M-line manifest. The map value
    // doubles as a creator-id cache so the second package by the same creator
    // is a HashMap hit, not a SQL lookup.
    let mut seen_packages: HashMap<String, Option<i64>> = HashMap::with_capacity(16_384);
    let mut creator_cache: HashMap<String, i64> = HashMap::new();

    {
        let mut pkg_stmt = tx
            .prepare(db::MANIFEST_PACKAGE_INSERT_SQL)
            .context("failed to prepare manifest package insert")?;
        let mut res_stmt = tx
            .prepare(db::MANIFEST_RESOURCE_INSERT_SQL)
            .context("failed to prepare manifest resource insert")?;

        // Reused line buffer — avoids 1 allocation per line vs `reader.lines()`.
        let mut line_buf = String::with_capacity(256);

        loop {
            line_buf.clear();
            let read = reader
                .read_line(&mut line_buf)
                .context("failed to read manifest line")?;
            if read == 0 {
                break;
            }
            lines_total += 1;
            bytes_consumed = bytes_consumed.saturating_add(read as u64);

            match parse_manifest_line_slices(&line_buf) {
                Some(parsed) => {
                    // Insert the package row only the first time we see this
                    // package_id within this run — saves N-1 redundant
                    // INSERT-OR-IGNORE roundtrips per package.
                    if !seen_packages.contains_key(parsed.package_id) {
                        let creator_id = match crate::naming::creator_from_package_id(
                            parsed.package_id,
                        ) {
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
                        let changed = db::execute_manifest_package_insert(
                            &mut pkg_stmt,
                            parsed.package_id,
                            creator_id,
                        )?;
                        if changed > 0 {
                            packages_inserted += 1;
                        } else {
                            packages_existing += 1;
                        }
                        seen_packages.insert(parsed.package_id.to_string(), creator_id);
                    }

                    let changed = db::execute_manifest_resource_insert(
                        &mut res_stmt,
                        parsed.package_id,
                        parsed.internal_path,
                        parsed.crc32,
                        &category_ids,
                    )?;
                    if changed > 0 {
                        resources_inserted += 1;
                    } else {
                        resources_skipped_existing += 1;
                    }
                    lines_parsed += 1;
                }
                None => {
                    lines_skipped += 1;
                }
            }

            // Emit progress at most once per 0.1% of file consumed (and never
            // more often than every 25k lines on huge manifests).
            if lines_total % 25_000 == 0 {
                let pct = (bytes_consumed as f64 / total_bytes as f64).min(1.0);
                let bucket = (pct * PROGRESS_BUCKET_RESOLUTION as f64) as i32;
                if bucket != last_emitted_bucket {
                    last_emitted_bucket = bucket;
                    on_progress(
                        pct * 0.99,
                        format!("Importing ({lines_parsed} / {lines_total} lines)"),
                    );
                }
            }
        }
    }

    on_progress(0.99, "Committing transaction…".to_string());
    tx.commit().context("failed to commit manifest import")?;
    on_progress(1.0, "Manifest import completed".to_string());

    Ok(BulkImportResponse {
        lines_total,
        lines_parsed,
        lines_skipped,
        packages_inserted,
        packages_existing,
        resources_inserted,
        resources_skipped_existing,
    })
}
