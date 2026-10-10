//! Clean VARs — make a package smaller by pointing at copies elsewhere.
//!
//! The inverse of Internalize Resources: a file the package carries that
//! another package also has (same contents) can go, its references pointing
//! at that package instead, which becomes a dependency. This lists, for one
//! package, each file it uses itself that has an exact copy elsewhere — in
//! the folders, and, when asked, in packages only the database knows (not
//! installed: the package needs them downloaded) — and applies the choices
//! to the package itself, with a backup. The duplicate groups and the
//! rewrite come from the existing dedupe engine (`scan`, `execute`).

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs,
    io::Read,
    path::Path,
    sync::{atomic::Ordering, Arc, Mutex},
    thread,
};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use tauri::State;

use crate::{
    db::{self, Db},
    execute::{prepare_package_changes, rewrite_package, sum_removed_bytes},
    fix_var::{collect_pkg_refs, free_backup_path, is_text_path},
    models::{AppState, PreparedPackage, ProgressPayload, ScannedData, TaskHandle, KEEP_ALL_VALUE, META_PATH},
    naming,
    scan::{compute_missing_siblings, load_cached_scan_shared, load_cached_scan_with_target},
    tasks::{list_target_var_text_refs, new_progress_payload, parse_additional_dirs, set_task_progress},
    utils::{decode_text, normalize_zip_path},
};
use zip::ZipArchive;

/// A package that has an exact copy of a file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct CleanSource {
    pub(crate) package_id: String,
    /// The .var on disk; empty for a package only the database knows.
    pub(crate) file_path: String,
    pub(crate) internal_path: String,
    /// In the folders; false: only in the database (needs downloading).
    pub(crate) installed: bool,
    /// The package already lists it as a dependency: no new one.
    pub(crate) already_dependency: bool,
    /// What it lacks of the whole resource (a .vam's .vaj/.vab/textures, a
    /// .vmi's .vmb): missing, or not the same, where the references will
    /// point. Not empty: it can't be pointed at.
    #[serde(default)]
    pub(crate) incomplete: Vec<String>,
}

/// A file the package uses itself that has an exact copy elsewhere.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct CleanItem {
    /// The key the apply step takes: the duplicate group, or a database-only
    /// one (`dbfind|<package>|<path>`).
    pub(crate) key: String,
    pub(crate) path: String,
    /// With what goes along with it (a .vam's .vaj, .vab, textures).
    pub(crate) size: u64,
    pub(crate) bundle: Vec<String>,
    /// Installed packages first.
    pub(crate) copies: Vec<CleanSource>,
    /// Why it stays whatever copy exists (see `Keeps`): `script` or
    /// `picture:<the file it is the picture of>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) stays: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct CleanReport {
    pub(crate) target_package_id: String,
    pub(crate) items: Vec<CleanItem>,
    /// Files with a copy elsewhere that the package doesn't reference itself
    /// (offered as content, not used): left alone.
    pub(crate) unreferenced_files: u32,
    pub(crate) unreferenced_bytes: u64,
}

/// The same file, however its path is spelled (a package outside the
/// folders is kept under its canonical form, with a long-path prefix). The package to rewrite
/// must be exactly this one, never another copy with the same id.
fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy()),
    }
}

/// The scanned package that is this file: by its path as written first (one
/// pass over strings), else by the real file (a package outside the folders
/// is kept under its canonical form).
fn find_target<'a>(scanned: &'a ScannedData, target_path: &Path) -> Option<&'a PreparedPackage> {
    let plain = |p: &Path| {
        let s = p.to_string_lossy().replace('\\', "/");
        s.strip_prefix("//?/").unwrap_or(&s).to_lowercase()
    };
    let want = plain(target_path);
    scanned
        .packages
        .values()
        .find(|p| plain(&p.file_path) == want)
        .or_else(|| scanned.packages.values().find(|p| same_file(&p.file_path, target_path)))
}

/// Other versions of the package's own id its text uses (`Creator.Name.1:/…`
/// inside version 2) that aren't in the folders: VaM falls back to this
/// version for those, so they point at its files like `SELF:/`. An installed
/// version answers for itself and is left alone.
fn self_aliases(target_path: &Path, target_id: &str, scanned: &ScannedData) -> Result<BTreeSet<String>> {
    let family = naming::package_base(target_id).to_ascii_lowercase();
    let installed: HashSet<String> = scanned.packages.keys().map(|k| k.to_ascii_lowercase()).collect();
    let mut archive = ZipArchive::new(fs::File::open(target_path)?)?;
    let mut out = BTreeSet::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let name = normalize_zip_path(entry.name());
        if entry.is_dir() || !is_text_path(&name) {
            continue;
        }
        let mut raw = Vec::new();
        if entry.read_to_end(&mut raw).is_err() {
            continue;
        }
        let Some((text, _)) = decode_text(&raw) else {
            continue;
        };
        for (pkg, _) in collect_pkg_refs(&text) {
            let lower = pkg.to_ascii_lowercase();
            if naming::package_base(&pkg).eq_ignore_ascii_case(&family)
                && !lower.ends_with(".latest")
                && !pkg.eq_ignore_ascii_case(target_id)
                && !installed.contains(&lower)
            {
                out.insert(pkg);
            }
        }
    }
    Ok(out)
}

const IMAGE_EXTS: &[&str] = &["jpg", "jpeg", "png", "tif", "tiff", "tga"];

fn ext_of(path: &str) -> String {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default()
}

/// What in the package itself keeps a file where it is, whatever copy exists:
/// - a plugin script in it names the path (code builds it at runtime, e.g.
///   `GetPackagePath(this) + "Custom/…/LensDirt00.png"`): Clean can't change
///   code;
/// - an image that is the picture of a file beside it (`Preset_X.jpg` next to
///   `Preset_X.vap`): VaM finds it by name, there is no reference to point
///   elsewhere. A .vam/.vmi's picture goes with its item instead.
struct Keeps {
    scripts: String,
    /// Lowercase path without extension -> the non-image files with it.
    named: HashMap<String, Vec<String>>,
}

impl Keeps {
    fn read(target_path: &Path, target: &PreparedPackage) -> Result<Self> {
        let mut scripts = String::new();
        let mut archive = ZipArchive::new(fs::File::open(target_path)?)?;
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index)?;
            let name = normalize_zip_path(entry.name());
            if entry.is_dir() || !matches!(ext_of(&name).as_str(), "cs" | "cslist") {
                continue;
            }
            let mut raw = Vec::new();
            if entry.read_to_end(&mut raw).is_ok() {
                if let Some((text, _)) = decode_text(&raw) {
                    scripts.push_str(&text.to_lowercase());
                    scripts.push('\n');
                }
            }
        }
        let mut named: HashMap<String, Vec<String>> = HashMap::new();
        for r in &target.resource_refs {
            let ext = ext_of(&r.internal_path);
            if IMAGE_EXTS.contains(&ext.as_str()) || r.internal_path == META_PATH {
                continue;
            }
            let stem = r.internal_path[..r.internal_path.len() - ext.len()].trim_end_matches('.').to_lowercase();
            named.entry(stem).or_default().push(r.internal_path.clone());
        }
        Ok(Self { scripts, named })
    }

    /// Why `path` stays, if it does; `gone` are the paths leaving with it (a
    /// picture goes along when the file it shows goes too).
    fn reason(&self, path: &str, gone: &dyn Fn(&str) -> bool) -> Option<String> {
        if self.scripts.contains(&path.to_lowercase()) {
            return Some("script".to_string());
        }
        let ext = ext_of(path);
        if !IMAGE_EXTS.contains(&ext.as_str()) {
            return None;
        }
        let stem = path[..path.len() - ext.len()].trim_end_matches('.').to_lowercase();
        let files = self.named.get(&stem)?;
        // A .vam/.vmi's picture (beside its .vaj/.vab/.vmb too) belongs to
        // that item and moves with it.
        if files.iter().any(|f| matches!(ext_of(f).as_str(), "vam" | "vmi")) {
            return None;
        }
        files.iter().find(|f| !gone(f)).map(|f| format!("picture:{f}"))
    }
}

/// A scene is the package itself, not a resource to point elsewhere (the old
/// page's rule: `Saves/scene/` at any depth).
fn is_scene(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.starts_with("saves/scene/") || lower.contains("/saves/scene/")
}

/// What goes with `path` in the package (a .vam's .vaj/.vab/textures, a
/// .vmi's .vmb): the members it actually has.
fn bundle_in(package: &PreparedPackage, path: &str) -> Vec<String> {
    let inside: HashSet<&str> = package.resource_refs.iter().map(|r| r.internal_path.as_str()).collect();
    package
        .support_paths
        .get(path)
        .map(|s| s.iter().filter(|m| inside.contains(m.as_str())).cloned().collect())
        .unwrap_or_default()
}

fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map(|(d, _)| d).unwrap_or("")
}

/// `dir` joined with a relative path, `.` and `..` resolved.
fn join_rel(dir: &str, rel: &str) -> String {
    let mut parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
    for p in rel.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            _ => parts.push(p),
        }
    }
    parts.join("/")
}

/// The `SELF:/…` paths in a text, as written.
fn self_refs(text: &str) -> Vec<String> {
    text.match_indices("SELF:/")
        .map(|(i, _)| {
            let rest = &text[i + 6..];
            let end = rest.find(['"', '\'', '\r', '\n']).unwrap_or(rest.len());
            normalize_zip_path(rest[..end].trim())
        })
        .filter(|p| !p.is_empty())
        .collect()
}

/// What another package's copy must also have for the resource to work
/// from there: files at the same path (`same`: a preset's `SELF:/` files load
/// from the package it is in), or at the same place relative to the
/// resource's folder (`beside`: a plugin runs with the files beside it).
#[derive(Default)]
struct Needs {
    same: Vec<String>,
    beside: Vec<String>,
}

/// The package's own files its presets, sub-scenes and plugin lists name,
/// read once.
struct OwnRefs {
    /// .vap/.json -> the package's files it refers to (SELF:/ or its own id).
    refs: HashMap<String, Vec<String>>,
    /// .cslist -> the files it lists.
    lists: HashMap<String, Vec<String>>,
    files: BTreeSet<String>,
}

impl OwnRefs {
    fn read(target_path: &Path, target: &PreparedPackage) -> Result<Self> {
        let files: BTreeSet<String> = target.resource_refs.iter().map(|r| r.internal_path.clone()).collect();
        let family = naming::package_base(&target.package_id).to_ascii_lowercase();
        let (mut refs, mut lists) = (HashMap::new(), HashMap::new());
        let mut archive = ZipArchive::new(fs::File::open(target_path)?)?;
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index)?;
            let name = normalize_zip_path(entry.name());
            let ext = ext_of(&name);
            if entry.is_dir() || name == META_PATH || !matches!(ext.as_str(), "json" | "vap" | "cslist") {
                continue;
            }
            let mut raw = Vec::new();
            if entry.read_to_end(&mut raw).is_err() {
                continue;
            }
            let Some((text, _)) = decode_text(&raw) else {
                continue;
            };
            if ext == "cslist" {
                let dir = parent_dir(&name).to_string();
                let listed: Vec<String> = text
                    .lines()
                    .map(|l| normalize_zip_path(l.trim()))
                    .filter(|l| !l.is_empty())
                    .map(|l| join_rel(&dir, &l))
                    .filter(|p| files.contains(p))
                    .collect();
                lists.insert(name, listed);
                continue;
            }
            let mut found: Vec<String> = self_refs(&text);
            found.extend(
                collect_pkg_refs(&text)
                    .into_iter()
                    .filter(|(pkg, _)| naming::package_base(pkg).eq_ignore_ascii_case(&family))
                    .map(|(_, p)| normalize_zip_path(&p)),
            );
            found.retain(|p| files.contains(p) && *p != name);
            found.sort();
            found.dedup();
            if !found.is_empty() {
                refs.insert(name, found);
            }
        }
        Ok(Self { refs, lists, files })
    }

    fn needs(&self, path: &str) -> Needs {
        let mut needs = Needs::default();
        match ext_of(path).as_str() {
            // Presets inside presets too.
            "json" | "vap" => {
                let mut seen: BTreeSet<&str> = BTreeSet::from([path]);
                let mut todo = vec![path];
                while let Some(p) = todo.pop() {
                    for r in self.refs.get(p).into_iter().flatten() {
                        if seen.insert(r.as_str()) {
                            needs.same.push(r.clone());
                            if matches!(ext_of(r).as_str(), "json" | "vap") {
                                todo.push(r.as_str());
                            }
                        }
                    }
                }
            }
            // A plugin: what its list names, and its own folder (unless it
            // sits loose in Custom/Scripts beside unrelated plugins).
            "cs" | "cslist" | "dll" => {
                let dir = parent_dir(path);
                let mut beside: BTreeSet<String> = self.lists.get(path).cloned().unwrap_or_default().into_iter().collect();
                if !dir.is_empty() && !dir.eq_ignore_ascii_case("custom/scripts") {
                    let prefix = format!("{dir}/");
                    beside.extend(self.files.iter().filter(|f| f.starts_with(&prefix) && f.as_str() != path).cloned());
                }
                needs.beside = beside.into_iter().collect();
            }
            _ => {}
        }
        needs
    }
}

/// Where a plugin's file `member` (beside `item`) is in a copy of `item` at
/// `copy`: the same place relative to the folder.
fn beside_at(item: &str, copy: &str, member: &str) -> String {
    let (item_dir, copy_dir) = (parent_dir(item), parent_dir(copy));
    match member.strip_prefix(&format!("{item_dir}/")) {
        Some(rest) if copy_dir.is_empty() => rest.to_string(),
        Some(rest) => format!("{copy_dir}/{rest}"),
        None => member.to_string(),
    }
}

type FileMap<'a> = HashMap<&'a str, (Option<u32>, u64)>;

fn file_map(package: &PreparedPackage) -> FileMap<'_> {
    package
        .resource_refs
        .iter()
        .map(|r| (r.internal_path.as_str(), (r.crc32, r.size)))
        .collect()
}

/// Whether another package holds the whole resource, worked out once per
/// package.
struct Whole<'a> {
    scanned: &'a ScannedData,
    target_files: FileMap<'a>,
    missing: HashMap<String, BTreeMap<String, BTreeSet<String>>>,
    files: HashMap<String, FileMap<'a>>,
}

impl<'a> Whole<'a> {
    fn new(scanned: &'a ScannedData, target: &'a PreparedPackage) -> Self {
        Self {
            scanned,
            target_files: file_map(target),
            missing: HashMap::new(),
            files: HashMap::new(),
        }
    }

    /// What `src`'s copy at `src_path` lacks to stand in for the target's
    /// `item` with its `bundle` and what it `needs`: each the same, at the
    /// member's own path (where Clean points references to it, or a preset
    /// loads it) or beside it (a plugin), and the copy's own bundle whole
    /// (the old page's rule). Empty: the whole resource is there.
    fn lacks(&mut self, item: &str, bundle: &[String], needs: &Needs, src_id: &str, src_path: &str) -> Vec<String> {
        let Some(src) = self.scanned.packages.get(src_id) else {
            return bundle.iter().chain(&needs.same).chain(&needs.beside).cloned().collect();
        };
        let mut out: BTreeSet<String> = self
            .missing
            .entry(src_id.to_string())
            .or_insert_with(|| compute_missing_siblings(src))
            .get(src_path)
            .cloned()
            .unwrap_or_default();
        let target_files = &self.target_files;
        let files = self.files.entry(src_id.to_string()).or_insert_with(|| file_map(src));
        let same = bundle.iter().chain(&needs.same).map(|m| (m, m.clone()));
        let beside = needs.beside.iter().map(|m| (m, beside_at(item, src_path, m)));
        for (m, at) in same.chain(beside) {
            let want = target_files.get(m.as_str());
            if want.is_none() || files.get(at.as_str()) != want {
                out.insert(m.clone());
            }
        }
        out.into_iter().collect()
    }
}

/// What the target uses itself that has exact copies elsewhere.
pub(crate) fn clean_candidates(
    target_path: &Path,
    scanned: &ScannedData,
    db: Option<&Db>,
) -> Result<CleanReport> {
    let target_str = target_path.to_string_lossy().to_string();
    let target = find_target(scanned, target_path).ok_or_else(|| anyhow!("the package isn't in the scan: check it again"))?;
    let target_id = target.package_id.clone();

    // What it references itself; a bundle's members come with their parent.
    let used: HashSet<String> = list_target_var_text_refs(target_str.clone())
        .map_err(|e| anyhow!(e))?
        .into_iter()
        .collect();
    let members: HashSet<&String> = target.support_paths.values().flatten().collect();
    let deps: HashSet<String> = target
        .dependencies
        .as_object()
        .map(|m| m.keys().map(|k| naming::package_base(k).to_ascii_lowercase()).collect())
        .unwrap_or_default();
    let is_dep = |pkg: &str| deps.contains(&naming::package_base(pkg).to_ascii_lowercase());

    let mut report = CleanReport {
        target_package_id: target_id.clone(),
        ..CleanReport::default()
    };
    let mut by_path: HashMap<String, usize> = HashMap::new();
    let mut whole = Whole::new(scanned, target);
    let own_refs = OwnRefs::read(target_path, target)?;
    for group in &scanned.duplicate_groups {
        let Some(own) = group.refs.iter().find(|r| r.package_id == target_id) else {
            continue;
        };
        let path = own.internal_path.clone();
        if path == META_PATH || is_scene(&path) || members.contains(&path) {
            continue;
        }
        if !used.contains(&path) {
            report.unreferenced_files += 1;
            report.unreferenced_bytes += own.effective_size.max(own.size);
            continue;
        }
        let bundle = bundle_in(target, &path);
        let needs = own_refs.needs(&path);
        let mut seen: HashSet<&str> = HashSet::new();
        let mut copies: Vec<CleanSource> = Vec::new();
        for r in &group.refs {
            if r.package_id == target_id || !seen.insert(r.package_id.as_str()) {
                continue;
            }
            let lacks = whole.lacks(&path, &bundle, &needs, &r.package_id, &r.internal_path);
            copies.push(CleanSource {
                package_id: r.package_id.clone(),
                file_path: r.package_file.clone(),
                internal_path: r.internal_path.clone(),
                installed: true,
                already_dependency: is_dep(&r.package_id),
                incomplete: lacks,
            });
        }
        if copies.is_empty() {
            continue;
        }
        by_path.insert(path.clone(), report.items.len());
        report.items.push(CleanItem {
            key: group.key.clone(),
            size: own.effective_size.max(own.size),
            bundle,
            path,
            copies,
            stays: None,
        });
    }

    // Packages only the database knows, by the files' contents (the crc32
    // index): not installed, so choosing one means downloading it.
    if let Some(db) = db {
        let wanted: Vec<&crate::models::ResourceRef> = target
            .resource_refs
            .iter()
            .filter(|r| {
                r.crc32.is_some()
                    && r.size > 0
                    && r.internal_path != META_PATH
                    && !is_scene(&r.internal_path)
                    && !members.contains(&r.internal_path)
                    && used.contains(&r.internal_path)
            })
            .collect();
        let target_files = file_map(target);
        let crcs: Vec<u32> = wanted
            .iter()
            .flat_map(|r| {
                let needs = own_refs.needs(&r.internal_path);
                std::iter::once(r.crc32).chain(
                    bundle_in(target, &r.internal_path)
                        .iter()
                        .chain(&needs.same)
                        .chain(&needs.beside)
                        .map(|m| target_files.get(m.as_str()).and_then(|(crc, _)| *crc))
                        .collect::<Vec<_>>(),
                )
            })
            .flatten()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let exclude: HashSet<String> = std::iter::once(target_id.clone()).collect();
        let conn = db.read()?;
        let found = db::find_crc_matches_bulk(&conn, &crcs, &exclude, |_, _| {})?;
        drop(conn);
        for r in wanted {
            let Some(hits) = r.crc32.and_then(|crc| found.get(&crc)) else {
                continue;
            };
            let bundle = bundle_in(target, &r.internal_path);
            let needs = own_refs.needs(&r.internal_path);
            // A member is there when the database has the same file (crc,
            // size) in that package where it must be.
            let has = |pkg: &str, m: &String, at: &str| {
                target_files.get(m.as_str()).is_some_and(|(crc, size)| {
                    crc.and_then(|c| found.get(&c))
                        .is_some_and(|hs| hs.iter().any(|h| h.package_id == pkg && h.internal_path == at && h.size as u64 == *size))
                })
            };
            let lacks = |pkg: &str, copy: &str| -> Vec<String> {
                let same = bundle.iter().chain(&needs.same).filter(|m| !has(pkg, m, m));
                let beside = needs.beside.iter().filter(|m| !has(pkg, m, &beside_at(&r.internal_path, copy, m)));
                same.chain(beside).cloned().collect()
            };
            let mut seen: HashSet<&str> = HashSet::new();
            let extra: Vec<CleanSource> = hits
                .iter()
                .filter(|h| h.size as u64 == r.size && !scanned.packages.contains_key(&h.package_id))
                .filter(|h| seen.insert(h.package_id.as_str()))
                .map(|h| CleanSource {
                    package_id: h.package_id.clone(),
                    file_path: String::new(),
                    internal_path: h.internal_path.clone(),
                    installed: false,
                    already_dependency: is_dep(&h.package_id),
                    incomplete: lacks(&h.package_id, &h.internal_path),
                })
                .collect();
            if extra.is_empty() {
                continue;
            }
            match by_path.get(&r.internal_path) {
                Some(&i) => report.items[i].copies.extend(extra),
                None => {
                    by_path.insert(r.internal_path.clone(), report.items.len());
                    report.items.push(CleanItem {
                        key: format!("dbfind|{}|{}", target_id, r.internal_path),
                        path: r.internal_path.clone(),
                        size: r.effective_size.max(r.size),
                        bundle,
                        copies: extra,
                        stays: None,
                    });
                }
            }
        }
    }

    // What the package itself keeps in place: a file a plugin script in it
    // names, or the picture of a file beside it.
    let keeps = Keeps::read(target_path, target)?;
    for item in &mut report.items {
        item.stays = std::iter::once(&item.path)
            .chain(item.bundle.iter())
            .find_map(|p| keeps.reason(p, &|_| false));
    }

    report.items.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.path.cmp(&b.path)));
    Ok(report)
}

/// Lists what the package could point at elsewhere. Needs the folder scan
/// (`start_scan_task`, as Fix Missing runs it) cached; `include_db` adds
/// packages only the database knows.
#[tauri::command(async)]
pub(crate) fn clean_var_candidates(
    input_dir: String,
    additional_input_dirs: Option<Vec<String>>,
    target_var_path: String,
    include_db: bool,
    state: State<'_, AppState>,
    db: State<'_, Db>,
) -> Result<CleanReport, String> {
    let target_path = Path::new(&target_var_path);
    if !target_path.is_file() {
        return Err(format!("package not found on disk: {}", target_path.display()));
    }
    let additional = parse_additional_dirs(&additional_input_dirs.unwrap_or_default());
    // Only read: the cached scan itself, not a copy of it.
    let scanned = load_cached_scan_shared(&state.scan_cache, Path::new(&input_dir), &additional, Some(target_path))
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "No folder scan yet: check the package again.".to_string())?;
    clean_candidates(target_path, &scanned, include_db.then_some(&*db)).map_err(|e| e.to_string())
}

/// What Clean did to the package.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct CleanResult {
    pub(crate) removed_files: u32,
    pub(crate) removed_bytes: u64,
    pub(crate) dependencies: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) backup_path: Option<String>,
    /// The file on disk before and after (removed_bytes is what came out,
    /// uncompressed).
    pub(crate) size_before: u64,
    pub(crate) size_after: u64,
}

/// Applies `keep_map` (item key → `package:path` of the copy to point at)
/// to the package itself, backing it up first when asked.
pub(crate) fn apply_clean(
    target_path: &Path,
    mut scanned: ScannedData,
    keep_map: &BTreeMap<String, String>,
    backup_dir: Option<&Path>,
    db: Option<&Db>,
) -> Result<CleanResult> {
    let target_id = find_target(&scanned, target_path)
        .map(|p| p.package_id.clone())
        .ok_or_else(|| anyhow!("the package isn't in the scan: check it again"))?;
    // Never point at a copy without the whole resource (the page doesn't
    // offer one).
    {
        let target = scanned
            .packages
            .get(&target_id)
            .ok_or_else(|| anyhow!("the package isn't in the scan"))?;
        let own_path: HashMap<&str, &str> = scanned
            .duplicate_groups
            .iter()
            .filter_map(|g| g.refs.iter().find(|r| r.package_id == target_id).map(|r| (g.key.as_str(), r.internal_path.as_str())))
            .collect();
        let mut whole = Whole::new(&scanned, target);
        let own_refs = OwnRefs::read(target_path, target)?;
        for (key, value) in keep_map {
            let Some((pkg, path)) = value.split_once(':') else {
                continue;
            };
            if !scanned.packages.contains_key(pkg) {
                continue;
            }
            let own = own_path
                .get(key.as_str())
                .copied()
                .or_else(|| key.strip_prefix("dbfind|").and_then(|k| k.split_once('|')).map(|(_, p)| p));
            let Some(own) = own else {
                continue;
            };
            let lacks = whole.lacks(own, &bundle_in(target, own), &own_refs.needs(own), pkg, path);
            if !lacks.is_empty() {
                bail!("{pkg} doesn't have all of {own} ({}): choose another copy", lacks.join(", "));
            }
        }
    }
    // Only what was chosen changes: every other duplicate group stays as it
    // is (the engine would otherwise fall back to a group's first copy,
    // whichever package that is).
    let mut keep_map = keep_map.clone();
    for group in &scanned.duplicate_groups {
        keep_map.entry(group.key.clone()).or_insert_with(|| KEEP_ALL_VALUE.to_string());
    }
    prepare_package_changes(&mut scanned, &keep_map, Some(&target_id), db)?;
    let aliases = self_aliases(target_path, &target_id, &scanned)?;
    if let Some(p) = scanned.packages.get_mut(&target_id) {
        p.self_aliases = aliases;
    }
    let package = scanned
        .packages
        .get(&target_id)
        .ok_or_else(|| anyhow!("the package isn't in the scan"))?;
    if package.removed_paths.is_empty() && package.required_dependencies.is_empty() {
        bail!("nothing to change");
    }
    // Never remove what the package itself keeps in place (the page doesn't
    // offer it).
    let keeps = Keeps::read(target_path, package)?;
    let gone = |p: &str| package.removed_paths.contains(p);
    if let Some((path, why)) = package.removed_paths.iter().find_map(|p| keeps.reason(p, &gone).map(|w| (p, w))) {
        match why.strip_prefix("picture:") {
            Some(of) => bail!("{path} is the picture of {of}: it stays"),
            None => bail!("a plugin script in the package loads {path} by its path: it stays"),
        }
    }
    let mut result = CleanResult {
        removed_files: package.removed_paths.len() as u32,
        removed_bytes: sum_removed_bytes(package),
        dependencies: package.required_dependencies.iter().cloned().collect(),
        backup_path: None,
        size_before: fs::metadata(target_path).map(|m| m.len()).unwrap_or(0),
        size_after: 0,
    };
    if let Some(dir) = backup_dir {
        fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
        let name = target_path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| anyhow!("invalid file name"))?;
        let backup = free_backup_path(dir, name);
        fs::copy(target_path, &backup).with_context(|| format!("failed to back up to {}", backup.display()))?;
        result.backup_path = Some(backup.to_string_lossy().to_string());
    }
    // Written beside the package, then moved over it in one step: the package
    // is never missing, and nothing is left behind if either step fails.
    let file_name = package
        .file_path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow!("invalid file name"))?;
    let staging = package.file_path.with_file_name(format!("{file_name}.clean-tmp"));
    let written = rewrite_package(&scanned, package, &staging)
        .and_then(|()| fs::rename(&staging, &package.file_path).map_err(anyhow::Error::from));
    if let Err(err) = written {
        let _ = fs::remove_file(&staging);
        return Err(err).with_context(|| format!("couldn't write {} (is VaM using it?)", package.file_path.display()));
    }
    result.size_after = fs::metadata(target_path).map(|m| m.len()).unwrap_or(0);
    Ok(result)
}

/// Puts the backup Clean made back in place of the package. The backup must
/// be a copy of this package (its name, or its name with a ` (n)` Clean adds)
/// and a readable package; it's copied beside the package first, then moved
/// over it. Returns the size written.
pub(crate) fn restore_backup(backup: &Path, target: &Path) -> Result<u64> {
    let name = target.file_name().and_then(|n| n.to_str()).ok_or_else(|| anyhow!("invalid file name"))?;
    let stem = name.strip_suffix(".var").unwrap_or(name);
    let backup_name = backup.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    let same_name = backup_name.eq_ignore_ascii_case(name)
        || backup_name
            .strip_prefix(stem)
            .and_then(|rest| rest.strip_prefix(" ("))
            .and_then(|rest| rest.strip_suffix(").var"))
            .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
    if !same_name {
        bail!("{backup_name} isn't a backup of {name}");
    }
    let mut archive = ZipArchive::new(fs::File::open(backup).with_context(|| format!("can't open {}", backup.display()))?)
        .with_context(|| format!("{backup_name} isn't a readable package"))?;
    if archive.by_name(META_PATH).is_err() {
        bail!("{backup_name} has no meta.json: not a package");
    }
    drop(archive);
    let staging = target.with_file_name(format!("{name}.restore-tmp"));
    fs::copy(backup, &staging).with_context(|| format!("failed to copy {}", backup.display()))?;
    if let Err(err) = fs::rename(&staging, target) {
        let _ = fs::remove_file(&staging);
        return Err(err).with_context(|| format!("failed to replace {}", target.display()));
    }
    Ok(fs::metadata(target).map(|m| m.len()).unwrap_or(0))
}

/// Restore the original from Clean's backup (the report's button).
#[tauri::command(async)]
pub(crate) fn restore_clean_backup(backup_path: String, target_var_path: String) -> Result<u64, String> {
    restore_backup(Path::new(&backup_path), Path::new(&target_var_path)).map_err(|e| e.to_string())
}

/// Runs Clean on a worker thread; the UI polls `get_task_progress` and reads
/// `clean_result`.
#[allow(clippy::too_many_arguments)] // a Tauri command: one argument per field the UI sends
#[tauri::command]
pub(crate) fn start_clean_var_task(
    input_dir: String,
    additional_input_dirs: Option<Vec<String>>,
    target_var_path: String,
    keep_map: BTreeMap<String, String>,
    backup: bool,
    backup_dir: Option<String>,
    state: State<'_, AppState>,
    db: State<'_, Db>,
) -> Result<TaskHandle, String> {
    let task_id = state.next_task_id.fetch_add(1, Ordering::SeqCst) + 1;
    {
        let mut guard = state.tasks.lock().map_err(|_| "task state poisoned".to_string())?;
        guard.insert(task_id, new_progress_payload("clean_starting", "Cleaning the package"));
    }
    let tasks = Arc::clone(&state.tasks);
    let scan_cache = Arc::clone(&state.scan_cache);
    let db = db.inner().clone();
    let additional = parse_additional_dirs(&additional_input_dirs.unwrap_or_default());
    thread::spawn(move || {
        let result = (|| -> Result<CleanResult, String> {
            let target_path = Path::new(&target_var_path);
            if !target_path.is_file() {
                return Err(format!("package not found on disk: {}", target_path.display()));
            }
            let backup_dir = backup_dir.as_deref().map(str::trim).filter(|s| !s.is_empty());
            if backup && backup_dir.is_none() {
                return Err("Choose a folder for the backups first.".to_string());
            }
            set_task_progress(&tasks, task_id, "clean_loading", 0.1, "Loading the folder scan");
            let scanned = load_cached_scan_with_target(&scan_cache, Path::new(&input_dir), &additional, Some(target_path))
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "No folder scan yet: check the package again.".to_string())?;
            set_task_progress(&tasks, task_id, "clean_writing", 0.3, "Rewriting the package");
            apply_clean(
                target_path,
                scanned,
                &keep_map,
                if backup { backup_dir.map(Path::new) } else { None },
                Some(&db),
            )
            .map_err(|e| e.to_string())
        })();
        finish(&tasks, task_id, result);
    });
    Ok(TaskHandle { id: task_id })
}

fn finish(tasks: &Arc<Mutex<HashMap<u64, ProgressPayload>>>, task_id: u64, result: Result<CleanResult, String>) {
    if let Ok(mut guard) = tasks.lock() {
        if let Some(task) = guard.get_mut(&task_id) {
            task.done = true;
            task.progress = 1.0;
            match result {
                Ok(r) => {
                    task.phase = "clean_complete".to_string();
                    task.message = "Cleaned".to_string();
                    task.clean_result = Some(r);
                }
                Err(err) => {
                    task.phase = "clean_failed".to_string();
                    task.message = "Clean failed".to_string();
                    task.error = Some(err);
                }
            }
        }
    }
}
