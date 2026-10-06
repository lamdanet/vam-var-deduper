use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use anyhow::{anyhow, Context, Result};
use encoding_rs::{UTF_16BE, UTF_16LE};
use serde_json::Value;
use walkdir::WalkDir;

use crate::models::{TextEncoding, VarFileEntry, VarFileFingerprint};

pub(crate) fn normalize_zip_path(path: &str) -> String {
    path.replace('\\', "/").trim_matches('/').to_string()
}

/// Cache key for a scan spanning one or more roots. The roots are canonicalized
/// and sorted so the key is independent of the order they were supplied in —
/// the scan and execute paths must produce byte-identical keys for the cache to
/// hit. With a single root the output is identical to the historical
/// `{input}|{target}` format, preserving existing one-root cache semantics.
pub(crate) fn scan_cache_key_multi(roots: &[&Path], target_var_path: Option<&Path>) -> String {
    let mut inputs = roots
        .iter()
        .map(|root| {
            fs::canonicalize(root)
                .unwrap_or_else(|_| root.to_path_buf())
                .display()
                .to_string()
        })
        .collect::<Vec<_>>();
    inputs.sort();
    let target = target_var_path
        .map(|path| fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()))
        .map(|path| path.display().to_string())
        .unwrap_or_default();
    format!("{}|{}", inputs.join("\n"), target)
}

/// How far down a root a `.var` walk should reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScanDepth {
    /// Every `.var` under the root, at any depth. The historical behavior, and
    /// what every path except the VAR Packages listing uses.
    Recursive,
    /// Only `.var` files sitting directly in the root — subfolders are not
    /// descended into at all.
    TopLevelOnly,
}

impl ScanDepth {
    /// `true` -> Recursive, mirroring the `deep_scan` flag the UI sends.
    pub(crate) fn from_deep(deep: bool) -> Self {
        if deep {
            Self::Recursive
        } else {
            Self::TopLevelOnly
        }
    }

    /// Stable token for cache keys. A depth change must invalidate a cached
    /// listing, or switching modes would re-serve the previous walk.
    pub(crate) fn cache_token(self) -> &'static str {
        match self {
            Self::Recursive => "deep",
            Self::TopLevelOnly => "top",
        }
    }
}

pub(crate) fn collect_var_files_with_depth(
    input_dir: &Path,
    depth: ScanDepth,
) -> Result<Vec<VarFileEntry>> {
    fs::read_dir(input_dir)
        .with_context(|| format!("failed to read input directory {}", input_dir.display()))?;

    let walker = match depth {
        ScanDepth::Recursive => WalkDir::new(input_dir),
        // Depth 0 is the root itself, so 1 is exactly its direct children.
        ScanDepth::TopLevelOnly => WalkDir::new(input_dir).max_depth(1),
    };

    let mut var_files = walker
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry.file_type().is_file()
                && entry
                    .path()
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .map(|ext| ext.eq_ignore_ascii_case("var"))
                    .unwrap_or(false)
        })
        .map(|entry| {
            let path = entry.into_path();
            let metadata = fs::metadata(&path)
                .with_context(|| format!("failed to read metadata for {}", path.display()))?;
            let modified = metadata
                .modified()
                .with_context(|| format!("failed to read modified time for {}", path.display()))?;
            let modified_ns = modified
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let relative_path = path
                .strip_prefix(input_dir)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");

            Ok(VarFileEntry {
                path,
                fingerprint: VarFileFingerprint {
                    relative_path,
                    size: metadata.len(),
                    modified_ns,
                },
            })
        })
        .collect::<Result<Vec<_>>>()?;

    var_files.sort_by(|a, b| {
        a.fingerprint
            .relative_path
            .cmp(&b.fingerprint.relative_path)
    });
    Ok(var_files)
}

pub(crate) fn build_var_file_entry(base_dir: &Path, path: PathBuf) -> Result<VarFileEntry> {
    let metadata = fs::metadata(&path)
        .with_context(|| format!("failed to read metadata for {}", path.display()))?;
    let modified = metadata
        .modified()
        .with_context(|| format!("failed to read modified time for {}", path.display()))?;
    let modified_ns = modified
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let relative_path = path
        .strip_prefix(base_dir)
        .unwrap_or(&path)
        .to_string_lossy()
        .replace('\\', "/");

    Ok(VarFileEntry {
        path,
        fingerprint: VarFileFingerprint {
            relative_path,
            size: metadata.len(),
            modified_ns,
        },
    })
}

/// Collect .var files across multiple roots, deduped by canonical path (first
/// root wins on collision), keeping the root each file was found under.
///
/// The owning root is what lets Organize by Creator regroup each file relative
/// to its *own* root, so every `fs::rename` stays on one volume — `fs::rename`
/// fails across volumes, and a copy+delete fallback is where data loss lives.
pub(crate) fn collect_var_files_by_root(roots: &[PathBuf]) -> Result<Vec<(PathBuf, VarFileEntry)>> {
    collect_var_files_by_root_with_depth(roots, ScanDepth::Recursive)
}

pub(crate) fn collect_var_files_by_root_with_depth(
    roots: &[PathBuf],
    depth: ScanDepth,
) -> Result<Vec<(PathBuf, VarFileEntry)>> {
    let mut var_files: Vec<(PathBuf, VarFileEntry)> = Vec::new();
    let mut seen_paths: BTreeSet<PathBuf> = BTreeSet::new();
    for root in roots {
        for entry in collect_var_files_with_depth(root, depth)? {
            let canonical = fs::canonicalize(&entry.path).unwrap_or_else(|_| entry.path.clone());
            if seen_paths.insert(canonical) {
                var_files.push((root.clone(), entry));
            }
        }
    }

    var_files.sort_by(|a, b| {
        a.1.fingerprint
            .relative_path
            .cmp(&b.1.fingerprint.relative_path)
    });
    Ok(var_files)
}

/// Collect .var files across multiple roots, deduped by canonical path (first
/// root wins on collision). `relative_path` is stripped per its own root and is
/// only a sort key downstream, so cross-root relative_path collisions are
/// harmless. Entries are sorted once at the end.
pub(crate) fn collect_var_files_multi(roots: &[PathBuf]) -> Result<Vec<VarFileEntry>> {
    collect_var_files_multi_with_depth(roots, ScanDepth::Recursive)
}

pub(crate) fn collect_var_files_multi_with_depth(
    roots: &[PathBuf],
    depth: ScanDepth,
) -> Result<Vec<VarFileEntry>> {
    Ok(collect_var_files_by_root_with_depth(roots, depth)?
        .into_iter()
        .map(|(_root, entry)| entry)
        .collect())
}

pub(crate) fn collect_var_files_with_targets(
    roots: &[PathBuf],
    target_var_path: Option<&Path>,
) -> Result<Vec<VarFileEntry>> {
    let mut var_files = collect_var_files_multi(roots)?;
    let mut seen_paths = var_files
        .iter()
        .filter_map(|entry| fs::canonicalize(&entry.path).ok())
        .collect::<BTreeSet<_>>();

    if let Some(target_var_path) = target_var_path {
        let canonical_target = fs::canonicalize(target_var_path)
            .with_context(|| format!("failed to resolve {}", target_var_path.display()))?;
        let is_var = canonical_target
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("var"))
            .unwrap_or(false);
        if !is_var {
            return Err(anyhow!(
                "target VAR path is not a .var file: {}",
                canonical_target.display()
            ));
        }
        if !seen_paths.contains(&canonical_target) {
            // relative_path is only a sort key, so the base dir choice is
            // cosmetic — use the primary root (or the target itself if no roots).
            let base = roots.first().map(PathBuf::as_path).unwrap_or(&canonical_target);
            var_files.push(build_var_file_entry(base, canonical_target.clone())?);
            seen_paths.insert(canonical_target);
        }
    }

    var_files.sort_by(|a, b| {
        a.fingerprint
            .relative_path
            .cmp(&b.fingerprint.relative_path)
    });
    Ok(var_files)
}

pub(crate) fn decode_text(data: &[u8]) -> Option<(String, TextEncoding)> {
    if data.starts_with(&[0xEF, 0xBB, 0xBF]) {
        let text = String::from_utf8(data[3..].to_vec()).ok()?;
        return Some((text, TextEncoding::Utf8Bom));
    }

    if let Ok(text) = String::from_utf8(data.to_vec()) {
        return Some((text, TextEncoding::Utf8));
    }

    if data.len() >= 2 {
        if data.starts_with(&[0xFF, 0xFE]) {
            let (decoded, _, had_errors) = UTF_16LE.decode(&data[2..]);
            if !had_errors {
                return Some((decoded.into_owned(), TextEncoding::Utf16Le));
            }
        }
        if data.starts_with(&[0xFE, 0xFF]) {
            let (decoded, _, had_errors) = UTF_16BE.decode(&data[2..]);
            if !had_errors {
                return Some((decoded.into_owned(), TextEncoding::Utf16Be));
            }
        }

        let (decoded_le, _, had_errors_le) = UTF_16LE.decode(data);
        if !had_errors_le {
            return Some((decoded_le.into_owned(), TextEncoding::Utf16Le));
        }

        let (decoded_be, _, had_errors_be) = UTF_16BE.decode(data);
        if !had_errors_be {
            return Some((decoded_be.into_owned(), TextEncoding::Utf16Be));
        }
    }

    None
}

pub(crate) fn encode_text(text: &str, encoding: &TextEncoding) -> Vec<u8> {
    match encoding {
        TextEncoding::Utf8 | TextEncoding::Utf8Bom => text.as_bytes().to_vec(),
        TextEncoding::Utf16Le => {
            let (encoded, _, _) = UTF_16LE.encode(text);
            encoded.into_owned()
        }
        TextEncoding::Utf16Be => {
            let (encoded, _, _) = UTF_16BE.encode(text);
            encoded.into_owned()
        }
    }
}

pub(crate) fn strip_trailing_commas(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let chars = text.chars().collect::<Vec<_>>();
    let mut index = 0;
    let mut in_string = false;
    let mut escaped = false;

    while index < chars.len() {
        let ch = chars[index];
        if in_string {
            result.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            index += 1;
            continue;
        }

        if ch == '"' {
            in_string = true;
            result.push(ch);
            index += 1;
            continue;
        }

        if ch == ',' {
            let mut lookahead = index + 1;
            while lookahead < chars.len() && chars[lookahead].is_whitespace() {
                lookahead += 1;
            }
            if lookahead < chars.len() && matches!(chars[lookahead], '}' | ']') {
                index += 1;
                continue;
            }
        }

        result.push(ch);
        index += 1;
    }

    result
}

pub(crate) fn read_json_bytes(data: &[u8], context: &str) -> Result<Value> {
    let (text, _) =
        decode_text(data).ok_or_else(|| anyhow!("{context} is not a recognized text JSON file"))?;
    match serde_json::from_str(&text) {
        Ok(value) => Ok(value),
        Err(primary_error) => {
            let normalized = strip_trailing_commas(&text);
            if normalized == text {
                Err(primary_error).with_context(|| format!("failed to parse JSON from {context}"))
            } else {
                serde_json::from_str(&normalized)
                    .with_context(|| format!("failed to parse JSON from {context}"))
            }
        }
    }
}

pub(crate) fn dump_json_bytes(payload: &Value) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec_pretty(payload)?)
}

pub(crate) fn format_bytes(size: u64) -> String {
    let units = ["B", "KB", "MB", "GB"];
    let mut value = size as f64;
    for unit in units {
        if value < 1024.0 || unit == "GB" {
            return format!("{value:.1} {unit}");
        }
        value /= 1024.0;
    }
    format!("{size} B")
}
