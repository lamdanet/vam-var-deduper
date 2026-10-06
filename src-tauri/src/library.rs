//! VAR Packages library view: what is *inside* each scanned `.var`.
//!
//! The folder scan used to stop at the filesystem (name, size, mtime). The
//! library view also needs each package's content type, item count, license
//! and top-level dependencies, which means opening the archive. That is done
//! here, in parallel, and cached per file path under a size + mtime
//! fingerprint — in memory and in the `var_info_cache` table — so only new or
//! changed archives are ever re-read.
//!
//! On top of the per-archive info sits the dependency graph across one scan:
//! which dependencies resolve inside the scanned folders, how many packages use
//! each one, and which packages have a newer version beside them.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    io::Read,
    path::Path,
};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use tauri::State;
use zip::ZipArchive;

use crate::{
    db::Db,
    models::{AppState, VarFileEntry, VarPackageFilters, VarPackageListItem, META_PATH},
    naming,
    utils::{normalize_zip_path, read_json_bytes},
};

/// Bump whenever classification or the info shape changes: cached rows carry
/// the version they were computed with and are recomputed on mismatch.
const INFO_VERSION: u32 = 2;

/// Package type keys, in the order the filter panel lists them. Mirrors VaM
/// Backstage's library taxonomy (Scenes, Looks, Poses, Clothing, Hairstyles,
/// Other); the frontend owns labels and colors.
pub(crate) const TYPE_KEYS: &[&str] = &["scene", "look", "pose", "clothing", "hair", "other"];

/// Position of a type key in `TYPE_KEYS` (unknown keys sort last).
pub(crate) fn type_order(key: &str) -> usize {
    TYPE_KEYS.iter().position(|k| *k == key).unwrap_or(TYPE_KEYS.len())
}

/// Primary-type precedence: the first category a package has content in.
const CATEGORY_PRECEDENCE: &[&str] = &["scene", "look", "pose", "clothing", "hair"];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct VarPkgInfo {
    #[serde(default)]
    pub(crate) v: u32,
    pub(crate) readable: bool,
    pub(crate) pkg_type: String,
    /// Item counts per category (`scene`, `subscene`, `look`, `pose`,
    /// `clothing`, `hair`).
    pub(crate) type_counts: BTreeMap<String, u32>,
    pub(crate) item_count: u32,
    pub(crate) morph_count: u32,
    pub(crate) deps: Vec<String>,
    pub(crate) license: Option<String>,
    pub(crate) has_scene_image: bool,
    /// In-archive image used as the package thumbnail when there is no
    /// side-car image next to the `.var` (see `get_var_thumbnail`).
    pub(crate) thumb_entry: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct CachedVarInfo {
    pub(crate) size: u64,
    pub(crate) modified_ns: u128,
    pub(crate) info: VarPkgInfo,
}

// ----------------------------------------------------------------------------
// Classification — a port of VaM Backstage's `scanner/classifier.js`
// ----------------------------------------------------------------------------

struct Rule {
    fine: &'static str,
    prefixes: &'static [&'static str],
    exts: &'static [&'static str],
}

/// First match wins. Prefixes are lowercased; paths are compared lowercased.
const RULES: &[Rule] = &[
    Rule { fine: "scene", prefixes: &["saves/scene/"], exts: &["json"] },
    Rule { fine: "legacyScene", prefixes: &["saves/scene/"], exts: &["vac"] },
    Rule { fine: "subscene", prefixes: &["custom/subscene/", "custom/subscenes/"], exts: &["json"] },
    Rule { fine: "look", prefixes: &["custom/atom/person/appearance/"], exts: &["vap"] },
    Rule { fine: "legacyLook", prefixes: &["saves/person/appearance/"], exts: &["json"] },
    Rule { fine: "skinPreset", prefixes: &["custom/atom/person/skin/"], exts: &["vap"] },
    Rule { fine: "pose", prefixes: &["custom/atom/person/pose/"], exts: &["vap"] },
    Rule { fine: "legacyPose", prefixes: &["saves/person/pose/"], exts: &["json"] },
    Rule { fine: "clothingItem", prefixes: &["custom/clothing/"], exts: &["vab", "vaj", "vam"] },
    Rule { fine: "clothingPreset", prefixes: &["custom/atom/person/clothing/"], exts: &["vap"] },
    Rule { fine: "hairItem", prefixes: &["custom/hair/"], exts: &["vab", "vaj", "vam"] },
    Rule { fine: "hairPreset", prefixes: &["custom/atom/person/hair/"], exts: &["vap"] },
    Rule {
        fine: "pluginPreset",
        prefixes: &["custom/atom/person/plugin/", "custom/atom/person/plugins/"],
        exts: &["vap"],
    },
    Rule {
        fine: "morphBinary",
        prefixes: &["custom/atom/person/morph/", "custom/atom/person/morphs/"],
        exts: &["vmi", "vmb", "dsf"],
    },
    Rule { fine: "pluginScript", prefixes: &["custom/scripts/"], exts: &["cs"] },
    Rule { fine: "scriptList", prefixes: &["custom/scripts/"], exts: &["cslist"] },
    Rule {
        fine: "assetbundle",
        prefixes: &[
            "custom/asset/", "custom/assets/", "custom/sound/", "custom/sounds/", "custom/audio/",
        ],
        exts: &["assetbundle"],
    },
    Rule {
        fine: "audio",
        prefixes: &["custom/sound/", "custom/sounds/", "custom/audio/"],
        exts: &["wav", "mp3", "ogg", "aif", "aiff"],
    },
    Rule {
        fine: "texture",
        prefixes: &["custom/atom/person/texture/", "custom/atom/person/textures/"],
        exts: &["jpg", "jpeg", "png", "tif", "tiff"],
    },
];

/// `Custom/<X>/<file>.vap` exactly two levels deep is an atom preset, for any
/// X outside the folders the rules above own.
const ATOM_PRESET_EXCLUDED: &[&str] = &[
    "atom", "clothing", "hair", "assets", "asset", "scripts", "plugindata", "sounds", "sound",
    "audio", "subscene", "subscenes",
];

/// Extension preference when one item ships as several files (`.vam` +
/// `.vaj` + `.vab`, `.vmi` + `.vmb`): the lowest index wins.
fn prefer_rank(fine: &str, ext: &str) -> Option<usize> {
    let order: &[&str] = match fine {
        "clothingItem" | "hairItem" => &["vam", "vab", "vaj"],
        "morphBinary" => &["vmi", "dsf", "vmb"],
        _ => return None,
    };
    Some(order.iter().position(|e| *e == ext).unwrap_or(order.len()))
}

/// The UI category of a fine type, or `None` for hidden types.
fn category_of(fine: &str) -> Option<&'static str> {
    match fine {
        "scene" | "legacyScene" => Some("scene"),
        "subscene" => Some("subscene"),
        "look" | "legacyLook" | "skinPreset" => Some("look"),
        "pose" | "legacyPose" => Some("pose"),
        "clothingItem" | "clothingPreset" => Some("clothing"),
        "hairItem" | "hairPreset" => Some("hair"),
        _ => None,
    }
}

/// Thumbnail priority when picking a package image from its content.
fn type_rank(fine: &str) -> u8 {
    match fine {
        "scene" | "legacyScene" => 1,
        "subscene" => 2,
        "look" | "legacyLook" | "skinPreset" => 3,
        "pose" | "legacyPose" => 4,
        "clothingItem" | "clothingPreset" => 5,
        "hairItem" | "hairPreset" => 6,
        _ => 7,
    }
}

fn ext_of(lower: &str) -> &str {
    let file = lower.rsplit('/').next().unwrap_or(lower);
    match file.rsplit_once('.') {
        Some((_, ext)) => ext,
        None => "",
    }
}

/// Path minus its last extension (only within the final segment).
fn strip_ext(path: &str) -> &str {
    let slash = path.rfind('/').map(|i| i + 1).unwrap_or(0);
    match path[slash..].rfind('.') {
        Some(dot) => &path[..slash + dot],
        None => path,
    }
}

fn fine_type(lower_live: &str) -> Option<&'static str> {
    let ext = ext_of(lower_live);
    for rule in RULES {
        if rule.exts.contains(&ext) && rule.prefixes.iter().any(|p| lower_live.starts_with(p)) {
            return Some(rule.fine);
        }
    }
    if ext == "vap" {
        let parts: Vec<&str> = lower_live.split('/').collect();
        if parts.len() == 3 && parts[0] == "custom" && !ATOM_PRESET_EXCLUDED.contains(&parts[1]) {
            return Some("atomPreset");
        }
    }
    None
}

/// `Preset_Red_Dress.vap` -> `Red Dress`.
fn display_name(live: &str) -> String {
    let stem = strip_ext(live.rsplit('/').next().unwrap_or(live));
    let spaced = stem.replace('_', " ");
    match spaced.get(..7) {
        Some(head) if head.eq_ignore_ascii_case("preset ") => spaced[7..].to_string(),
        _ => spaced,
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ClassifiedItem {
    /// Normalized in-archive path (forward slashes, original case).
    pub(crate) path: String,
    pub(crate) fine: &'static str,
    pub(crate) category: Option<&'static str>,
    pub(crate) name: String,
    pub(crate) thumb: Option<String>,
}

/// `.disabled` suffix length; `Foo.vam.disabled` is still `Foo.vam` content.
const DISABLED_SUFFIX: &str = ".disabled";

/// Classifies an archive's file entries (normalized paths, no directories).
pub(crate) fn classify_entries(names: &[String]) -> Vec<ClassifiedItem> {
    let name_set: HashSet<&str> = names.iter().map(String::as_str).collect();
    let mut items: Vec<ClassifiedItem> = Vec::new();
    for name in names {
        let cut = name.len().saturating_sub(DISABLED_SUFFIX.len());
        let live = match name.get(cut..) {
            Some(tail) if cut > 0 && tail.eq_ignore_ascii_case(DISABLED_SUFFIX) => &name[..cut],
            _ => name.as_str(),
        };
        let Some(fine) = fine_type(&live.to_ascii_lowercase()) else {
            continue;
        };
        let stem = strip_ext(live);
        let thumb = ["jpg", "jpeg", "png"]
            .iter()
            .map(|ext| format!("{stem}.{ext}"))
            .find(|candidate| name_set.contains(candidate.as_str()));
        items.push(ClassifiedItem {
            path: name.clone(),
            fine,
            category: category_of(fine),
            name: display_name(live),
            thumb,
        });
    }

    // Pass 1: one item per (type, path-minus-extension) for multi-file items.
    let mut passthrough: Vec<ClassifiedItem> = Vec::new();
    let mut winners: Vec<(usize, ClassifiedItem)> = Vec::new();
    let mut winner_index: HashMap<String, usize> = HashMap::new();
    for item in items {
        let ext = ext_of(&item.path.to_ascii_lowercase()).to_string();
        let Some(rank) = prefer_rank(item.fine, &ext) else {
            passthrough.push(item);
            continue;
        };
        let key = format!("{}\0{}", item.fine, strip_ext(&item.path));
        match winner_index.get(&key) {
            Some(&i) if winners[i].0 <= rank => {}
            Some(&i) => winners[i] = (rank, item),
            None => {
                winner_index.insert(key, winners.len());
                winners.push((rank, item));
            }
        }
    }
    let mut items: Vec<ClassifiedItem> = passthrough;
    items.extend(winners.into_iter().map(|(_, item)| item));

    // Pass 2: an item and its preset with the same display name are one thing.
    // The one with a thumbnail wins (the preset on a tie), and a thumbless
    // winner inherits the loser's.
    let mut dropped: HashSet<String> = HashSet::new();
    let mut inherit: HashMap<String, String> = HashMap::new();
    for (item_fine, preset_fine) in [("clothingItem", "clothingPreset"), ("hairItem", "hairPreset")] {
        let mut by_name: HashMap<String, (Option<usize>, Option<usize>)> = HashMap::new();
        for (i, item) in items.iter().enumerate() {
            let slot = by_name.entry(item.name.to_lowercase()).or_default();
            if item.fine == item_fine {
                slot.0 = Some(i);
            } else if item.fine == preset_fine {
                slot.1 = Some(i);
            }
        }
        for (_, pair) in by_name {
            let (Some(a), Some(p)) = pair else {
                continue;
            };
            let (winner, loser) = if items[a].thumb.is_some() && items[p].thumb.is_none() {
                (a, p)
            } else {
                (p, a)
            };
            if items[winner].thumb.is_none() {
                if let Some(t) = items[loser].thumb.clone() {
                    inherit.insert(items[winner].path.clone(), t);
                }
            }
            dropped.insert(items[loser].path.clone());
        }
    }
    items.retain(|item| !dropped.contains(&item.path));
    for item in items.iter_mut() {
        if let Some(t) = inherit.remove(&item.path) {
            item.thumb = Some(t);
        }
    }
    items
}

/// Primary type from per-category item counts.
pub(crate) fn primary_type(counts: &BTreeMap<String, u32>) -> String {
    for key in CATEGORY_PRECEDENCE {
        if counts.get(*key).copied().unwrap_or(0) > 0 {
            return (*key).to_string();
        }
    }
    "other".to_string()
}

fn is_scene_image(lower: &str) -> bool {
    lower.starts_with("saves/scene/") && matches!(ext_of(lower), "jpg" | "jpeg" | "png")
}

struct Summary {
    type_counts: BTreeMap<String, u32>,
    item_count: u32,
    morph_count: u32,
    pkg_type: String,
    thumb_entry: Option<String>,
}

fn summarize(items: &[ClassifiedItem], first_scene_image: Option<&String>) -> Summary {
    let mut type_counts: BTreeMap<String, u32> = BTreeMap::new();
    let mut item_count = 0;
    let mut morph_count = 0;
    for item in items {
        if let Some(cat) = item.category {
            *type_counts.entry(cat.to_string()).or_insert(0) += 1;
            item_count += 1;
        }
        if item.fine == "morphBinary" {
            morph_count += 1;
        }
    }
    let thumb_entry = items
        .iter()
        .filter(|i| i.thumb.is_some())
        .min_by(|a, b| {
            type_rank(a.fine)
                .cmp(&type_rank(b.fine))
                .then_with(|| a.path.cmp(&b.path))
        })
        .and_then(|i| i.thumb.clone())
        .or_else(|| first_scene_image.cloned());
    Summary {
        pkg_type: primary_type(&type_counts),
        type_counts,
        item_count,
        morph_count,
        thumb_entry,
    }
}

// ----------------------------------------------------------------------------
// Reading one archive
// ----------------------------------------------------------------------------

fn read_meta(archive: &mut ZipArchive<fs::File>, index: usize) -> Option<serde_json::Value> {
    let mut entry = archive.by_index(index).ok()?;
    let mut buf = Vec::new();
    entry.read_to_end(&mut buf).ok()?;
    read_json_bytes(&buf, "meta.json").ok()
}

/// File entry names (normalized), the meta.json index, and the first scene
/// image — one pass over the central directory.
fn list_entries(archive: &mut ZipArchive<fs::File>) -> (Vec<String>, Option<usize>, Option<String>) {
    let mut names = Vec::with_capacity(archive.len());
    let mut meta_index = None;
    let mut scene_image = None;
    for i in 0..archive.len() {
        let Ok(entry) = archive.by_index_raw(i) else {
            continue;
        };
        if entry.is_dir() {
            continue;
        }
        let name = normalize_zip_path(entry.name());
        let lower = name.to_ascii_lowercase();
        if lower == META_PATH {
            meta_index = Some(i);
            continue;
        }
        if scene_image.is_none() && is_scene_image(&lower) {
            scene_image = Some(name.clone());
        }
        names.push(name);
    }
    (names, meta_index, scene_image)
}

/// Opens a `.var` once: classifies its central directory and reads
/// `meta.json`. Never fails — an unreadable archive yields `readable: false`
/// with an `other` type, so one bad file can't sink a scan.
pub(crate) fn read_var_pkg_info(path: &Path) -> VarPkgInfo {
    let mut info = VarPkgInfo {
        v: INFO_VERSION,
        pkg_type: "other".to_string(),
        ..VarPkgInfo::default()
    };
    let Ok(file) = fs::File::open(path) else {
        return info;
    };
    let Ok(mut archive) = ZipArchive::new(file) else {
        return info;
    };

    let (names, meta_index, scene_image) = list_entries(&mut archive);
    let items = classify_entries(&names);
    let summary = summarize(&items, scene_image.as_ref());
    info.has_scene_image = scene_image.is_some();
    info.type_counts = summary.type_counts;
    info.item_count = summary.item_count;
    info.morph_count = summary.morph_count;
    info.pkg_type = summary.pkg_type;
    info.thumb_entry = summary.thumb_entry;

    if let Some(index) = meta_index {
        if let Some(meta) = read_meta(&mut archive, index) {
            info.readable = true;
            info.license = meta_str(&meta, "licenseType");
            info.deps = meta_dependency_keys(&meta);
        }
    }
    info
}

fn meta_str(meta: &serde_json::Value, key: &str) -> Option<String> {
    meta.get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Top-level `dependencies` keys. VaM writes an object keyed by package id;
/// some hand-made packages use an array of ids instead.
fn meta_dependency_keys(meta: &serde_json::Value) -> Vec<String> {
    let mut out: Vec<String> = match meta.get("dependencies") {
        Some(serde_json::Value::Object(obj)) => obj.keys().map(|k| k.trim().to_string()).collect(),
        Some(serde_json::Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .collect(),
        _ => Vec::new(),
    };
    out.retain(|k| !k.is_empty());
    out
}

// ----------------------------------------------------------------------------
// Cache
// ----------------------------------------------------------------------------

fn load_cache_rows(db: &Db) -> HashMap<String, CachedVarInfo> {
    let mut out = HashMap::new();
    let Ok(conn) = db.read() else {
        return out;
    };
    let Ok(mut stmt) = conn.prepare("SELECT file_path, size, modified_ns, info FROM var_info_cache")
    else {
        return out;
    };
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    });
    if let Ok(rows) = rows {
        for (path, size, mtime, json) in rows.flatten() {
            let Ok(info) = serde_json::from_str::<VarPkgInfo>(&json) else {
                continue;
            };
            if info.v != INFO_VERSION {
                continue;
            }
            out.insert(
                path,
                CachedVarInfo {
                    size: size.max(0) as u64,
                    modified_ns: mtime.parse().unwrap_or(0),
                    info,
                },
            );
        }
    }
    out
}

/// Best effort: a writer busy with a long import must not stall the listing,
/// so a contended lock just skips persisting (the in-memory cache still holds
/// the rows, and the next scan tries again).
fn persist_cache_rows(db: &Db, rows: &[(String, CachedVarInfo)]) {
    if rows.is_empty() {
        return;
    }
    let Ok(mut conn) = db.conn.try_lock() else {
        return;
    };
    let Ok(tx) = conn.transaction() else {
        return;
    };
    {
        let Ok(mut stmt) = tx.prepare(
            "INSERT OR REPLACE INTO var_info_cache (file_path, size, modified_ns, info) \
             VALUES (?1, ?2, ?3, ?4)",
        ) else {
            return;
        };
        for (path, cached) in rows {
            let Ok(json) = serde_json::to_string(&cached.info) else {
                continue;
            };
            let _ = stmt.execute(rusqlite::params![
                path,
                cached.size as i64,
                cached.modified_ns.to_string(),
                json
            ]);
        }
    }
    let _ = tx.commit();
}

/// Library info for every scanned entry, in `entries` order. Cached rows whose
/// fingerprint still matches are reused; the rest are read in parallel.
pub(crate) fn infos_for_entries(
    entries: &[VarFileEntry],
    state: &AppState,
    db: &Db,
) -> Vec<VarPkgInfo> {
    // Snapshot what we can reuse, then release the lock for the slow part.
    let reusable: HashMap<String, VarPkgInfo> = {
        let mut guard = match state.var_info_cache.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        if guard.is_none() {
            *guard = Some(load_cache_rows(db));
        }
        let cache = guard.as_ref().expect("just populated");
        entries
            .iter()
            .filter_map(|e| {
                let key = e.path.display().to_string();
                cache.get(&key).and_then(|c| {
                    (c.size == e.fingerprint.size && c.modified_ns == e.fingerprint.modified_ns)
                        .then(|| (key, c.info.clone()))
                })
            })
            .collect()
    };

    let computed: Vec<(usize, VarPkgInfo)> = entries
        .par_iter()
        .enumerate()
        .filter(|(_, e)| !reusable.contains_key(&e.path.display().to_string()))
        .map(|(i, e)| (i, read_var_pkg_info(&e.path)))
        .collect();

    if !computed.is_empty() {
        let fresh: Vec<(String, CachedVarInfo)> = computed
            .iter()
            .map(|(i, info)| {
                let e = &entries[*i];
                (
                    e.path.display().to_string(),
                    CachedVarInfo {
                        size: e.fingerprint.size,
                        modified_ns: e.fingerprint.modified_ns,
                        info: info.clone(),
                    },
                )
            })
            .collect();
        if let Ok(mut guard) = state.var_info_cache.lock() {
            if let Some(cache) = guard.as_mut() {
                for (k, v) in &fresh {
                    cache.insert(k.clone(), v.clone());
                }
            }
        }
        persist_cache_rows(db, &fresh);
    }

    let mut by_index: HashMap<usize, VarPkgInfo> = computed.into_iter().collect();
    entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            by_index
                .remove(&i)
                .or_else(|| reusable.get(&e.path.display().to_string()).cloned())
                .unwrap_or_default()
        })
        .collect()
}

// ----------------------------------------------------------------------------
// Dependency graph across one scan
// ----------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VersionSpec {
    Latest,
    Exact(u64),
    Min(u64),
}

/// Splits a dependency key into its lowercased family and version spec:
/// `A.B.3`, `A.B.latest`, `A.B.min3`. A key with no recognizable version is
/// treated as `latest` of the whole key.
fn parse_dep(dep: &str) -> (String, VersionSpec) {
    let lc = dep.trim().to_ascii_lowercase();
    if let Some((base, last)) = lc.rsplit_once('.') {
        if last == "latest" {
            return (base.to_string(), VersionSpec::Latest);
        }
        if let Some(n) = last.strip_prefix("min") {
            if !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) {
                if let Ok(v) = n.parse() {
                    return (base.to_string(), VersionSpec::Min(v));
                }
            }
        }
        if !last.is_empty() && last.len() <= 19 && last.chars().all(|c| c.is_ascii_digit()) {
            if let Ok(v) = last.parse() {
                return (base.to_string(), VersionSpec::Exact(v));
            }
        }
    }
    (lc, VersionSpec::Latest)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DepResolution {
    /// The requested version (or a satisfying one) is present.
    Found(usize),
    /// Only a different version of the package is present.
    OtherVersion(usize),
    Missing,
}

/// Family index over a set of items: lowercased base -> (version, item index).
pub(crate) struct LibIndex {
    families: HashMap<String, Vec<(Option<u64>, usize)>>,
}

impl LibIndex {
    pub(crate) fn build(items: &[VarPackageListItem]) -> Self {
        let mut families: HashMap<String, Vec<(Option<u64>, usize)>> = HashMap::new();
        for (i, item) in items.iter().enumerate() {
            let base = naming::package_base(&item.package_id).to_ascii_lowercase();
            families
                .entry(base)
                .or_default()
                .push((naming::package_version(&item.package_id), i));
        }
        Self { families }
    }

    /// Highest numbered member; falls back to any member when none is numbered.
    fn newest(members: &[(Option<u64>, usize)]) -> Option<usize> {
        members
            .iter()
            .filter(|(v, _)| v.is_some())
            .max_by_key(|(v, _)| *v)
            .or_else(|| members.first())
            .map(|(_, i)| *i)
    }

    pub(crate) fn resolve(&self, dep: &str) -> DepResolution {
        let (base, spec) = parse_dep(dep);
        let Some(members) = self.families.get(&base) else {
            return DepResolution::Missing;
        };
        let Some(newest) = Self::newest(members) else {
            return DepResolution::Missing;
        };
        match spec {
            VersionSpec::Latest => DepResolution::Found(newest),
            VersionSpec::Exact(want) => members
                .iter()
                .find(|(v, _)| *v == Some(want))
                .map(|(_, i)| DepResolution::Found(*i))
                .unwrap_or(DepResolution::OtherVersion(newest)),
            VersionSpec::Min(min) => members
                .iter()
                .filter(|(v, _)| v.is_some_and(|v| v >= min))
                .max_by_key(|(v, _)| *v)
                .map(|(_, i)| DepResolution::Found(*i))
                .unwrap_or(DepResolution::OtherVersion(newest)),
        }
    }

    fn family_max(&self, package_id: &str) -> Option<u64> {
        let base = naming::package_base(package_id).to_ascii_lowercase();
        self.families
            .get(&base)?
            .iter()
            .filter_map(|(v, _)| *v)
            .max()
    }
}

/// Fills the graph-derived fields (`missing_dep_count`, `used_by_count`,
/// `newer_version`) across a whole scan. Returns how many distinct dependency
/// keys don't resolve exactly (the Missing status count — the same set
/// `list_missing_dependencies` lists).
pub(crate) fn apply_graph(items: &mut [VarPackageListItem]) -> u64 {
    let index = LibIndex::build(items);
    let mut used_by = vec![0u32; items.len()];
    let mut unresolved: HashSet<String> = HashSet::new();
    for (i, item) in items.iter_mut().enumerate() {
        let mut missing = 0;
        // A package listing the same family twice should count once as a user.
        let mut targets: HashSet<usize> = HashSet::new();
        for dep in &item.deps {
            match index.resolve(dep) {
                DepResolution::Found(t) => {
                    if t != i {
                        targets.insert(t);
                    }
                }
                DepResolution::OtherVersion(t) => {
                    unresolved.insert(dep.to_ascii_lowercase());
                    if t != i {
                        targets.insert(t);
                    }
                }
                DepResolution::Missing => {
                    unresolved.insert(dep.to_ascii_lowercase());
                    missing += 1;
                }
            }
        }
        item.missing_dep_count = missing;
        for t in targets {
            used_by[t] += 1;
        }
        let own = naming::package_version(&item.package_id);
        item.newer_version = match (own, index.family_max(&item.package_id)) {
            (Some(own), Some(max)) => max > own,
            _ => false,
        };
    }
    for (item, n) in items.iter_mut().zip(used_by) {
        item.used_by_count = n;
    }
    unresolved.len() as u64
}

pub(crate) fn status_matches(
    item: &VarPackageListItem,
    status: &str,
    favorite_ids: &HashSet<String>,
) -> bool {
    match status {
        "favorites" => favorite_ids.contains(&item.package_id),
        "indexed" => item.indexed,
        "unindexed" => !item.indexed,
        "dependency" => item.used_by_count > 0,
        "standalone" => item.used_by_count == 0,
        "broken" => item.missing_dep_count > 0,
        "outdated" => item.newer_version,
        _ => true,
    }
}

const STATUS_KEYS: &[&str] = &[
    "favorites", "dependency", "standalone", "broken", "outdated", "indexed", "unindexed",
];

pub(crate) fn type_matches(item: &VarPackageListItem, filters: &VarPackageFilters) -> bool {
    match filters.pkg_type.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(t) => item.pkg_type == t,
        None => true,
    }
}

pub(crate) fn enabled_matches(item: &VarPackageListItem, filters: &VarPackageFilters) -> bool {
    match filters.enabled.as_deref().map(str::trim) {
        Some("enabled") => !item.disabled,
        Some("disabled") => item.disabled,
        _ => true,
    }
}

pub(crate) fn library_status_matches(
    item: &VarPackageListItem,
    filters: &VarPackageFilters,
    favorite_ids: &HashSet<String>,
) -> bool {
    match filters.status.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => status_matches(item, s, favorite_ids),
        None => true,
    }
}

/// Counts for the filter panel. `base` is every item that passes the
/// non-facet filters (search, creator, size, favorites, scene image, enabled);
/// each facet then applies the *other* facet's selection.
pub(crate) fn compute_facets(
    all: &[VarPackageListItem],
    base: &[&VarPackageListItem],
    filters: &VarPackageFilters,
    favorite_ids: &HashSet<String>,
    missing_unique: u64,
) -> crate::models::VarPackageFacets {
    let mut facets = crate::models::VarPackageFacets::default();
    for key in TYPE_KEYS {
        facets.types.insert((*key).to_string(), 0);
    }
    for key in STATUS_KEYS {
        facets.statuses.insert((*key).to_string(), 0);
    }
    facets.statuses.insert("all".to_string(), 0);

    for item in base {
        let status_ok = library_status_matches(item, filters, favorite_ids);
        let type_ok = type_matches(item, filters);
        if status_ok {
            *facets.types.entry(item.pkg_type.clone()).or_insert(0) += 1;
        }
        if type_ok {
            *facets.statuses.entry("all".to_string()).or_insert(0) += 1;
            for key in STATUS_KEYS {
                if status_matches(item, key, favorite_ids) {
                    *facets.statuses.entry((*key).to_string()).or_insert(0) += 1;
                }
            }
        }
        if status_ok && type_ok {
            if item.disabled {
                facets.disabled += 1;
            } else {
                facets.enabled += 1;
            }
        }
    }
    facets.library_count = all.len() as u64;
    facets.statuses.insert("missing".to_string(), missing_unique);
    facets.library_bytes = all.iter().map(|i| i.size_bytes).sum();
    facets
}

// ----------------------------------------------------------------------------
// Search
// ----------------------------------------------------------------------------

/// Backstage-style search: whitespace-separated terms that must ALL match the
/// package id / file name / creator; `@term` matches the creator only;
/// `-term` excludes; `type:scene` matches the content type.
pub(crate) struct SearchQuery {
    include: Vec<String>,
    exclude: Vec<String>,
    creators: Vec<String>,
    types: Vec<String>,
}

impl SearchQuery {
    pub(crate) fn parse(raw: Option<&str>) -> Option<Self> {
        let raw = raw.map(str::trim).filter(|s| !s.is_empty())?;
        let mut q = SearchQuery {
            include: Vec::new(),
            exclude: Vec::new(),
            creators: Vec::new(),
            types: Vec::new(),
        };
        for term in raw.split_whitespace() {
            let term = term.to_lowercase();
            if let Some(rest) = term.strip_prefix('-') {
                if !rest.is_empty() {
                    q.exclude.push(rest.to_string());
                }
            } else if let Some(rest) = term.strip_prefix('@') {
                if !rest.is_empty() {
                    q.creators.push(rest.to_string());
                }
            } else if let Some(rest) = term.strip_prefix("type:") {
                if !rest.is_empty() {
                    q.types.push(rest.to_string());
                }
            } else {
                q.include.push(term);
            }
        }
        Some(q)
    }

    pub(crate) fn matches(&self, item: &VarPackageListItem) -> bool {
        let creator = item.creator.as_deref().unwrap_or("").to_lowercase();
        let hay = format!(
            "{}\n{}\n{}",
            item.package_id.to_lowercase(),
            item.file_name.to_lowercase(),
            creator
        );
        self.include.iter().all(|t| hay.contains(t.as_str()))
            && !self.exclude.iter().any(|t| hay.contains(t.as_str()))
            && self.creators.iter().all(|t| creator.contains(t.as_str()))
            && self.types.iter().all(|t| item.pkg_type.starts_with(t.as_str()))
    }
}

// ----------------------------------------------------------------------------
// Details panel
// ----------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub(crate) struct VarPackageDependency {
    /// The key as written in meta.json.
    pub(crate) id: String,
    /// `"found"` | `"other_version"` | `"indexed"` (only in the database) | `"missing"`.
    pub(crate) status: String,
    pub(crate) resolved_id: Option<String>,
    pub(crate) file_path: Option<String>,
    pub(crate) size_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct VarPackageContentItem {
    pub(crate) path: String,
    /// Fine type (`scene`, `legacyLook`, `clothingPreset`, ...).
    pub(crate) fine: String,
    /// UI category (`scene`, `subscene`, `look`, `pose`, `clothing`, `hair`).
    pub(crate) category: String,
    pub(crate) name: String,
    /// In-archive thumbnail image, when one sits beside the item.
    pub(crate) thumb: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct VarPackageUser {
    pub(crate) package_id: String,
    pub(crate) file_path: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct VarPackageDetails {
    pub(crate) readable: bool,
    pub(crate) creator_name: Option<String>,
    pub(crate) package_name: Option<String>,
    pub(crate) title: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) credits: Option<String>,
    pub(crate) instructions: Option<String>,
    pub(crate) license: Option<String>,
    pub(crate) promotional_link: Option<String>,
    pub(crate) program_version: Option<String>,
    pub(crate) pkg_type: String,
    pub(crate) type_counts: BTreeMap<String, u32>,
    pub(crate) item_count: u32,
    pub(crate) morph_count: u32,
    pub(crate) file_count: u32,
    pub(crate) image_count: u32,
    pub(crate) dependencies: Vec<VarPackageDependency>,
    /// Categorized content only (hidden types such as textures and scripts
    /// are counted in `file_count` but not listed).
    pub(crate) content: Vec<VarPackageContentItem>,
    /// True when `content` was capped at `CONTENT_CAP`.
    pub(crate) content_truncated: bool,
    pub(crate) used_by: Vec<VarPackageUser>,
    pub(crate) disabled: bool,
}

const CONTENT_CAP: usize = 2000;

/// Everything the details panel shows for one package: meta.json fields, the
/// classified content list, its dependencies resolved against the current
/// folder scan (then the database), and the scanned packages that use it.
#[tauri::command(async)]
pub(crate) fn get_var_package_details(
    file_path: String,
    db: State<'_, Db>,
    state: State<'_, AppState>,
) -> Result<VarPackageDetails, String> {
    let path = Path::new(&file_path);
    let file = fs::File::open(path).map_err(|e| format!("could not open: {e}"))?;
    let mut archive = ZipArchive::new(file).map_err(|e| format!("not a readable VAR: {e}"))?;

    let mut details = VarPackageDetails {
        disabled: crate::packages::disabled_sidecar(path).exists(),
        ..VarPackageDetails::default()
    };
    let (names, meta_index, scene_image) = list_entries(&mut archive);
    details.file_count = names.len() as u32 + u32::from(meta_index.is_some());
    details.image_count = names
        .iter()
        .filter(|n| matches!(ext_of(&n.to_ascii_lowercase()), "jpg" | "jpeg" | "png"))
        .count() as u32;
    let items = classify_entries(&names);
    let summary = summarize(&items, scene_image.as_ref());
    details.pkg_type = summary.pkg_type;
    details.type_counts = summary.type_counts;
    details.item_count = summary.item_count;
    details.morph_count = summary.morph_count;

    let mut content: Vec<VarPackageContentItem> = items
        .into_iter()
        .filter_map(|item| {
            item.category.map(|category| VarPackageContentItem {
                path: item.path,
                fine: item.fine.to_string(),
                category: category.to_string(),
                name: item.name,
                thumb: item.thumb,
            })
        })
        .collect();
    content.sort_by(|a, b| {
        a.category
            .cmp(&b.category)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    if content.len() > CONTENT_CAP {
        content.truncate(CONTENT_CAP);
        details.content_truncated = true;
    }
    details.content = content;

    let mut deps: Vec<String> = Vec::new();
    if let Some(index) = meta_index {
        if let Some(meta) = read_meta(&mut archive, index) {
            details.readable = true;
            details.creator_name = meta_str(&meta, "creatorName");
            details.package_name = meta_str(&meta, "packageName");
            details.title = meta_str(&meta, "title");
            details.description = meta_str(&meta, "description");
            details.credits = meta_str(&meta, "credits");
            details.instructions = meta_str(&meta, "instructions");
            details.license = meta_str(&meta, "licenseType");
            details.promotional_link = meta_str(&meta, "promotionalLink");
            details.program_version = meta_str(&meta, "programVersion");
            deps = meta_dependency_keys(&meta);
        }
    }
    drop(archive);

    // Resolve against the folder scan the page is showing.
    let mut unresolved: Vec<usize> = Vec::new();
    {
        let cache = state
            .var_packages_folder_cache
            .lock()
            .map_err(|_| "var packages folder cache poisoned".to_string())?;
        let items: &[VarPackageListItem] = cache.as_ref().map(|c| c.items.as_slice()).unwrap_or(&[]);
        let index = LibIndex::build(items);
        for dep in &deps {
            let (status, hit) = match index.resolve(dep) {
                DepResolution::Found(i) => ("found", Some(i)),
                DepResolution::OtherVersion(i) => ("other_version", Some(i)),
                DepResolution::Missing => ("missing", None),
            };
            let hit = hit.map(|i| &items[i]);
            if hit.is_none() {
                unresolved.push(details.dependencies.len());
            }
            details.dependencies.push(VarPackageDependency {
                id: dep.clone(),
                status: status.to_string(),
                resolved_id: hit.map(|h| h.package_id.clone()),
                file_path: hit.map(|h| h.file_path.clone()),
                size_bytes: hit.map(|h| h.size_bytes),
            });
        }

        // Users: scanned packages whose deps resolve to this file's family
        // member — matched by path so a same-id copy elsewhere isn't conflated.
        let own_index = items
            .iter()
            .position(|it| it.file_path.eq_ignore_ascii_case(&file_path));
        if let Some(own_index) = own_index {
            for (i, item) in items.iter().enumerate() {
                if i == own_index {
                    continue;
                }
                let uses = item.deps.iter().any(|d| {
                    matches!(
                        index.resolve(d),
                        DepResolution::Found(t) | DepResolution::OtherVersion(t) if t == own_index
                    )
                });
                if uses {
                    details.used_by.push(VarPackageUser {
                        package_id: item.package_id.clone(),
                        file_path: item.file_path.clone(),
                    });
                }
            }
        }
    }
    details
        .used_by
        .sort_by(|a, b| a.package_id.to_lowercase().cmp(&b.package_id.to_lowercase()));

    // Anything not in the folder may still be known to the database index.
    if !unresolved.is_empty() {
        if let Ok(conn) = db.read() {
            if let Ok(known) = crate::tasks::load_known_package_ids(&conn) {
                let known_lc: HashMap<String, String> = known
                    .into_iter()
                    .map(|id| (naming::package_base(&id).to_ascii_lowercase(), id))
                    .collect();
                for i in unresolved {
                    let dep = &mut details.dependencies[i];
                    let (base, _) = parse_dep(&dep.id);
                    if let Some(id) = known_lc.get(&base) {
                        dep.status = "indexed".to_string();
                        dep.resolved_id = Some(id.clone());
                    }
                }
            }
        }
    }
    Ok(details)
}

// ----------------------------------------------------------------------------
// Thumbnails
// ----------------------------------------------------------------------------

fn image_mime(name: &str) -> Option<&'static str> {
    match ext_of(&name.to_ascii_lowercase()) {
        "jpg" | "jpeg" => Some("image/jpeg"),
        "png" => Some("image/png"),
        _ => None,
    }
}

fn data_url(mime: &str, bytes: &[u8]) -> String {
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
    format!("data:{mime};base64,{}", BASE64.encode(bytes))
}

/// Reads one in-archive image by its *normalized* path, so entries stored
/// with backslashes still match.
fn read_archive_image(path: &Path, entry: &str) -> Option<String> {
    let mime = image_mime(entry)?;
    let file = fs::File::open(path).ok()?;
    let mut archive = ZipArchive::new(file).ok()?;
    let index = (0..archive.len()).find(|i| {
        archive
            .by_index_raw(*i)
            .map(|e| normalize_zip_path(e.name()) == entry)
            .unwrap_or(false)
    })?;
    let mut zip_file = archive.by_index(index).ok()?;
    let mut buf = Vec::new();
    zip_file.read_to_end(&mut buf).ok()?;
    Some(data_url(mime, &buf))
}

/// A package's card image, in VaM Backstage's priority order:
/// 1. a side-car `<stem>.jpg|jpeg|png` next to the `.var` (what Export Scene
///    Images writes),
/// 2. the best content thumbnail inside the archive (scenes first, then
///    looks, poses, clothing, hair),
/// 3. any `Saves/scene/` image.
///
/// With `entry` set, returns that in-archive image instead (content rows).
#[tauri::command(async)]
pub(crate) fn get_var_image(
    file_path: String,
    entry: Option<String>,
    state: State<'_, AppState>,
) -> Result<Option<String>, String> {
    let path = Path::new(&file_path);
    if let Some(entry) = entry.as_deref().map(str::trim).filter(|e| !e.is_empty()) {
        return Ok(read_archive_image(path, &normalize_zip_path(entry)));
    }

    for ext in ["jpg", "jpeg", "png"] {
        let sidecar = path.with_extension(ext);
        if sidecar.is_file() {
            if let (Ok(bytes), Some(mime)) = (fs::read(&sidecar), image_mime(ext)) {
                return Ok(Some(data_url(mime, &bytes)));
            }
        }
    }

    let cached = state
        .var_info_cache
        .lock()
        .ok()
        .and_then(|guard| guard.as_ref()?.get(&file_path).map(|c| c.info.thumb_entry.clone()));
    let thumb_entry = match cached {
        Some(entry) => entry,
        None => read_var_pkg_info(path).thumb_entry,
    };
    Ok(thumb_entry.and_then(|entry| read_archive_image(path, &entry)))
}

// ----------------------------------------------------------------------------
// Missing dependencies across the scan
// ----------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub(crate) struct MissingDependency {
    /// The dependency key as the first requester wrote it.
    pub(crate) id: String,
    /// `"missing"` (no version present) or `"other_version"`.
    pub(crate) status: String,
    /// The version that IS present, for `other_version`.
    pub(crate) have_id: Option<String>,
    /// Known to the database index (so it exists somewhere else).
    pub(crate) indexed: bool,
    pub(crate) needed_by: Vec<VarPackageUser>,
}

/// Every dependency the scanned packages reference that doesn't resolve
/// exactly inside the scan, grouped by key, most-needed first.
#[tauri::command(async)]
pub(crate) fn list_missing_dependencies(
    db: State<'_, Db>,
    state: State<'_, AppState>,
) -> Result<Vec<MissingDependency>, String> {
    let mut grouped: BTreeMap<String, MissingDependency> = BTreeMap::new();
    {
        let cache = state
            .var_packages_folder_cache
            .lock()
            .map_err(|_| "var packages folder cache poisoned".to_string())?;
        let items: &[VarPackageListItem] = cache.as_ref().map(|c| c.items.as_slice()).unwrap_or(&[]);
        let index = LibIndex::build(items);
        for item in items {
            for dep in &item.deps {
                let (status, have) = match index.resolve(dep) {
                    DepResolution::Found(_) => continue,
                    DepResolution::OtherVersion(i) => ("other_version", Some(items[i].package_id.clone())),
                    DepResolution::Missing => ("missing", None),
                };
                let entry = grouped
                    .entry(dep.to_ascii_lowercase())
                    .or_insert_with(|| MissingDependency {
                        id: dep.clone(),
                        status: status.to_string(),
                        have_id: have,
                        indexed: false,
                        needed_by: Vec::new(),
                    });
                entry.needed_by.push(VarPackageUser {
                    package_id: item.package_id.clone(),
                    file_path: item.file_path.clone(),
                });
            }
        }
    }
    if !grouped.is_empty() {
        if let Ok(conn) = db.read() {
            if let Ok(known) = crate::tasks::load_known_package_ids(&conn) {
                let bases: HashSet<String> = known
                    .iter()
                    .map(|id| naming::package_base(id).to_ascii_lowercase())
                    .collect();
                for dep in grouped.values_mut() {
                    let (base, _) = parse_dep(&dep.id);
                    dep.indexed = bases.contains(&base);
                }
            }
        }
    }
    let mut out: Vec<MissingDependency> = grouped.into_values().collect();
    out.sort_by(|a, b| {
        (a.status != "missing")
            .cmp(&(b.status != "missing"))
            .then_with(|| b.needed_by.len().cmp(&a.needed_by.len()))
            .then_with(|| a.id.to_lowercase().cmp(&b.id.to_lowercase()))
    });
    Ok(out)
}

/// Turns VaM's `.var.disabled` marker on or off for one package and patches
/// the folder cache so the listing reflects it without a rescan.
#[tauri::command]
pub(crate) fn set_var_package_disabled(
    file_path: String,
    disabled: bool,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let path = Path::new(&file_path);
    if !path.is_file() {
        return Err(format!("not a file: {file_path}"));
    }
    let is_var = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("var"));
    if !is_var {
        return Err("only .var packages can be disabled".to_string());
    }
    let marker = crate::packages::disabled_sidecar(path);
    if disabled {
        if !marker.exists() {
            fs::write(&marker, b"").map_err(|e| format!("could not create marker: {e}"))?;
        }
    } else if marker.exists() {
        fs::remove_file(&marker).map_err(|e| format!("could not remove marker: {e}"))?;
    }
    if let Ok(mut cache) = state.var_packages_folder_cache.lock() {
        if let Some(c) = cache.as_mut() {
            for item in c.items.iter_mut() {
                if item.file_path.eq_ignore_ascii_case(&file_path) {
                    item.disabled = disabled;
                }
            }
        }
    }
    Ok(disabled)
}

// ----------------------------------------------------------------------------
// VaM directory
// ----------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub(crate) struct VamDirInfo {
    /// The VaM directory to store (a picked `AddonPackages` folder resolves to
    /// its parent).
    pub(crate) vam_dir: String,
    pub(crate) addon_packages: String,
    /// `AddonPackages` exists under `vam_dir`.
    pub(crate) valid: bool,
    /// `.var` files anywhere under `AddonPackages` (0 when not valid).
    pub(crate) var_count: u64,
}

/// Checks a candidate VaM directory the way VaM Backstage's settings do: it
/// must contain an `AddonPackages` folder. Picking `AddonPackages` itself is
/// accepted and resolved to its parent.
#[tauri::command(async)]
pub(crate) fn inspect_vam_dir(path: String) -> VamDirInfo {
    let trimmed = path.trim().trim_end_matches(['\\', '/']);
    let mut dir = std::path::PathBuf::from(trimmed);
    let picked_addon = dir
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.eq_ignore_ascii_case("AddonPackages"));
    if picked_addon {
        if let Some(parent) = dir.parent() {
            dir = parent.to_path_buf();
        }
    }
    let addon = dir.join("AddonPackages");
    let valid = !trimmed.is_empty() && addon.is_dir();
    let var_count = if valid {
        walkdir::WalkDir::new(&addon)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| {
                e.file_type().is_file()
                    && e.path()
                        .extension()
                        .and_then(|x| x.to_str())
                        .is_some_and(|x| x.eq_ignore_ascii_case("var"))
            })
            .count() as u64
    } else {
        0
    };
    VamDirInfo {
        vam_dir: dir.display().to_string(),
        addon_packages: addon.display().to_string(),
        valid,
        var_count,
    }
}

// ----------------------------------------------------------------------------
// Hub card data for dependency rows
// ----------------------------------------------------------------------------

/// Per-family Hub lookups for this session. Only definite answers are kept (a
/// network failure is retried next time the row is shown).
static HUB_META_CACHE: std::sync::OnceLock<std::sync::Mutex<HashMap<String, crate::hub::HubPackageMeta>>> =
    std::sync::OnceLock::new();

/// Title, author, type, size, license and thumbnail from the VaM Hub for a
/// dependency that isn't on disk (Download Dependencies / Find Dependencies
/// Locally rows).
#[tauri::command]
pub(crate) async fn get_hub_package_meta(
    package_id: String,
) -> Result<crate::hub::HubPackageMeta, String> {
    let key = naming::package_base(package_id.trim()).to_ascii_lowercase();
    if key.is_empty() {
        return Ok(crate::hub::HubPackageMeta::default());
    }
    let cache = HUB_META_CACHE.get_or_init(Default::default);
    if let Some(hit) = cache.lock().ok().and_then(|c| c.get(&key).cloned()) {
        return Ok(hit);
    }
    // `reqwest::blocking` must never run on a tokio worker (see hub.rs), so the
    // lookup gets a plain OS thread, awaited from the blocking pool.
    let id = package_id.trim().to_string();
    let meta = tauri::async_runtime::spawn_blocking(move || {
        std::thread::spawn(move || match crate::hub::hub_client() {
            Ok(client) => crate::hub::fetch_package_meta(&client, &id),
            Err(e) => crate::hub::HubPackageMeta {
                error: Some(e.to_string()),
                ..Default::default()
            },
        })
        .join()
        .unwrap_or_else(|_| crate::hub::HubPackageMeta {
            error: Some("Hub lookup failed".to_string()),
            ..Default::default()
        })
    })
    .await
    .map_err(|e| e.to_string())?;
    if meta.error.is_none() {
        if let Ok(mut c) = cache.lock() {
            c.insert(key, meta.clone());
        }
    }
    Ok(meta)
}

// ----------------------------------------------------------------------------
// Dependency graph page
// ----------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub(crate) struct GraphNode {
    /// Stable id: the package file path, or `missing:<dep key>` for a
    /// dependency no scanned package satisfies.
    pub(crate) id: String,
    pub(crate) package_id: String,
    pub(crate) creator: Option<String>,
    pub(crate) pkg_type: String,
    pub(crate) size_bytes: u64,
    pub(crate) item_count: u32,
    pub(crate) dep_count: u32,
    pub(crate) missing_dep_count: u32,
    pub(crate) used_by_count: u32,
    pub(crate) disabled: bool,
    pub(crate) newer_version: bool,
    pub(crate) readable: bool,
    /// Placeholder for a dependency that isn't installed.
    pub(crate) missing: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct GraphEdge {
    /// Dependent (the package declaring the dependency).
    pub(crate) source: String,
    /// Dependency (a package node or a missing placeholder).
    pub(crate) target: String,
    /// The key as declared in meta.json.
    pub(crate) dep: String,
    /// `"found"` | `"other_version"` | `"missing"`.
    pub(crate) status: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct LibraryGraph {
    pub(crate) nodes: Vec<GraphNode>,
    pub(crate) edges: Vec<GraphEdge>,
    /// False when no folder scan is cached yet (the page should scan first).
    pub(crate) scanned: bool,
}

fn graph_node(item: &VarPackageListItem) -> GraphNode {
    GraphNode {
        id: item.file_path.clone(),
        package_id: item.package_id.clone(),
        creator: item.creator.clone(),
        pkg_type: item.pkg_type.clone(),
        size_bytes: item.size_bytes,
        item_count: item.item_count,
        dep_count: item.dep_count,
        missing_dep_count: item.missing_dep_count,
        used_by_count: item.used_by_count,
        disabled: item.disabled,
        newer_version: item.newer_version,
        readable: item.readable,
        missing: false,
    }
}

/// The whole dependency graph of the scanned AddonPackages (the folder cache
/// `list_var_packages` filled): every package, every declared dependency edge
/// resolved like the library does, and a placeholder node per dependency
/// nothing satisfies.
#[tauri::command(async)]
pub(crate) fn get_library_graph(state: State<'_, AppState>) -> Result<LibraryGraph, String> {
    let cache = state
        .var_packages_folder_cache
        .lock()
        .map_err(|_| "var packages folder cache poisoned".to_string())?;
    Ok(match cache.as_ref() {
        Some(cache) => build_library_graph(&cache.items),
        None => LibraryGraph::default(),
    })
}

pub(crate) fn build_library_graph(items: &[VarPackageListItem]) -> LibraryGraph {
    let index = LibIndex::build(items);
    let mut graph = LibraryGraph {
        nodes: items.iter().map(graph_node).collect(),
        edges: Vec::new(),
        scanned: true,
    };
    let mut missing_nodes: BTreeMap<String, GraphNode> = BTreeMap::new();
    for (i, item) in items.iter().enumerate() {
        let mut seen: HashSet<String> = HashSet::new();
        for dep in &item.deps {
            let (target, status) = match index.resolve(dep) {
                DepResolution::Found(t) => (items[t].file_path.clone(), "found"),
                DepResolution::OtherVersion(t) => (items[t].file_path.clone(), "other_version"),
                DepResolution::Missing => {
                    let key = dep.to_ascii_lowercase();
                    let id = format!("missing:{key}");
                    missing_nodes.entry(key).or_insert_with(|| GraphNode {
                        id: id.clone(),
                        package_id: dep.clone(),
                        creator: naming::creator_from_package_id(dep).map(str::to_string),
                        pkg_type: String::new(),
                        size_bytes: 0,
                        item_count: 0,
                        dep_count: 0,
                        missing_dep_count: 0,
                        used_by_count: 0,
                        disabled: false,
                        newer_version: false,
                        readable: false,
                        missing: true,
                    });
                    (id, "missing")
                }
            };
            // A package depending on its own family, or listing one target
            // twice (`Pkg.3` and `Pkg.latest`), adds no information.
            if target == items[i].file_path || !seen.insert(target.clone()) {
                continue;
            }
            graph.edges.push(GraphEdge {
                source: item.file_path.clone(),
                target,
                dep: dep.clone(),
                status: status.to_string(),
            });
        }
    }
    for edge in &graph.edges {
        if edge.status == "missing" {
            if let Some(node) = missing_nodes.get_mut(edge.target.trim_start_matches("missing:")) {
                node.used_by_count += 1;
            }
        }
    }
    graph.nodes.extend(missing_nodes.into_values());
    graph
}

// ----------------------------------------------------------------------------
// Hub page commands
// ----------------------------------------------------------------------------

/// Session cache of Hub API answers: body -> (fetched at, response).
static HUB_API_CACHE: std::sync::OnceLock<
    std::sync::Mutex<HashMap<String, (std::time::Instant, serde_json::Value)>>,
> = std::sync::OnceLock::new();

/// Runs blocking Hub I/O on a plain OS thread (never a tokio worker, see
/// hub.rs), awaited from the blocking pool.
async fn on_hub_thread<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(move || {
        std::thread::spawn(work)
            .join()
            .unwrap_or_else(|_| Err("Hub request failed".to_string()))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// One read-only Hub API call for the Hub page (getInfo, getResources,
/// getResourceDetail, findPackages). Answers are cached for a few minutes;
/// `refresh` skips the cache.
#[tauri::command]
pub(crate) async fn hub_api(
    action: String,
    params: Option<serde_json::Map<String, serde_json::Value>>,
    refresh: Option<bool>,
) -> Result<serde_json::Value, String> {
    if !crate::hub::BROWSE_ACTIONS.contains(&action.as_str()) {
        return Err(format!("unsupported Hub action: {action}"));
    }
    let params = params.unwrap_or_default();
    let key = crate::hub::vam_body(&action, &params);
    let ttl = std::time::Duration::from_secs(if action == "getResources" { 120 } else { 600 });
    let cache = HUB_API_CACHE.get_or_init(Default::default);
    if !refresh.unwrap_or(false) {
        if let Some((at, value)) = cache.lock().ok().and_then(|c| c.get(&key).cloned()) {
            if at.elapsed() < ttl {
                return Ok(value);
            }
        }
    }
    let value = on_hub_thread(move || {
        let client = crate::hub::hub_client().map_err(|e| e.to_string())?;
        crate::hub::api_request(&client, &action, &params).map_err(|e| e.to_string())
    })
    .await?;
    if let Ok(mut c) = cache.lock() {
        if c.len() > 400 {
            c.clear();
        }
        c.insert(key, (std::time::Instant::now(), value.clone()));
    }
    Ok(value)
}

static HUB_IMAGE_CACHE: std::sync::OnceLock<std::sync::Mutex<HashMap<String, Option<String>>>> =
    std::sync::OnceLock::new();

/// A Hub image as a data URL — the fallback when the webview can't load the
/// CDN URL directly. Only Hub / CDN hosts are fetched.
#[tauri::command]
pub(crate) async fn hub_image(url: String) -> Result<Option<String>, String> {
    let allowed = url.starts_with("https://hub.virtamate.com/")
        || url.starts_with("https://1424104733.rsc.cdn77.org/");
    if !allowed {
        return Ok(None);
    }
    let cache = HUB_IMAGE_CACHE.get_or_init(Default::default);
    if let Some(hit) = cache.lock().ok().and_then(|c| c.get(&url).cloned()) {
        return Ok(hit);
    }
    let fetch_url = url.clone();
    let data = on_hub_thread(move || {
        let client = crate::hub::hub_client().map_err(|e| e.to_string())?;
        Ok(crate::hub::image_data_url(&client, &fetch_url))
    })
    .await?;
    if let Ok(mut c) = cache.lock() {
        if c.len() > 600 {
            c.clear();
        }
        c.insert(url, data.clone());
    }
    Ok(data)
}

/// Lowercased file stems of every `.var` under `roots` (recursive) — what the
/// Hub page compares Hub files against to show "Installed".
#[tauri::command(async)]
pub(crate) fn list_local_package_ids(roots: Vec<String>) -> Vec<String> {
    let mut out: HashSet<String> = HashSet::new();
    for root in roots.iter().map(|r| r.trim()).filter(|r| !r.is_empty()) {
        for entry in walkdir::WalkDir::new(root).into_iter().filter_map(Result::ok) {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            let is_var = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("var"));
            if !is_var {
                continue;
            }
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                out.insert(stem.to_ascii_lowercase());
            }
        }
    }
    let mut ids: Vec<String> = out.into_iter().collect();
    ids.sort();
    ids
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct WishlistEntry {
    pub(crate) resource_id: String,
    pub(crate) snapshot: serde_json::Value,
    pub(crate) created_at: i64,
}

/// The Hub wishlist, newest first.
#[tauri::command(async)]
pub(crate) fn hub_wishlist_list(db: State<'_, Db>) -> Result<Vec<WishlistEntry>, String> {
    let conn = db.read().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare("SELECT resource_id, snapshot, created_at FROM hub_wishlist ORDER BY created_at DESC")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    Ok(rows
        .flatten()
        .map(|(resource_id, snapshot, created_at)| WishlistEntry {
            resource_id,
            snapshot: serde_json::from_str(&snapshot).unwrap_or(serde_json::Value::Null),
            created_at,
        })
        .collect())
}

/// Adds (with `snapshot`) or removes (`snapshot: null`) a wishlist entry.
/// Re-adding refreshes the snapshot but keeps the original date.
#[tauri::command]
pub(crate) fn hub_wishlist_set(
    resource_id: String,
    snapshot: Option<serde_json::Value>,
    db: State<'_, Db>,
) -> Result<(), String> {
    let rid = resource_id.trim();
    if rid.is_empty() {
        return Err("missing resource id".to_string());
    }
    let conn = db.conn.lock().map_err(|_| "database connection poisoned".to_string())?;
    match snapshot {
        Some(snap) => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            conn.execute(
                "INSERT INTO hub_wishlist (resource_id, snapshot, created_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(resource_id) DO UPDATE SET snapshot = excluded.snapshot",
                rusqlite::params![rid, snap.to_string(), now],
            )
            .map_err(|e| e.to_string())?;
        }
        None => {
            conn.execute("DELETE FROM hub_wishlist WHERE resource_id = ?1", rusqlite::params![rid])
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}
