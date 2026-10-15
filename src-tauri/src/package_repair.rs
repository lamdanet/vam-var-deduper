//! Repair a package's own bookkeeping without changing what it does in VaM.
//!
//! Repacked packages (common with Chinese creators) often carry a meta.json
//! that no longer matches their content: dependencies nothing inside uses,
//! the package listing itself, and nested copies of other packages' trees.
//! VaM is quiet about those, but every dependency view counts them as
//! missing. Three findings, each fixable:
//!
//! - **Dependencies it doesn't use**: declared in meta.json, but no scene,
//!   preset or other text file inside refers to that package (or only to
//!   itself). Removing them changes nothing in VaM.
//! - **References to files it carries itself**: `Other.Pkg.1:/path` where
//!   `path` is inside this package. Pointed at its own copy (`SELF:/path`),
//!   which is what VaM loads when the other package is missing anyway; the
//!   dependency is then unused too.
//! - **File names VaM reads wrongly**: names stored without the zip UTF-8
//!   flag (old Chinese / Japanese zip tools write GBK or Shift-JIS). VaM reads
//!   them as code page 437, so a scene shows as `╥∞╜τ…` or not at all. Written
//!   back as UTF-8 with the flag.
//! - **meta.json itself**: a creator or package name that isn't the one in
//!   the file name, packages its files use but it doesn't list, a byte-order
//!   mark or JSON only a lenient reader accepts.
//!
//! The Repair VARs page checks one package, or the whole library at once
//! (`start_repair_scan_task`), and repairs one or many
//! (`start_repair_packages_task`).

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    thread,
};

use anyhow::{anyhow, Context, Result};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::State;
use zip::{write::SimpleFileOptions, CompressionMethod, System, ZipArchive, ZipWriter};

use crate::{
    execute::rewrite_external_text_payload,
    fix_var::{collect_pkg_refs, free_backup_path, is_text_path},
    models::{AppState, ProgressPayload, TaskHandle, VarPackageListItem, META_PATH},
    tasks::{new_progress_payload, set_task_progress},
    utils::{byte_step, copy_with_progress, decode_text, dump_json_bytes, normalize_zip_path, read_json_bytes, Progress},
};

/// The error when a package has nothing the request would change; a batch
/// counts it as skipped, not failed.
const NOTHING_TO_REPAIR: &str = "Nothing to repair.";

/// Outside references listed per dependency, as examples.
const SAMPLE_CAP: usize = 4;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct RepairDep {
    /// The key as meta.json has it.
    pub(crate) key: String,
    /// `"self"` (the package itself) | `"unused"` (nothing inside refers to
    /// it) | `"covered"` (every reference is to a file this package has) |
    /// `"needed"` (some reference is to a file only it has).
    pub(crate) status: String,
    pub(crate) refs: u32,
    /// References to files this package also has.
    pub(crate) inside: u32,
    /// The other references, by what VaM does with them when it's missing:
    /// plugins fail with an error, morphs are skipped quietly.
    pub(crate) plugins: u32,
    pub(crate) morphs: u32,
    pub(crate) others: u32,
    /// Times its name appears without a path, as a plugin's code does when it
    /// loads the package itself (`GetLatestPackageUid("Creator.Name")`).
    /// Any of these keeps it: what the code does with it can't be seen.
    pub(crate) named: u32,
    /// Packages its meta.json nests under this one (a snapshot of that
    /// package's own tree).
    pub(crate) nested: u32,
    pub(crate) samples: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct RepairUndeclared {
    pub(crate) package: String,
    pub(crate) refs: u32,
    pub(crate) inside: u32,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct RepairName {
    /// What VaM reads (the stored bytes as code page 437).
    pub(crate) stored: String,
    /// The name it was meant to have.
    pub(crate) fixed: String,
}

/// One thing wrong with meta.json itself.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct MetaIssue {
    /// `"creator"` | `"package_name"` | `"undeclared"` | `"format"` (fixable);
    /// `"license"` | `"missing"` (only reported).
    pub(crate) code: String,
    pub(crate) fixable: bool,
    /// What meta.json has, and what the repair writes (when fixable).
    pub(crate) found: String,
    pub(crate) fixed: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct RepairReport {
    pub(crate) package_id: String,
    pub(crate) deps: Vec<RepairDep>,
    /// Packages its files refer to that meta.json doesn't list.
    pub(crate) undeclared: Vec<RepairUndeclared>,
    pub(crate) names: Vec<RepairName>,
    /// References to other packages for files this package has.
    pub(crate) redirectable: u32,
    pub(crate) meta: Vec<MetaIssue>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct RepairRequest {
    pub(crate) file_path: String,
    /// meta.json keys to remove. Only ones nothing would still refer to after
    /// the repair are removed; the rest are reported as kept.
    #[serde(default)]
    pub(crate) remove_deps: Vec<String>,
    /// Remove every dependency nothing would still use (for many packages at
    /// once, where nobody picks keys).
    #[serde(default)]
    pub(crate) remove_unneeded: bool,
    /// Point references to files this package has at its own copy.
    #[serde(default)]
    pub(crate) redirect: bool,
    /// meta.json: the creator and package name from the file name, and the
    /// packages its files use but it doesn't list.
    #[serde(default)]
    pub(crate) fix_creator: bool,
    #[serde(default)]
    pub(crate) fix_package_name: bool,
    #[serde(default)]
    pub(crate) add_undeclared: bool,
    /// Rewrite meta.json as plain JSON (no BOM, no trailing commas) even when
    /// nothing else in it changes.
    #[serde(default)]
    pub(crate) clean_meta: bool,
    #[serde(default)]
    pub(crate) backup: bool,
    #[serde(default)]
    pub(crate) backup_dir: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct RepairResult {
    pub(crate) removed_deps: Vec<String>,
    pub(crate) kept_deps: Vec<String>,
    pub(crate) added_deps: Vec<String>,
    /// meta.json fields set: "creator", "package_name", "format".
    pub(crate) meta_fixed: Vec<String>,
    pub(crate) redirected: u32,
    pub(crate) renamed: u32,
    pub(crate) backup_path: Option<String>,
}

/// The lowercase family of a package id or dependency key. Also strips
/// `.minN` ("version N or newer"), which `naming::package_base` leaves on.
fn base_lc(id: &str) -> String {
    let base = crate::naming::package_base(id);
    // By bytes: the last segment can be Chinese, and slicing it by byte
    // offsets would cut a character.
    let is_min = |last: &str| {
        let b = last.as_bytes();
        b.len() > 3 && b[..3].eq_ignore_ascii_case(b"min") && b[3..].iter().all(u8::is_ascii_digit)
    };
    let base = match base.rsplit_once('.') {
        Some((head, last)) if is_min(last) => head,
        _ => base,
    };
    base.to_ascii_lowercase()
}

/// Files a reference can be in: the usual text payloads, plus plugin sources
/// and lists (a plugin can name another package's file in its code) and
/// legacy `.vac` scenes.
fn holds_refs(inner: &str) -> bool {
    if is_text_path(inner) {
        return true;
    }
    let lc = inner.to_ascii_lowercase();
    lc.ends_with(".cs") || lc.ends_with(".cslist") || lc.ends_with(".vac")
}

fn ref_kind(path: &str) -> &'static str {
    let lc = path.to_ascii_lowercase();
    if lc.ends_with(".cslist") || lc.ends_with(".cs") || lc.ends_with(".dll") {
        "plugin"
    } else if lc.ends_with(".vmi") || lc.ends_with(".vmb") || lc.ends_with(".dsf") {
        "morph"
    } else {
        "other"
    }
}

fn count_nested(v: &Value) -> u32 {
    v.get("dependencies")
        .and_then(Value::as_object)
        .map(|m| m.values().map(|c| 1 + count_nested(c)).sum())
        .unwrap_or(0)
}

/// The high half of code page 437, which is how VaM (and Windows Explorer's
/// zip view) reads a name stored without the UTF-8 flag.
const CP437_HIGH: &str = "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜ¢£¥₧ƒáíóúñÑªº¿⌐¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{a0}";

fn cp437(bytes: &[u8]) -> String {
    let high: Vec<char> = CP437_HIGH.chars().collect();
    bytes
        .iter()
        .map(|&b| if b < 0x80 { b as char } else { high.get((b - 0x80) as usize).copied().unwrap_or('?') })
        .collect()
}

/// The name a stored name was meant to have: the Info-ZIP Unicode extra field
/// when there is one (the zip crate already applied it), else the bytes as
/// UTF-8, else as GBK, else Shift-JIS. `None` when no reading is clean.
fn intended_name(raw: &[u8], parsed: &str) -> Option<String> {
    let as_437 = cp437(raw);
    if parsed != as_437 && !parsed.is_empty() {
        return Some(parsed.to_string());
    }
    if let Ok(s) = std::str::from_utf8(raw) {
        return Some(s.to_string());
    }
    for enc in [encoding_rs::GBK, encoding_rs::SHIFT_JIS] {
        let (text, _, bad) = enc.decode(raw);
        if !bad {
            return Some(text.into_owned());
        }
    }
    None
}

/// Every entry whose stored name isn't flagged UTF-8 but isn't plain ASCII:
/// index -> (as VaM reads it, the intended name). Reads the raw central
/// directory headers, since the zip crate hides the flag once it has applied
/// a Unicode extra field.
fn misnamed_entries<R: Read + Seek>(archive: &mut ZipArchive<R>, file: &mut fs::File) -> Result<BTreeMap<usize, (String, String)>> {
    let mut out = BTreeMap::new();
    for i in 0..archive.len() {
        let (start, parsed) = {
            let e = archive.by_index_raw(i)?;
            (e.central_header_start(), e.name().to_string())
        };
        let mut head = [0u8; 46];
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut head)?;
        if u32::from_le_bytes([head[0], head[1], head[2], head[3]]) != 0x0201_4b50 {
            continue;
        }
        let flags = u16::from_le_bytes([head[8], head[9]]);
        let name_len = u16::from_le_bytes([head[28], head[29]]) as usize;
        let mut raw = vec![0u8; name_len];
        file.read_exact(&mut raw)?;
        if flags & 0x0800 != 0 || raw.is_ascii() {
            continue;
        }
        if let Some(fixed) = intended_name(&raw, &parsed) {
            out.insert(i, (cp437(&raw), fixed));
        }
    }
    Ok(out)
}

/// Every `<family>.<version>:/<path>` in `text` for one family, as
/// (package as written, path). Searched by the exact name, because package
/// names can hold characters a general parser can't tell from the text
/// around them (`Nails!.3`, `Rich Wilder.Solara`, `【Flat_Shoes】…(001-576)`).
/// `text_lc` is `text` with ASCII lowercased, so byte offsets match.
fn refs_to_family(text: &str, text_lc: &str, family_lc: &str) -> Vec<(String, String)> {
    let bytes = text.as_bytes();
    let needle = format!("{family_lc}.");
    let mut out = Vec::new();
    for (at, _) in text_lc.match_indices(&needle) {
        // Not the tail of a longer name.
        if at > 0 {
            let prev = bytes[at - 1];
            if prev.is_ascii_alphanumeric() || matches!(prev, b'_' | b'-' | b'.') || prev >= 0x80 {
                continue;
            }
        }
        let ver_start = at + needle.len();
        let rest = &text_lc[ver_start..];
        let ver_len = if rest.starts_with("latest") {
            6
        } else {
            let digits_from = if rest.starts_with("min") { 3 } else { 0 };
            let n = rest[digits_from..].bytes().take_while(u8::is_ascii_digit).count();
            if n == 0 {
                continue;
            }
            digits_from + n
        };
        let after = ver_start + ver_len;
        if !text_lc[after..].starts_with(":/") {
            continue;
        }
        let path_start = after + 2;
        let path_end = text[path_start..]
            .find(['"', '\'', '\r', '\n'])
            .map_or(text.len(), |i| path_start + i);
        let path = text[path_start..path_end].trim_end();
        if !path.is_empty() {
            out.push((text[at..after].to_string(), path.to_string()));
        }
    }
    out
}

/// How often a family's name appears as a whole name (not inside a longer
/// one), with or without a version and path after it.
fn family_mentions(text_lc: &str, family_lc: &str) -> u32 {
    let bytes = text_lc.as_bytes();
    let is_name = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-') || b >= 0x80;
    text_lc
        .match_indices(family_lc)
        .filter(|(at, _)| {
            let before_ok = *at == 0 || !(is_name(bytes[at - 1]) || bytes[at - 1] == b'.');
            let end = at + family_lc.len();
            let after_ok = end >= bytes.len() || !is_name(bytes[end]);
            before_ok && after_ok
        })
        .count() as u32
}

struct Scan {
    report: RepairReport,
    /// Exact `Pkg:/path` -> `SELF:/path` for every reference to a file this
    /// package has.
    redirects: BTreeMap<String, String>,
    names: BTreeMap<usize, (String, String)>,
}

fn scan(path: &Path) -> Result<Scan> {
    let mut file = fs::File::open(path).with_context(|| format!("couldn't open {}", path.display()))?;
    let mut archive = ZipArchive::new(file.try_clone()?).context("not a readable package")?;
    let package_id = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
    let own = base_lc(&package_id);

    let names = misnamed_entries(&mut archive, &mut file)?;
    let mut own_paths: HashSet<String> = HashSet::new();
    for i in 0..archive.len() {
        let e = archive.by_index_raw(i)?;
        if !e.is_dir() {
            let name = names.get(&i).map(|(_, f)| f.clone()).unwrap_or_else(|| e.name().to_string());
            own_paths.insert(normalize_zip_path(&name).to_lowercase());
        }
    }

    #[derive(Default)]
    struct Group {
        display: String,
        refs: u32,
        inside: u32,
        plugins: u32,
        morphs: u32,
        others: u32,
        named: u32,
        samples: Vec<String>,
    }
    let mut groups: HashMap<String, Group> = HashMap::new();
    let mut redirects = BTreeMap::new();
    let mut meta: Option<Value> = None;
    let mut meta_raw: Option<Vec<u8>> = None;
    for i in 0..archive.len() {
        let mut e = archive.by_index(i)?;
        if !e.is_dir() && normalize_zip_path(e.name()).eq_ignore_ascii_case(META_PATH) {
            let mut raw = Vec::new();
            e.read_to_end(&mut raw)?;
            meta = read_json_bytes(&raw, "meta.json").ok();
            meta_raw = Some(raw);
            break;
        }
    }
    // The families meta.json lists, found by their exact names; anything else
    // the general parser finds is a package it uses but doesn't list.
    let listed: BTreeSet<String> = meta
        .as_ref()
        .and_then(|m| m.get("dependencies"))
        .and_then(Value::as_object)
        .map(|m| m.keys().map(|k| base_lc(k)).filter(|b| *b != own).collect())
        .unwrap_or_default();
    let mut tally = |groups: &mut HashMap<String, Group>, pkg: &str, ref_path: &str| {
        let g = groups.entry(base_lc(pkg)).or_default();
        if g.display.is_empty() {
            g.display = pkg.to_string();
        }
        g.refs += 1;
        if own_paths.contains(&normalize_zip_path(ref_path).to_lowercase()) {
            g.inside += 1;
            redirects.insert(format!("{pkg}:/{ref_path}"), format!("SELF:/{ref_path}"));
        } else {
            match ref_kind(ref_path) {
                "plugin" => g.plugins += 1,
                "morph" => g.morphs += 1,
                _ => g.others += 1,
            }
            if g.samples.len() < SAMPLE_CAP {
                g.samples.push(format!("{pkg}:/{ref_path}"));
            }
        }
    };
    for i in 0..archive.len() {
        let mut e = archive.by_index(i)?;
        if e.is_dir() {
            continue;
        }
        let inner = normalize_zip_path(e.name());
        if inner.eq_ignore_ascii_case(META_PATH) || !holds_refs(&inner) {
            continue;
        }
        let mut raw = Vec::new();
        e.read_to_end(&mut raw)?;
        let Some((text, _)) = decode_text(&raw) else { continue };
        let text_lc = text.to_ascii_lowercase();
        // path -> the listed packages found pointing at it, so the general
        // parser's cut-short reading of one (`Wilder.Solara` for `Rich
        // Wilder.Solara`) isn't taken for another package.
        let mut exact: HashMap<String, Vec<String>> = HashMap::new();
        for family in &listed {
            let found = refs_to_family(&text, &text_lc, family);
            let bare = family_mentions(&text_lc, family).saturating_sub(found.len() as u32);
            if bare > 0 {
                groups.entry(family.clone()).or_default().named += bare;
            }
            for (pkg, ref_path) in found {
                tally(&mut groups, &pkg, &ref_path);
                exact.entry(ref_path).or_default().push(pkg);
            }
        }
        for (pkg, ref_path) in collect_pkg_refs(&text) {
            let b = base_lc(&pkg);
            if pkg == "SELF" || b == own || listed.contains(&b) {
                continue;
            }
            if exact.get(&ref_path).is_some_and(|found| found.iter().any(|f| f.ends_with(pkg.as_str()))) {
                continue;
            }
            tally(&mut groups, &pkg, &ref_path);
        }
    }

    let declared: Vec<(String, Value)> = meta
        .as_ref()
        .and_then(|m| m.get("dependencies"))
        .and_then(Value::as_object)
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default();
    let declared_bases: BTreeSet<String> = declared.iter().map(|(k, _)| base_lc(k)).collect();
    let deps = declared
        .iter()
        .map(|(key, v)| {
            let b = base_lc(key);
            let g = groups.get(&b);
            let refs = g.map_or(0, |g| g.refs);
            let inside = g.map_or(0, |g| g.inside);
            let named = g.map_or(0, |g| g.named);
            let status = if b == own {
                "self"
            } else if named > 0 {
                "needed"
            } else if refs == 0 {
                "unused"
            } else if inside == refs {
                "covered"
            } else {
                "needed"
            };
            RepairDep {
                key: key.clone(),
                status: status.to_string(),
                refs,
                inside,
                plugins: g.map_or(0, |g| g.plugins),
                morphs: g.map_or(0, |g| g.morphs),
                others: g.map_or(0, |g| g.others),
                named,
                nested: count_nested(v),
                samples: g.map(|g| g.samples.clone()).unwrap_or_default(),
            }
        })
        .collect();
    let mut undeclared: Vec<RepairUndeclared> = groups
        .iter()
        .filter(|(b, _)| !declared_bases.contains(*b))
        .map(|(_, g)| RepairUndeclared { package: g.display.clone(), refs: g.refs, inside: g.inside })
        .collect();
    undeclared.sort_by_key(|u| u.package.to_lowercase());

    let meta_issues = meta_issues(&package_id, meta.as_ref(), meta_raw.as_deref(), &undeclared);
    let report = RepairReport {
        package_id,
        deps,
        undeclared,
        names: names.values().map(|(stored, fixed)| RepairName { stored: stored.clone(), fixed: fixed.clone() }).collect(),
        redirectable: redirects.len() as u32,
        meta: meta_issues,
    };
    Ok(Scan { report, redirects, names })
}

/// The creator and the name part of a package id: `Creator.Name.3`.
fn id_parts(package_id: &str) -> (String, String) {
    let base = crate::naming::package_base(package_id);
    match base.split_once('.') {
        Some((c, n)) => (c.to_string(), n.to_string()),
        None => (String::new(), base.to_string()),
    }
}

fn meta_str<'a>(meta: &'a Value, key: &str) -> &'a str {
    meta.get(key).and_then(Value::as_str).map(str::trim).unwrap_or("")
}

/// Undeclared packages worth listing: ones with a reference to a file this
/// package doesn't have. A reference to a file it has is pointed at its own
/// copy instead (Unused dependencies), not made a dependency.
fn undeclared_to_add(undeclared: &[RepairUndeclared]) -> impl Iterator<Item = &RepairUndeclared> {
    undeclared.iter().filter(|u| u.refs > u.inside)
}

fn meta_issues(package_id: &str, meta: Option<&Value>, raw: Option<&[u8]>, undeclared: &[RepairUndeclared]) -> Vec<MetaIssue> {
    let issue = |code: &str, fixable: bool, found: String, fixed: String| MetaIssue { code: code.to_string(), fixable, found, fixed };
    let Some(meta) = meta else {
        return vec![issue("missing", false, if raw.is_some() { "unreadable".into() } else { "none".into() }, String::new())];
    };
    let (creator, name) = id_parts(package_id);
    let mut out = Vec::new();
    let mc = meta_str(meta, "creatorName");
    if !creator.is_empty() && !mc.eq_ignore_ascii_case(&creator) {
        out.push(issue("creator", true, mc.to_string(), creator));
    }
    let mn = meta_str(meta, "packageName");
    if !name.is_empty() && mn != name {
        out.push(issue("package_name", true, mn.to_string(), name));
    }
    let add: Vec<&str> = undeclared_to_add(undeclared).map(|u| u.package.as_str()).collect();
    if !add.is_empty() {
        out.push(issue("undeclared", true, String::new(), add.join("\n")));
    }
    if let Some(raw) = raw {
        let bom = raw.starts_with(&[0xEF, 0xBB, 0xBF]);
        let strict = serde_json::from_slice::<Value>(if bom { &raw[3..] } else { raw }).is_ok();
        if bom || !strict {
            let what = match (bom, strict) {
                (true, false) => "a byte-order mark, and JSON only a lenient reader accepts",
                (true, true) => "a byte-order mark",
                _ => "JSON only a lenient reader accepts (trailing commas, comments)",
            };
            out.push(issue("format", true, what.to_string(), "plain JSON".into()));
        }
    }
    if meta_str(meta, "licenseType").is_empty() {
        out.push(issue("license", false, String::new(), String::new()));
    }
    out
}

/// What could be repaired in a package. Reads its text files once.
#[tauri::command(async)]
pub(crate) fn inspect_package_repair(file_path: String) -> Result<RepairReport, String> {
    scan(Path::new(&file_path)).map(|s| s.report).map_err(|e| e.to_string())
}

/// Rewrites the package with the chosen repairs (file names are always put
/// right when it's rewritten). Binary entries are copied without
/// recompressing; the new file replaces the old one only once it's complete.
/// `licenses`: lowercase family -> licenseType of the installed package, for
/// dependencies it adds.
pub(crate) fn repair_package(request: &RepairRequest, licenses: &HashMap<String, String>, progress: Progress<'_>) -> Result<RepairResult> {
    let target = Path::new(&request.file_path);
    progress(0.01, "Reading the package");
    let found = scan(target)?;
    let redirects = if request.redirect { found.redirects.clone() } else { BTreeMap::new() };
    let mut result = RepairResult::default();

    // Only what nothing would still use goes.
    let wanted: HashSet<&str> = request.remove_deps.iter().map(String::as_str).collect();
    let mut remove: BTreeSet<String> = BTreeSet::new();
    for d in &found.report.deps {
        let free = matches!(d.status.as_str(), "self" | "unused") || (d.status == "covered" && request.redirect);
        if free && (request.remove_unneeded || wanted.contains(d.key.as_str())) {
            remove.insert(d.key.clone());
        } else if wanted.contains(d.key.as_str()) {
            result.kept_deps.push(d.key.clone());
        }
    }
    let fixable = |code: &str| found.report.meta.iter().any(|m| m.code == code && m.fixable);
    let fix_creator = request.fix_creator && fixable("creator");
    let fix_name = request.fix_package_name && fixable("package_name");
    let add: Vec<String> = if request.add_undeclared {
        undeclared_to_add(&found.report.undeclared).map(|u| u.package.clone()).collect()
    } else {
        Vec::new()
    };
    let clean = request.clean_meta && fixable("format");
    let meta_changes = !remove.is_empty() || fix_creator || fix_name || !add.is_empty() || clean;
    if !meta_changes && redirects.is_empty() && found.names.is_empty() {
        return Err(anyhow!(NOTHING_TO_REPAIR));
    }
    let (file_creator, file_pkg_name) = id_parts(&found.report.package_id);

    let backup_root: Option<PathBuf> = if request.backup {
        request
            .backup_dir
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .or_else(|| target.parent().map(|p| p.join("fix-var-backup")))
    } else {
        None
    };
    if let Some(root) = &backup_root {
        fs::create_dir_all(root).with_context(|| format!("couldn't create the backup folder {}", root.display()))?;
        let name = target.file_name().ok_or_else(|| anyhow!("invalid file name"))?;
        let dest = free_backup_path(root, &name.to_string_lossy());
        copy_with_progress(target, &dest, &byte_step(progress, 0.05, 0.45, "Backing up the original"))
            .with_context(|| format!("couldn't back up to {}", dest.display()))?;
        result.backup_path = Some(dest.display().to_string());
    }

    let file_name = target.file_name().and_then(|n| n.to_str()).ok_or_else(|| anyhow!("invalid file name"))?;
    let staging = target.with_file_name(format!("{file_name}.repair-tmp"));
    let reader = fs::File::open(target)?;
    let total = reader.metadata().map(|m| m.len()).unwrap_or(0);
    let mut source = ZipArchive::new(reader)?;
    let written = byte_step(progress, if backup_root.is_some() { 0.45 } else { 0.05 }, 0.97, "Writing the repaired package");
    let mut done = 0u64;
    let outcome = (|| -> Result<()> {
        let mut out = ZipWriter::new(fs::File::create(&staging)?);
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .system(System::Unix);
        for i in 0..source.len() {
            written(done, total);
            let mut entry = source.by_index(i)?;
            done += entry.compressed_size();
            if entry.is_dir() {
                continue;
            }
            let name = found.names.get(&i).map(|(_, f)| f.clone()).unwrap_or_else(|| entry.name().to_string());
            if found.names.contains_key(&i) {
                result.renamed += 1;
            }
            let inner = normalize_zip_path(&name);
            if inner.eq_ignore_ascii_case(META_PATH) {
                let mut raw = Vec::new();
                entry.read_to_end(&mut raw)?;
                if !meta_changes {
                    // Untouched: the bytes it had, BOM and all.
                    out.start_file(&name, options)?;
                    out.write_all(&raw)?;
                    continue;
                }
                let mut meta = read_json_bytes(&raw, "meta.json")?;
                if let Some(obj) = meta.as_object_mut() {
                    if fix_creator {
                        obj.insert("creatorName".into(), Value::String(file_creator.clone()));
                        result.meta_fixed.push("creator".into());
                    }
                    if fix_name {
                        obj.insert("packageName".into(), Value::String(file_pkg_name.clone()));
                        result.meta_fixed.push("package_name".into());
                    }
                    if clean {
                        result.meta_fixed.push("format".into());
                    }
                    if !obj.get("dependencies").is_some_and(Value::is_object) {
                        obj.insert("dependencies".into(), Value::Object(serde_json::Map::new()));
                    }
                }
                if let Some(deps) = meta.get_mut("dependencies").and_then(Value::as_object_mut) {
                    for key in &remove {
                        if deps.remove(key).is_some() {
                            result.removed_deps.push(key.clone());
                        }
                    }
                    for pkg in &add {
                        if deps.contains_key(pkg) {
                            continue;
                        }
                        let license = licenses.get(&base_lc(pkg)).cloned().unwrap_or_else(|| "PC EA".to_string());
                        deps.insert(pkg.clone(), serde_json::json!({ "licenseType": license, "dependencies": {} }));
                        result.added_deps.push(pkg.clone());
                    }
                }
                out.start_file(&name, options)?;
                out.write_all(&dump_json_bytes(&meta)?)?;
            } else if !redirects.is_empty() && holds_refs(&inner) {
                let mut raw = Vec::new();
                entry.read_to_end(&mut raw)?;
                if let Some(updated) = rewrite_external_text_payload(&raw, &redirects) {
                    if let Some((before, _)) = decode_text(&raw) {
                        result.redirected += redirects.keys().map(|k| before.matches(k.as_str()).count() as u32).sum::<u32>();
                    }
                    raw = updated;
                }
                out.start_file(&name, options)?;
                out.write_all(&raw)?;
            } else if found.names.contains_key(&i) {
                out.raw_copy_file_rename(entry, &name)?;
            } else {
                out.raw_copy_file(entry)?;
            }
        }
        out.finish()?;
        Ok(())
    })();
    drop(source);
    if let Err(e) = outcome {
        let _ = fs::remove_file(&staging);
        return Err(e);
    }
    written(total, total);
    progress(0.99, "Putting the repaired package in place");
    fs::remove_file(target).with_context(|| format!("couldn't replace {}", target.display()))?;
    fs::rename(&staging, target).with_context(|| format!("couldn't move the repaired package to {}", target.display()))?;
    Ok(result)
}

/// One package's problems, for the library list.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct RepairSummary {
    pub(crate) file_path: String,
    pub(crate) package_id: String,
    pub(crate) size_bytes: u64,
    /// Dependencies nothing would use: self, unused, and covered ones (gone
    /// once references point at its own copies).
    pub(crate) unneeded: u32,
    pub(crate) covered: u32,
    /// Nested entries under those, i.e. what made it look short of packages.
    pub(crate) unneeded_nested: u32,
    pub(crate) redirectable: u32,
    pub(crate) names: u32,
    /// Fixable meta.json issue codes.
    pub(crate) meta: Vec<String>,
    pub(crate) error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct RepairScanResult {
    pub(crate) scanned: usize,
    pub(crate) cancelled: bool,
    /// Only packages with something to repair (or that couldn't be read).
    pub(crate) packages: Vec<RepairSummary>,
}

fn summarize(path: &Path, size: u64) -> Option<RepairSummary> {
    let package_id = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
    let base = RepairSummary { file_path: path.display().to_string(), package_id, size_bytes: size, ..RepairSummary::default() };
    let report = match scan(path) {
        Ok(found) => found.report,
        Err(e) => return Some(RepairSummary { error: Some(e.to_string()), ..base }),
    };
    let unneeded: Vec<&RepairDep> = report.deps.iter().filter(|d| matches!(d.status.as_str(), "self" | "unused" | "covered")).collect();
    let summary = RepairSummary {
        unneeded: unneeded.len() as u32,
        covered: unneeded.iter().filter(|d| d.status == "covered").count() as u32,
        unneeded_nested: unneeded.iter().map(|d| d.nested).sum(),
        redirectable: report.redirectable,
        names: report.names.len() as u32,
        meta: report.meta.iter().filter(|m| m.fixable).map(|m| m.code.clone()).collect(),
        ..base
    };
    let any = summary.unneeded + summary.redirectable + summary.names > 0 || !summary.meta.is_empty();
    any.then_some(summary)
}

/// The packages to check: the VAR Packages listing when there is one, else
/// the given folders.
fn library_files(state: &AppState, roots: &[String]) -> Result<Vec<(PathBuf, u64)>> {
    if let Ok(cache) = state.var_packages_folder_cache.lock() {
        if let Some(c) = cache.as_ref() {
            return Ok(c.items.iter().map(|it| (PathBuf::from(&it.file_path), it.size_bytes)).collect());
        }
    }
    let roots: Vec<PathBuf> = roots.iter().map(|r| r.trim()).filter(|r| !r.is_empty()).map(PathBuf::from).filter(|p| p.is_dir()).collect();
    let Some((first, rest)) = roots.split_first() else { return Ok(Vec::new()) };
    let entries = crate::utils::collect_var_files_multi(&crate::scan::scan_roots(first, rest))?;
    Ok(entries.into_iter().map(|e| (e.path, e.fingerprint.size)).collect())
}

fn licenses(items: &[VarPackageListItem]) -> HashMap<String, String> {
    items
        .iter()
        .filter_map(|it| it.license.as_ref().map(|l| (base_lc(&it.package_id), l.clone())))
        .collect()
}

fn set_result(tasks: &Arc<Mutex<HashMap<u64, ProgressPayload>>>, task_id: u64, apply: impl FnOnce(&mut ProgressPayload)) {
    if let Ok(mut guard) = tasks.lock() {
        if let Some(task) = guard.get_mut(&task_id) {
            task.done = true;
            task.progress = 1.0;
            apply(task);
        }
    }
}

fn register_task(state: &AppState, phase: &str, message: &str) -> Result<(u64, Arc<AtomicBool>), String> {
    let task_id = crate::db_location::next_task_id(state)?;
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
    Ok((task_id, cancel))
}

/// Checks every package in the library. Reads each one's text files and
/// central directory (not its textures), several at a time; stops early on
/// cancel_task, keeping what it found.
#[tauri::command]
pub(crate) fn start_repair_scan_task(roots: Vec<String>, state: State<'_, AppState>) -> Result<TaskHandle, String> {
    let files = library_files(&state, &roots).map_err(|e| e.to_string())?;
    let (task_id, cancel) = register_task(&state, "repair_scan_starting", "Listing packages")?;
    let tasks = Arc::clone(&state.tasks);
    thread::spawn(move || {
        let total = files.len();
        let done = AtomicUsize::new(0);
        let mut packages: Vec<RepairSummary> = files
            .par_iter()
            .filter_map(|(path, size)| {
                if cancel.load(Ordering::Relaxed) {
                    return None;
                }
                // A bug on one odd package must not stall the whole check: it
                // shows as that package's error instead.
                let found = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| summarize(path, *size))).unwrap_or_else(|_| {
                    Some(RepairSummary {
                        file_path: path.display().to_string(),
                        package_id: path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string(),
                        size_bytes: *size,
                        error: Some("couldn't be checked (an internal error; see the log)".into()),
                        ..RepairSummary::default()
                    })
                });
                let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                if n.is_multiple_of(8) || n == total {
                    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
                    set_task_progress(&tasks, task_id, "repair_scan", n as f64 / total.max(1) as f64, format!("Checked {n} of {total}: {name}"));
                }
                found
            })
            .collect();
        packages.sort_by_key(|p| p.package_id.to_lowercase());
        let result = RepairScanResult { scanned: done.load(Ordering::Relaxed), cancelled: cancel.load(Ordering::Relaxed), packages };
        set_result(&tasks, task_id, |t| {
            t.phase = "repair_scan_complete".into();
            t.message = format!("{} of {} packages have something to repair", result.packages.len(), result.scanned);
            t.repair_scan_result = Some(result);
        });
    });
    Ok(TaskHandle { id: task_id })
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct RepairDone {
    pub(crate) file_path: String,
    pub(crate) result: RepairResult,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct RepairFailed {
    pub(crate) file_path: String,
    pub(crate) error: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct RepairBatchResult {
    pub(crate) done: Vec<RepairDone>,
    pub(crate) failed: Vec<RepairFailed>,
    /// Had nothing the request would change.
    pub(crate) skipped: Vec<String>,
    pub(crate) cancelled: bool,
}

/// Repairs packages one after another. One that fails (locked, damaged) is
/// reported and the rest go on; cancel_task stops before the next one.
#[tauri::command]
pub(crate) fn start_repair_packages_task(requests: Vec<RepairRequest>, state: State<'_, AppState>) -> Result<TaskHandle, String> {
    let licenses = state
        .var_packages_folder_cache
        .lock()
        .ok()
        .and_then(|c| c.as_ref().map(|c| licenses(&c.items)))
        .unwrap_or_default();
    let (task_id, cancel) = register_task(&state, "repair_starting", "Starting the repair")?;
    let tasks = Arc::clone(&state.tasks);
    let folder_cache = Arc::clone(&state.var_packages_folder_cache);
    let generation = Arc::clone(&state.var_packages_cache_generation);
    thread::spawn(move || {
        let mut out = RepairBatchResult::default();
        let n = requests.len().max(1) as f64;
        for (i, request) in requests.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                out.cancelled = true;
                break;
            }
            let name = Path::new(&request.file_path).file_name().and_then(|s| s.to_str()).unwrap_or("").to_string();
            let step = |f: f64, m: &str| {
                let msg = if requests.len() > 1 { format!("{} of {}: {name}: {m}", i + 1, requests.len()) } else { m.to_string() };
                set_task_progress(&tasks, task_id, "repair_writing", (i as f64 + f) / n, msg);
            };
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| repair_package(request, &licenses, &step)))
                .unwrap_or_else(|_| Err(anyhow!("an internal error (see the log); the package was left as it was")));
            match outcome {
                Ok(result) => out.done.push(RepairDone { file_path: request.file_path.clone(), result }),
                Err(e) if e.to_string() == NOTHING_TO_REPAIR => out.skipped.push(request.file_path.clone()),
                Err(e) => out.failed.push(RepairFailed { file_path: request.file_path.clone(), error: e.to_string() }),
            }
        }
        // Their dependencies changed: the listing has to read them again.
        generation.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut cache) = folder_cache.lock() {
            *cache = None;
        }
        set_result(&tasks, task_id, |t| {
            t.phase = "repair_complete".into();
            t.message = format!("Repaired {}, {} failed", out.done.len(), out.failed.len());
            t.repair_batch_result = Some(out);
        });
    });
    Ok(TaskHandle { id: task_id })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use zip::write::SimpleFileOptions;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vvd_repair_{}_{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// meta.json plus files; `gbk_names` are stored with GBK bytes and no
    /// UTF-8 flag, the way old Chinese zip tools did: written under an ASCII
    /// placeholder of the same length, then the bytes are patched in (in the
    /// local and the central header alike).
    fn write_var(path: &Path, meta: Value, files: &[(&str, &str)], gbk_names: &[(&str, &str)]) {
        let mut zip = ZipWriter::new(fs::File::create(path).unwrap());
        let o = SimpleFileOptions::default();
        zip.start_file("meta.json", o).unwrap();
        zip.write_all(serde_json::to_string(&meta).unwrap().as_bytes()).unwrap();
        for (name, body) in files {
            zip.start_file(*name, o).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        let mut patches: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        for (n, (name, body)) in gbk_names.iter().enumerate() {
            let (bytes, _, _) = encoding_rs::GBK.encode(name);
            let mut holder = format!("gbk{n}_").into_bytes();
            holder.resize(bytes.len(), b'x');
            zip.start_file(String::from_utf8(holder.clone()).unwrap(), o).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
            patches.push((holder, bytes.into_owned()));
        }
        zip.finish().unwrap();
        let mut data = fs::read(path).unwrap();
        for (holder, bytes) in patches {
            let mut i = 0;
            while let Some(at) = data[i..].windows(holder.len()).position(|w| w == holder.as_slice()) {
                data[i + at..i + at + holder.len()].copy_from_slice(&bytes);
                i += at + holder.len();
            }
        }
        fs::write(path, data).unwrap();
    }

    #[test]
    fn classifies_dependencies_by_what_the_content_uses() {
        let dir = tmp("classify");
        let var = dir.join("me.Scene.1.var");
        write_var(
            &var,
            json!({"dependencies": {
                "me.Scene.latest": {},
                "a.Unused.latest": {"dependencies": {"x.Y.1": {}, "x.Z.1": {"dependencies": {"q.R.1": {}}}}},
                "b.Covered.latest": {},
                "c.Plugin.1": {},
            }}),
            &[
                ("Saves/scene/s.json", r#"{"a":"me.Scene.latest:/Custom/x.jpg","m":"b.Covered.latest:/Custom/m.vmi","p":"c.Plugin.1:/Custom/Scripts/p.cslist","u":"d.Undeclared.2:/Custom/u.jpg"}"#),
                ("Custom/m.vmi", "{}"),
            ],
            &[],
        );
        let r = scan(&var).unwrap().report;
        let st = |k: &str| r.deps.iter().find(|d| d.key == k).unwrap();
        assert_eq!(st("me.Scene.latest").status, "self");
        assert_eq!(st("a.Unused.latest").status, "unused");
        assert_eq!(st("a.Unused.latest").nested, 3);
        assert_eq!(st("b.Covered.latest").status, "covered");
        assert_eq!(st("c.Plugin.1").status, "needed");
        assert_eq!(st("c.Plugin.1").plugins, 1);
        assert_eq!(r.undeclared.len(), 1);
        assert_eq!(r.redirectable, 1);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn repair_drops_unused_points_at_own_copies_and_fixes_names() {
        let dir = tmp("repair");
        let var = dir.join("me.Scene.1.var");
        write_var(
            &var,
            json!({"dependencies": {"a.Unused.latest": {}, "b.Covered.latest": {}, "c.Plugin.1": {}}}),
            &[
                ("Saves/scene/s.json", r#"{"m":"b.Covered.latest:/Custom/m.vmi","p":"c.Plugin.1:/Custom/Scripts/p.cslist"}"#),
                ("Custom/m.vmi", "{}"),
            ],
            &[("Saves/scene/异界上仙风神.json", "{}")],
        );
        let found = scan(&var).unwrap();
        assert_eq!(found.report.names.len(), 1);
        assert_eq!(found.report.names[0].fixed, "Saves/scene/异界上仙风神.json");

        let res = repair_package(
            &RepairRequest {
                file_path: var.display().to_string(),
                remove_deps: vec!["a.Unused.latest".into(), "b.Covered.latest".into(), "c.Plugin.1".into()],
                remove_unneeded: false,
                redirect: true,
                fix_creator: false,
                fix_package_name: false,
                add_undeclared: false,
                clean_meta: false,
                backup: true,
                backup_dir: Some(dir.join("backup").display().to_string()),
            },
            &HashMap::new(),
            &crate::utils::no_progress,
        )
        .unwrap();
        assert_eq!(res.removed_deps, ["a.Unused.latest", "b.Covered.latest"]);
        assert_eq!(res.kept_deps, ["c.Plugin.1"], "still used by a plugin outside");
        assert_eq!(res.redirected, 1);
        assert_eq!(res.renamed, 1);
        assert!(Path::new(res.backup_path.as_deref().unwrap()).is_file());

        let again = scan(&var).unwrap();
        assert!(again.report.names.is_empty(), "names are UTF-8 now");
        let keys: Vec<&str> = again.report.deps.iter().map(|d| d.key.as_str()).collect();
        assert_eq!(keys, ["c.Plugin.1"]);
        let mut z = ZipArchive::new(fs::File::open(&var).unwrap()).unwrap();
        let mut s = String::new();
        z.by_name("Saves/scene/s.json").unwrap().read_to_string(&mut s).unwrap();
        assert!(s.contains("SELF:/Custom/m.vmi") && s.contains("c.Plugin.1:/"));
        assert!(z.by_name("Saves/scene/异界上仙风神.json").is_ok());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn listed_names_with_odd_characters_are_found() {
        let dir = tmp("odd");
        let var = dir.join("me.Scene.1.var");
        write_var(
            &var,
            json!({"dependencies": {
                "Jackaroo.Eyes_Lips_Nails!.3": {},
                "Rich Wilder.Solara.latest": {},
                "klphgz.【Flat_Shoes】Standing_Pose(001-576).latest": {},
                "aroo.Eyes_Lips_Nails!.3": {},
                "AcidBubbles.Glance.min14": {},
                "FallenDancer.JointCorrect.11": {},
                "CheesyFX.BodyLanguage_Resources.latest": {},
                "CheesyFX.BodyLanguage.latest": {},
            }}),
            &[(
                "Saves/scene/s.json",
                r#"{"a":"Jackaroo.Eyes_Lips_Nails!.3:/Custom/a.jpg","b":"Rich Wilder.Solara.latest:/Custom/b.vmi","c":"klphgz.【Flat_Shoes】Standing_Pose(001-576).latest:/Custom/c.json"}"#,
            ), (
                "Saves/scene/t.json",
                r#"{"p":"AcidBubbles.Glance.min14:/Custom/Scripts/AcidBubbles/Glance/Glance.cs"}"#,
            ), (
                "Custom/Scripts/x/Person.cs",
                r#"const string Jcm = "FallenDancer.JointCorrect.11:/Custom/Atom/Person/Morphs/female/FallenDancer/JCM";"#,
            ), (
                "Custom/Scripts/x/Fill.cs",
                r#"resourcesUid = GetLatestPackageUid("CheesyFX.BodyLanguage_Resources");"#,
            )],
            &[],
        );
        let r = scan(&var).unwrap().report;
        let st = |k: &str| r.deps.iter().find(|d| d.key == k).unwrap().status.clone();
        assert_eq!(st("AcidBubbles.Glance.min14"), "needed", ".minN keys are families too");
        assert_eq!(base_lc("vamxw.异界上仙风神.latest"), "vamxw.异界上仙风神");
        assert_eq!(base_lc("kk.小美甜.1"), "kk.小美甜");
        assert_eq!(base_lc("x.甜甜甜甜"), "x.甜甜甜甜");
        assert_eq!(st("FallenDancer.JointCorrect.11"), "needed", "a plugin's source names it");
        assert_eq!(st("CheesyFX.BodyLanguage_Resources.latest"), "needed", "loaded by name in code, no path");
        assert_eq!(st("CheesyFX.BodyLanguage.latest"), "unused", "a longer name isn't a mention");
        assert_eq!(st("Jackaroo.Eyes_Lips_Nails!.3"), "needed");
        assert_eq!(st("Rich Wilder.Solara.latest"), "needed");
        assert_eq!(st("klphgz.【Flat_Shoes】Standing_Pose(001-576).latest"), "needed");
        assert_eq!(st("aroo.Eyes_Lips_Nails!.3"), "unused", "a family's name inside a longer one isn't a reference to it");
        assert!(r.undeclared.is_empty(), "{:?}", r.undeclared.iter().map(|u| &u.package).collect::<Vec<_>>());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn meta_issues_are_found_and_fixed() {
        let dir = tmp("meta");
        let var = dir.join("vamxw.Fox.1.var");
        // A BOM, a trailing comma, another creator and name, an unlisted user.
        let mut raw = vec![0xEF, 0xBB, 0xBF];
        raw.extend_from_slice(br#"{"creatorName":"kesuen","packageName":"vamxw_Fox","licenseType":"CC BY","dependencies":{},}"#);
        let mut zip = ZipWriter::new(fs::File::create(&var).unwrap());
        let o = SimpleFileOptions::default();
        zip.start_file("meta.json", o).unwrap();
        zip.write_all(&raw).unwrap();
        zip.start_file("Saves/scene/s.json", o).unwrap();
        zip.write_all(br#"{"p":"MacGruber.PostMagic.3:/Custom/Scripts/p.cslist"}"#).unwrap();
        zip.finish().unwrap();

        let codes: Vec<String> = scan(&var).unwrap().report.meta.iter().map(|m| m.code.clone()).collect();
        assert_eq!(codes, ["creator", "package_name", "undeclared", "format"]);

        let licenses = HashMap::from([("macgruber.postmagic".to_string(), "CC BY-SA".to_string())]);
        let res = repair_package(
            &RepairRequest {
                file_path: var.display().to_string(),
                remove_deps: vec![],
                remove_unneeded: true,
                redirect: true,
                fix_creator: true,
                fix_package_name: true,
                add_undeclared: true,
                clean_meta: true,
                backup: false,
                backup_dir: None,
            },
            &licenses,
            &crate::utils::no_progress,
        )
        .unwrap();
        assert_eq!(res.added_deps, ["MacGruber.PostMagic.3"]);
        assert_eq!(res.meta_fixed, ["creator", "package_name", "format"]);
        let again = scan(&var).unwrap().report;
        assert!(again.meta.is_empty(), "{:?}", again.meta);
        let mut z = ZipArchive::new(fs::File::open(&var).unwrap()).unwrap();
        let mut m = String::new();
        z.by_name("meta.json").unwrap().read_to_string(&mut m).unwrap();
        let v: Value = serde_json::from_str(&m).expect("strict JSON now");
        assert_eq!(v["creatorName"], "vamxw");
        assert_eq!(v["packageName"], "Fox");
        assert_eq!(v["dependencies"]["MacGruber.PostMagic.3"]["licenseType"], "CC BY-SA");
        let _ = fs::remove_dir_all(dir);
    }
}
