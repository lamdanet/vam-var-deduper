//! Extract presets from scenes, after VaM Backstage's "Extract appearance /
//! outfit preset" (src/main/scenes/extract*.js), extended with hair and morph
//! presets.
//!
//! Each Person atom of a scene (`Saves/scene/*.json`) or legacy look
//! (`Saves/Person/Appearance/*.json`) inside a `.var` — or an appearance preset
//! (`Custom/Atom/Person/Appearance/*.vap`), which is a single person — can be
//! written out as VaM presets:
//!
//! - appearance → `Custom/Atom/Person/Appearance/extracted/`
//! - clothing   → `Custom/Atom/Person/Clothing/extracted/`
//! - hair       → `Custom/Atom/Person/Hair/extracted/`
//! - morphs     → `Custom/Atom/Person/Morphs/extracted/`
//!
//! named `Preset_<Creator> - <Package> - <scene>[_<atom>].vap`, with the
//! scene's thumbnail beside it. It is a JSON filter-and-copy: no clothing,
//! hair, morph or texture file is copied — the presets keep referencing the
//! package's files, with `SELF:/` rewritten to `<Creator>.<Package>.latest:/`
//! so they follow whichever version is installed.
//!
//! The storable filters were checked against VaM's own presets in a real
//! library (hair, morph, clothing and appearance `.vap`s) and differ from
//! Backstage where that showed it losing data: physics keys such as
//! `BreastControl.positionSpringZ` survive (only pose keys are stripped),
//! physics storables such as `BreastControl`/`GluteControl` are kept, and the
//! storables of built-in items (`"Heat Up Top"` → `HeatUpTopMaterial`) match
//! with the spaces removed.

use std::{
    collections::{BTreeMap, HashMap},
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use zip::ZipArchive;

use crate::{
    naming::{creator_from_package_id, package_base, package_version},
    utils::{decode_text, read_json_bytes},
};

/// What a preset holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum PresetKind {
    Appearance,
    Clothing,
    Hair,
    Morphs,
}

impl PresetKind {
    const ALL: [PresetKind; 4] = [Self::Appearance, Self::Clothing, Self::Hair, Self::Morphs];

    /// The folder under `Custom/Atom/Person/` VaM's preset browser reads.
    fn dir(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Clothing => "Clothing",
            Self::Hair => "Hair",
            Self::Morphs => "Morphs",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::Appearance => "appearance",
            Self::Clothing => "clothing",
            Self::Hair => "hair",
            Self::Morphs => "morphs",
        }
    }
}

/// Where the people come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum SourceType {
    Scene,
    Look,
    AppearancePreset,
}

pub(crate) fn source_type(internal_path: &str) -> Option<SourceType> {
    let lc = internal_path.replace('\\', "/").to_ascii_lowercase();
    if lc.starts_with("saves/scene/") && lc.ends_with(".json") {
        Some(SourceType::Scene)
    } else if lc.starts_with("saves/person/appearance/") && lc.ends_with(".json") {
        Some(SourceType::Look)
    } else if lc.starts_with("custom/atom/person/appearance/") && lc.ends_with(".vap") {
        Some(SourceType::AppearancePreset)
    } else {
        None
    }
}

// ---- Package naming ------------------------------------------------------------

/// The parts of a package file name the presets are named and pointed by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PackageNames {
    pub(crate) package_id: String,
    pub(crate) creator: String,
    /// The middle of `Creator.Name.3`.
    pub(crate) name: String,
    /// What `SELF:/` becomes: `Creator.Name.latest`, or the bare id when it
    /// carries no numeric version.
    pub(crate) self_ref: String,
}

pub(crate) fn package_names(var_path: &Path) -> PackageNames {
    let file = var_path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    let package_id = file
        .strip_suffix(".var")
        .or_else(|| file.strip_suffix(".VAR"))
        .unwrap_or(file)
        .to_string();
    let creator = creator_from_package_id(&package_id).unwrap_or("!local").to_string();
    let base = package_base(&package_id);
    let name = base
        .strip_prefix(&format!("{creator}."))
        .unwrap_or(base)
        .to_string();
    let self_ref = if package_version(&package_id).is_some() {
        format!("{base}.latest")
    } else {
        package_id.clone()
    };
    PackageNames {
        package_id,
        creator,
        name,
        self_ref,
    }
}

/// A file-name segment: `/` becomes `-`; characters Windows refuses, `#`
/// and control characters go; trailing dots and spaces (which Win32 drops
/// silently) are trimmed.
pub(crate) fn sanitize_segment(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| if c == '/' { '-' } else { c })
        .filter(|c| !matches!(c, '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '#'))
        .filter(|c| !c.is_control())
        .collect();
    cleaned
        .trim()
        .trim_end_matches(['.', ' '])
        .to_string()
}

/// `Preset_<Creator> - <Package> - <stem>[_<atom>]`. The package name keeps two
/// packages of one creator that both ship `scene.json` apart (Backstage drops
/// it and silently skips the second); it is left out when the scene is named
/// after the package. `atom_id` is given only when the source holds several
/// people.
pub(crate) fn preset_file_base(names: &PackageNames, internal_path: &str, atom_id: Option<&str>) -> String {
    let file = internal_path.rsplit(['/', '\\']).next().unwrap_or(internal_path);
    let stem = file.rsplit_once('.').map_or(file, |(s, _)| s);
    let stem = stem.strip_prefix("Preset_").unwrap_or(stem);
    let stem = sanitize_segment(stem);
    let package = sanitize_segment(&names.name);
    let mut name = if package.is_empty() || package.eq_ignore_ascii_case(&stem) {
        stem
    } else {
        format!("{package} - {stem}")
    };
    if let Some(atom) = atom_id {
        let atom = sanitize_segment(atom);
        if !atom.is_empty() {
            name.push('_');
            name.push_str(&atom);
        }
    }
    let creator = sanitize_segment(&names.creator);
    let creator = if creator.is_empty() { "!local".to_string() } else { creator };
    format!("Preset_{creator} - {name}")
}

pub(crate) fn preset_path(vam_dir: &Path, kind: PresetKind, file_base: &str) -> PathBuf {
    vam_dir
        .join("Custom")
        .join("Atom")
        .join("Person")
        .join(kind.dir())
        .join("extracted")
        .join(format!("{file_base}.vap"))
}

// ---- Storable filters ----------------------------------------------------------------

/// Pose keys: bones and control nodes carry only these. Everything else that
/// merely mentions position/rotation (`positionSpringZ`, `targetRotationX`,
/// `holdRotationSpring`, …) is physics and stays.
const POSE_KEYS: [&str; 10] = [
    "position",
    "rotation",
    "localPosition",
    "localRotation",
    "positionState",
    "rotationState",
    "rootPosition",
    "rootRotation",
    "relativeRootPosition",
    "relativeRootRotation",
];

/// Scene-only storables, never part of a VaM appearance preset: the preset
/// managers, plugins, triggers, pose animation, pose control nodes (camelCase
/// `headControl`, `lHandControl`, `control`, …; the PascalCase ones such as
/// `BreastControl` are physics and stay), lip sync and audio.
fn is_scene_only_storable(id: &str) -> bool {
    const EXACT: [&str; 9] = [
        "control",
        "Preset",
        "PluginManager",
        "AllJointsControl",
        "DebugJointsControl",
        "CharacterPoseSnapRestore",
        "AnimationControl",
        "LipSync",
        "HeadAudioSource",
    ];
    let lc = id.to_ascii_lowercase();
    EXACT.contains(&id)
        || id.ends_with("Presets")
        || lc.contains("trigger")
        || lc.contains("plugin")
        || lc.ends_with("animation")
        || (id.chars().next().is_some_and(|c| c.is_ascii_lowercase()) && id.ends_with("Control"))
}

/// Facial-expression and pose-driven morphs a scene leaves set mid-animation
/// (Backstage's list). Matched against the morph's name — its `name`, or the
/// file name of a package morph's `uid` — and as whole words, so a morph or
/// folder called "Painter" isn't taken for the "Pain" expression.
fn is_expression_morph(name: &str) -> bool {
    const CONTAINS: [&str; 8] = [
        "Breast Impact",
        "Tongue In-Out",
        "Shock",
        "Surprise",
        "Fear",
        "Pain",
        "Concentrate",
        "Eyes Closed",
    ];
    const STARTS: [&str; 5] = ["Left Fingers", "Right Fingers", "Mouth Open", "Smile", "Flirting"];
    if name == "OpenXXL" {
        return true;
    }
    // `Eyelids (Top|Bottom) (Down|Up) (Left|Right)`
    if let Some(rest) = name.strip_prefix("Eyelids ") {
        if let [edge, dir, side] = rest.split(' ').collect::<Vec<_>>()[..] {
            if matches!(edge, "Top" | "Bottom") && matches!(dir, "Down" | "Up") && matches!(side, "Left" | "Right") {
                return true;
            }
        }
    }
    // `Brow .*(Up|Down)`
    if let Some(i) = name.find("Brow ") {
        let rest = &name[i + "Brow ".len()..];
        if rest.contains("Up") || rest.contains("Down") {
            return true;
        }
    }
    CONTAINS.iter().any(|p| contains_word(name, p)) || STARTS.iter().any(|p| name.starts_with(p))
}

/// `needle` in `hay` as a whole word: not glued to a letter on either side.
fn contains_word(hay: &str, needle: &str) -> bool {
    hay.match_indices(needle).any(|(i, _)| {
        let before = hay[..i].chars().next_back();
        let after = hay[i + needle.len()..].chars().next();
        !before.is_some_and(char::is_alphabetic) && !after.is_some_and(char::is_alphabetic)
    })
}

/// A morph entry's display name: `name`, else the file stem of its `uid`.
fn morph_name(m: &Value) -> &str {
    if let Some(name) = m.get("name").and_then(Value::as_str).filter(|n| !n.is_empty()) {
        return name;
    }
    let uid = m.get("uid").and_then(Value::as_str).unwrap_or_default();
    let file = uid.rsplit(['/', '\\']).next().unwrap_or(uid);
    file.rsplit_once('.').map_or(file, |(stem, _)| stem)
}

fn storable_id(s: &Value) -> Option<&str> {
    s.get("id").and_then(Value::as_str)
}

fn geometry(storables: &[Value]) -> Option<&Map<String, Value>> {
    storables
        .iter()
        .filter(|s| storable_id(s) == Some("geometry"))
        .filter_map(Value::as_object)
        .find(|m| m.len() > 1)
}

/// The id prefixes of a geometry list's items (`clothing` / `hair`): the
/// `internalId` (`Creator:Item`) and the `id`, also with spaces removed —
/// built-in items name their storables that way.
fn item_keys(geo: Option<&Map<String, Value>>, list: &str) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    let items = geo.and_then(|g| g.get(list)).and_then(Value::as_array);
    for item in items.into_iter().flatten() {
        for field in ["internalId", "id"] {
            if let Some(key) = item.get(field).and_then(Value::as_str) {
                for k in [key.to_string(), key.replace(' ', "")] {
                    if !k.is_empty() && k != "geometry" && !keys.contains(&k) {
                        keys.push(k);
                    }
                }
            }
        }
    }
    keys
}

fn matches_any(id: &str, keys: &[String]) -> bool {
    keys.iter().any(|k| id.starts_with(k.as_str()))
}

/// Items of a geometry list that are switched on.
fn enabled_count(geo: Option<&Map<String, Value>>, list: &str) -> usize {
    geo.and_then(|g| g.get(list))
        .and_then(Value::as_array)
        .map_or(0, |items| {
            items
                .iter()
                .filter(|i| i.get("enabled").and_then(Value::as_str) != Some("false"))
                .count()
        })
}

fn morph_is_set(m: &Value) -> bool {
    match m.get("value") {
        Some(Value::String(s)) => s.parse::<f64>() != Ok(0.0),
        Some(Value::Number(n)) => n.as_f64().is_some_and(|v| v != 0.0),
        _ => false,
    }
}

fn kept_morphs(geo: Option<&Map<String, Value>>, keep_expressions: bool) -> Vec<Value> {
    geo.and_then(|g| g.get("morphs"))
        .and_then(Value::as_array)
        .map(|morphs| {
            morphs
                .iter()
                .filter(|m| morph_is_set(m))
                .filter(|m| keep_expressions || !is_expression_morph(morph_name(m)))
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// One person's appearance: every storable but the scene-only ones, minus
/// pose keys; storables left with nothing but their id go.
pub(crate) fn filter_appearance(storables: &[Value], keep_expressions: bool) -> Vec<Value> {
    let geo = geometry(storables);
    let mut items = item_keys(geo, "clothing");
    items.extend(item_keys(geo, "hair"));
    let mut out = Vec::new();
    for s in storables {
        let (Some(id), Some(obj)) = (storable_id(s), s.as_object()) else {
            continue;
        };
        if !matches_any(id, &items) && is_scene_only_storable(id) {
            continue;
        }
        let mut copy = obj.clone();
        for k in POSE_KEYS {
            copy.remove(k);
        }
        if id == "geometry" && copy.contains_key("morphs") {
            copy.insert("morphs".into(), Value::Array(kept_morphs(geo, keep_expressions)));
        }
        if copy.len() > 1 {
            out.push(Value::Object(copy));
        }
    }
    out
}

/// A clothing or hair preset: `geometry` with only that list, then every
/// storable belonging to one of its items, unchanged. For hair, built-in hair
/// storables that don't share their item's name (`Sim2HairStyle`) come along
/// by name.
fn filter_items(storables: &[Value], list: &str) -> Vec<Value> {
    let geo = geometry(storables);
    // Nothing switched on: a preset would only turn items off.
    if enabled_count(geo, list) == 0 {
        return Vec::new();
    }
    let Some(items) = geo.and_then(|g| g.get(list)).filter(|v| v.is_array()) else {
        return Vec::new();
    };
    let keys = item_keys(geo, list);
    let mut head = Map::new();
    head.insert("id".into(), Value::String("geometry".into()));
    head.insert(list.into(), items.clone());
    let mut out = vec![Value::Object(head)];
    for s in storables {
        let Some(id) = storable_id(s) else { continue };
        if id == "geometry" {
            continue;
        }
        let belongs = matches_any(id, &keys)
            || (list == "hair" && id.contains("Hair") && !is_scene_only_storable(id));
        if belongs {
            out.push(s.clone());
        }
    }
    out
}

pub(crate) fn filter_clothing(storables: &[Value]) -> Vec<Value> {
    filter_items(storables, "clothing")
}

pub(crate) fn filter_hair(storables: &[Value]) -> Vec<Value> {
    filter_items(storables, "hair")
}

/// A morph preset: `geometry` with the morphs that are set.
pub(crate) fn filter_morphs(storables: &[Value], keep_expressions: bool) -> Vec<Value> {
    let morphs = kept_morphs(geometry(storables), keep_expressions);
    if morphs.is_empty() {
        return Vec::new();
    }
    let mut head = Map::new();
    head.insert("id".into(), Value::String("geometry".into()));
    head.insert("morphs".into(), Value::Array(morphs));
    vec![Value::Object(head)]
}

/// The preset for one person, or `None` when it would hold nothing.
pub(crate) fn build_preset(storables: &[Value], kind: PresetKind, keep_expressions: bool) -> Option<Value> {
    let filtered = match kind {
        PresetKind::Appearance => filter_appearance(storables, keep_expressions),
        PresetKind::Clothing => filter_clothing(storables),
        PresetKind::Hair => filter_hair(storables),
        PresetKind::Morphs => filter_morphs(storables, keep_expressions),
    };
    if filtered.is_empty() {
        return None;
    }
    let mut preset = Map::new();
    preset.insert("setUnlistedParamsToDefault".into(), Value::String("true".into()));
    preset.insert("storables".into(), Value::Array(filtered));
    Some(Value::Object(preset))
}

/// VaM writes presets with three-space indentation.
fn preset_bytes(preset: &Value) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b"   ");
    let mut ser = serde_json::Serializer::with_formatter(&mut out, formatter);
    preset.serialize(&mut ser).map_err(|e| e.to_string())?;
    Ok(out)
}

// ---- Reading sources ----------------------------------------------------------------

/// A parsed source with `SELF:/` already pointed at the package.
struct Source {
    root: Value,
    thumb: Option<Vec<u8>>,
}

/// The people of a source: `(atom id, storables)`. An appearance preset is one
/// person with an empty id.
fn persons(source_type: SourceType, root: &Value) -> Vec<(String, &[Value])> {
    if source_type == SourceType::AppearancePreset {
        return root
            .get("storables")
            .and_then(Value::as_array)
            .map(|s| vec![(String::new(), s.as_slice())])
            .unwrap_or_default();
    }
    root.get("atoms")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|a| a.get("type").and_then(Value::as_str) == Some("Person"))
        .filter_map(|a| {
            let id = a.get("id").and_then(Value::as_str).unwrap_or("Person").to_string();
            let storables = a.get("storables").and_then(Value::as_array)?;
            Some((id, storables.as_slice()))
        })
        .collect()
}

/// Kinds a source type can give.
fn kinds_for(source_type: SourceType) -> &'static [PresetKind] {
    match source_type {
        // Already an appearance preset: only its parts make sense.
        SourceType::AppearancePreset => &[PresetKind::Clothing, PresetKind::Hair, PresetKind::Morphs],
        _ => &PresetKind::ALL,
    }
}

fn thumb_path(internal_path: &str) -> String {
    match internal_path.rsplit_once('.') {
        Some((stem, _)) => format!("{stem}.jpg"),
        None => format!("{internal_path}.jpg"),
    }
}

struct OpenVar {
    archive: ZipArchive<fs::File>,
    /// Lowercased entry name → (name, index). Entries are read by index: a
    /// name stored in a legacy code page doesn't always round-trip through
    /// `by_name`.
    names: HashMap<String, (String, usize)>,
}

fn open_var(path: &Path) -> Result<OpenVar, String> {
    let file = fs::File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let mut archive = ZipArchive::new(file).map_err(|e| format!("not a readable .var: {e}"))?;
    let mut names = HashMap::new();
    for i in 0..archive.len() {
        if let Ok(entry) = archive.by_index_raw(i) {
            let name = entry.name().to_string();
            names.insert(name.replace('\\', "/").to_ascii_lowercase(), (name, i));
        }
    }
    Ok(OpenVar { archive, names })
}

fn read_entry(var: &mut OpenVar, internal_path: &str) -> Option<Vec<u8>> {
    let &(_, index) = var.names.get(&internal_path.replace('\\', "/").to_ascii_lowercase())?;
    let mut entry = var.archive.by_index(index).ok()?;
    let mut buf = Vec::new();
    entry.read_to_end(&mut buf).ok()?;
    Some(buf)
}

fn read_source(var: &mut OpenVar, internal_path: &str, names: &PackageNames) -> Result<Source, String> {
    let bytes = read_entry(var, internal_path).ok_or("not found in the package")?;
    let (text, _) = decode_text(&bytes).ok_or("not a text file")?;
    let text = text.replace("SELF:/", &format!("{}:/", names.self_ref));
    let root = read_json_bytes(text.as_bytes(), internal_path).map_err(|e| e.to_string())?;
    let thumb = read_entry(var, &thumb_path(internal_path));
    Ok(Source { root, thumb })
}

// ---- Probe ----------------------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProbeTarget {
    pub(crate) path: String,
    pub(crate) exists: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProbeAtom {
    /// Empty for an appearance preset (a single person).
    pub(crate) atom_id: String,
    pub(crate) clothing: usize,
    pub(crate) hair: usize,
    pub(crate) morphs: usize,
    /// The presets this person can give, keyed by kind.
    pub(crate) targets: BTreeMap<String, ProbeTarget>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProbeSource {
    pub(crate) internal_path: String,
    pub(crate) source_type: SourceType,
    pub(crate) label: String,
    pub(crate) has_thumb: bool,
    pub(crate) atoms: Vec<ProbeAtom>,
    pub(crate) error: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProbePackage {
    pub(crate) file_path: String,
    pub(crate) package_id: String,
    pub(crate) creator: String,
    pub(crate) sources: Vec<ProbeSource>,
    pub(crate) error: Option<String>,
}

fn label_of(internal_path: &str) -> String {
    let file = internal_path.rsplit(['/', '\\']).next().unwrap_or(internal_path);
    file.rsplit_once('.').map_or(file, |(s, _)| s).to_string()
}

fn probe_atom(
    vam_dir: &Path,
    names: &PackageNames,
    internal_path: &str,
    source_type: SourceType,
    atom_id: &str,
    multi: bool,
    storables: &[Value],
) -> ProbeAtom {
    let geo = geometry(storables);
    let clothing = enabled_count(geo, "clothing");
    let hair = enabled_count(geo, "hair");
    let morphs = kept_morphs(geo, true).len();
    let base = preset_file_base(names, internal_path, multi.then_some(atom_id));
    let targets = kinds_for(source_type)
        .iter()
        .filter(|kind| match kind {
            PresetKind::Appearance => true,
            PresetKind::Clothing => clothing > 0,
            PresetKind::Hair => hair > 0,
            PresetKind::Morphs => morphs > 0,
        })
        .map(|&kind| {
            let path = preset_path(vam_dir, kind, &base);
            let exists = path.exists();
            (kind.key().to_string(), ProbeTarget { path: path.display().to_string(), exists })
        })
        .collect();
    ProbeAtom {
        atom_id: atom_id.to_string(),
        clothing,
        hair,
        morphs,
        targets,
    }
}

fn probe_package(path: &Path, vam_dir: &Path) -> ProbePackage {
    let names = package_names(path);
    let mut out = ProbePackage {
        file_path: path.display().to_string(),
        package_id: names.package_id.clone(),
        creator: names.creator.clone(),
        sources: Vec::new(),
        error: None,
    };
    let mut var = match open_var(path) {
        Ok(v) => v,
        Err(e) => {
            out.error = Some(e);
            return out;
        }
    };
    let mut entries: Vec<(String, SourceType)> = var
        .names
        .values()
        .filter_map(|(n, _)| source_type(n).map(|t| (n.clone(), t)))
        .collect();
    entries.sort_by_key(|a| a.0.to_lowercase());
    for (internal_path, st) in entries {
        let mut source = ProbeSource {
            label: label_of(&internal_path),
            internal_path: internal_path.clone(),
            source_type: st,
            has_thumb: false,
            atoms: Vec::new(),
            error: None,
        };
        match read_source(&mut var, &internal_path, &names) {
            Ok(src) => {
                source.has_thumb = src.thumb.is_some();
                let people = persons(st, &src.root);
                let multi = people.len() > 1;
                source.atoms = people
                    .iter()
                    .map(|(id, storables)| probe_atom(vam_dir, &names, &internal_path, st, id, multi, storables))
                    .collect();
            }
            Err(e) => source.error = Some(e),
        }
        // Scenes without people (environments, plugins) offer nothing.
        if source.error.is_some() || !source.atoms.is_empty() {
            out.sources.push(source);
        }
    }
    out
}

/// The scenes, looks and appearance presets of each package, with the people
/// in them and the presets each could give.
#[tauri::command(async)]
pub(crate) fn extract_probe(paths: Vec<String>, vam_dir: String) -> Result<Vec<ProbePackage>, String> {
    let vam_dir = PathBuf::from(vam_dir.trim());
    if vam_dir.as_os_str().is_empty() {
        return Err("Set your VaM folder in Settings first.".into());
    }
    Ok(paths
        .par_iter()
        .map(|p| probe_package(Path::new(p), &vam_dir))
        .collect())
}

// ---- Run --------------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ExtractItem {
    pub(crate) file_path: String,
    pub(crate) internal_path: String,
    /// The people to extract; empty = all of them.
    #[serde(default)]
    pub(crate) atom_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ExtractRequest {
    pub(crate) vam_dir: String,
    pub(crate) items: Vec<ExtractItem>,
    pub(crate) kinds: Vec<PresetKind>,
    /// Replace presets that already exist (otherwise they are skipped).
    #[serde(default)]
    pub(crate) overwrite: bool,
    /// Keep expression morphs (smile, eyes closed, …) a scene left set.
    #[serde(default)]
    pub(crate) keep_expressions: bool,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ExtractError {
    pub(crate) source: String,
    pub(crate) reason: String,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ExtractResult {
    pub(crate) written: Vec<String>,
    pub(crate) skipped: Vec<String>,
    /// People with nothing for a kind (no clothing, no set morphs, …).
    pub(crate) empty: usize,
    pub(crate) errors: Vec<ExtractError>,
}

fn write_preset(path: &Path, preset: &Value, thumb: Option<&[u8]>) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    fs::write(path, preset_bytes(preset)?).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    if let Some(jpg) = thumb {
        // Best-effort: a preset without its thumbnail still loads.
        let _ = fs::write(path.with_extension("jpg"), jpg);
    }
    Ok(())
}

pub(crate) fn run_extract(request: &ExtractRequest) -> ExtractResult {
    let vam_dir = PathBuf::from(request.vam_dir.trim());
    let mut result = ExtractResult::default();
    if vam_dir.as_os_str().is_empty() {
        result.errors.push(ExtractError {
            source: String::new(),
            reason: "Set your VaM folder in Settings first.".into(),
        });
        return result;
    }
    // One archive open per package.
    let mut by_package: BTreeMap<&str, Vec<&ExtractItem>> = BTreeMap::new();
    for item in &request.items {
        by_package.entry(item.file_path.as_str()).or_default().push(item);
    }
    for (file_path, items) in by_package {
        let var_path = Path::new(file_path);
        let names = package_names(var_path);
        let mut var = match open_var(var_path) {
            Ok(v) => v,
            Err(reason) => {
                result.errors.push(ExtractError { source: file_path.to_string(), reason });
                continue;
            }
        };
        for item in items {
            let source_label = format!("{}:/{}", names.package_id, item.internal_path);
            let Some(st) = source_type(&item.internal_path) else {
                result.errors.push(ExtractError {
                    source: source_label,
                    reason: "not a scene, look or appearance preset".into(),
                });
                continue;
            };
            let src = match read_source(&mut var, &item.internal_path, &names) {
                Ok(s) => s,
                Err(reason) => {
                    result.errors.push(ExtractError { source: source_label, reason });
                    continue;
                }
            };
            let people = persons(st, &src.root);
            let multi = people.len() > 1;
            for (atom_id, storables) in &people {
                if !item.atom_ids.is_empty() && !item.atom_ids.contains(atom_id) {
                    continue;
                }
                let base = preset_file_base(&names, &item.internal_path, multi.then_some(atom_id.as_str()));
                for &kind in &request.kinds {
                    if !kinds_for(st).contains(&kind) {
                        continue;
                    }
                    let Some(preset) = build_preset(storables, kind, request.keep_expressions) else {
                        result.empty += 1;
                        continue;
                    };
                    let path = preset_path(&vam_dir, kind, &base);
                    if path.exists() && !request.overwrite {
                        result.skipped.push(path.display().to_string());
                        continue;
                    }
                    match write_preset(&path, &preset, src.thumb.as_deref()) {
                        Ok(()) => result.written.push(path.display().to_string()),
                        Err(reason) => result.errors.push(ExtractError {
                            source: format!("{source_label} ({atom_id})"),
                            reason,
                        }),
                    }
                }
            }
        }
    }
    result
}

/// Write the chosen presets. Never fails as a whole: problems are listed per
/// source in `errors`.
#[tauri::command(async)]
pub(crate) fn extract_run(request: ExtractRequest) -> ExtractResult {
    run_extract(&request)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;
    use zip::{write::SimpleFileOptions, ZipWriter};

    fn names(file: &str) -> PackageNames {
        package_names(Path::new(file))
    }

    fn ids(storables: &[Value]) -> Vec<&str> {
        storables.iter().filter_map(storable_id).collect()
    }

    /// One Person's storables, shaped after real scenes.
    fn person() -> Vec<Value> {
        serde_json::from_value(json!([
            { "id": "geometry", "character": "Female 1",
              "clothing": [
                { "id": "Author.Pkg.latest:/Custom/Clothing/Female/Author/Top/Top.vam", "internalId": "Author:Top", "enabled": "true" },
                { "id": "Heat Up Top", "enabled": "true" },
                { "id": "Tank Top", "enabled": "false" } ],
              "hair": [
                { "id": "Author.Pkg.latest:/Custom/Hair/Female/Author/Bun/Bun.vam", "internalId": "Author:Bun", "enabled": "true" } ],
              "morphs": [
                { "uid": "Breast Size", "name": "Breast Size", "value": "0.4" },
                { "uid": "Smile Open Full Face", "name": "Smile Open Full Face", "value": "0.8" },
                { "uid": "Other.Pkg.3:/Custom/Atom/Person/Morphs/female/Other/Painter Jaw.vmi", "name": "Painter Jaw", "value": "1" },
                { "uid": "Unused", "name": "Unused", "value": "0" } ] },
            { "id": "Author:TopSim", "simEnabled": "true" },
            { "id": "Author:TopItemControl", "disableAnatomy": "false" },
            { "id": "HeatUpTopMaterial", "Diffuse Color": { "h": "0.1" } },
            { "id": "Author:BunSim", "simulationEnabled": "true" },
            { "id": "Sim2HairStyle", "curl": "0.2" },
            { "id": "HairPresets", "presetName": "x" },
            { "id": "skin", "Diffuse Color": { "h": "0.5" } },
            { "id": "BreastControl", "mass": "0.5", "positionSpringZ": "250", "targetRotationX": "3" },
            { "id": "control", "position": { "x": "0" }, "rotation": { "x": "0" } },
            { "id": "headControl", "position": { "x": "0" }, "rotation": { "x": "0" }, "holdPositionSpring": "1" },
            { "id": "lThigh", "position": { "x": "0" }, "rotation": { "x": "0" } },
            { "id": "chestAnimation", "steps": [] },
            { "id": "LabiaTrigger", "trigger": {} },
            { "id": "PluginManager", "plugins": {} },
            { "id": "plugin#0_VamTimeline.AtomPlugin", "x": "1" },
            { "id": "AppearancePresets", "presetName": "x" }
        ]))
        .unwrap()
    }

    #[test]
    fn package_names_point_self_at_latest() {
        let n = names("D:/VaM/AddonPackages/Author.Pkg.3.var");
        assert_eq!((n.creator.as_str(), n.name.as_str()), ("Author", "Pkg"));
        assert_eq!(n.self_ref, "Author.Pkg.latest");
        assert_eq!(names("Author.Multi.Part.12.var").name, "Multi.Part");
        assert_eq!(names("Author.Pkg.var").self_ref, "Author.Pkg");
    }

    #[test]
    fn preset_names_carry_package_scene_and_atom() {
        let n = names("Author.Pkg.3.var");
        assert_eq!(preset_file_base(&n, "Saves/scene/Demo.json", None), "Preset_Author - Pkg - Demo");
        // A scene named after its package isn't doubled.
        assert_eq!(preset_file_base(&n, "Saves/scene/pkg.json", None), "Preset_Author - pkg");
        assert_eq!(
            preset_file_base(&n, "Saves/scene/Demo.json", Some("Person#2")),
            "Preset_Author - Pkg - Demo_Person2"
        );
        assert_eq!(
            preset_file_base(&n, "Saves/scene/Demo.json", Some("Person/2")),
            "Preset_Author - Pkg - Demo_Person-2"
        );
        assert_eq!(
            preset_file_base(&n, "Custom/Atom/Person/Appearance/Preset_Casey_v2.vap", None),
            "Preset_Author - Pkg - Casey_v2"
        );
        let p = preset_path(Path::new("D:/VaM"), PresetKind::Hair, "Preset_A - B");
        assert!(p.ends_with("Custom/Atom/Person/Hair/extracted/Preset_A - B.vap"));
    }

    #[test]
    fn source_types_by_path() {
        assert_eq!(source_type("Saves/scene/a.json"), Some(SourceType::Scene));
        assert_eq!(source_type("Saves/Person/Appearance/a.json"), Some(SourceType::Look));
        assert_eq!(
            source_type("Custom/Atom/Person/Appearance/x/Preset_a.vap"),
            Some(SourceType::AppearancePreset)
        );
        assert_eq!(source_type("Saves/scene/a.jpg"), None);
        assert_eq!(source_type("Custom/Atom/Person/Clothing/Preset_a.vap"), None);
    }

    #[test]
    fn appearance_keeps_physics_and_drops_scene_only_storables() {
        let out = filter_appearance(&person(), false);
        let kept = ids(&out);
        for id in ["geometry", "Author:TopSim", "Author:TopItemControl", "HeatUpTopMaterial", "skin", "BreastControl"] {
            assert!(kept.contains(&id), "{id} should stay: {kept:?}");
        }
        for id in [
            "control",
            "headControl",
            "lThigh",
            "chestAnimation",
            "LabiaTrigger",
            "PluginManager",
            "plugin#0_VamTimeline.AtomPlugin",
            "AppearancePresets",
            "HairPresets",
        ] {
            assert!(!kept.contains(&id), "{id} should go: {kept:?}");
        }
        let breast = out.iter().find(|s| storable_id(s) == Some("BreastControl")).unwrap();
        assert_eq!(breast["positionSpringZ"], "250", "physics keys that mention position stay");
        assert_eq!(breast["targetRotationX"], "3");
        let geo = out.iter().find(|s| storable_id(s) == Some("geometry")).unwrap();
        let morphs: Vec<&str> = geo["morphs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["name"].as_str().unwrap())
            .collect();
        assert_eq!(morphs, ["Breast Size", "Painter Jaw"], "expressions and unset morphs go, 'Painter' is not 'Pain'");
        assert_eq!(geo["character"], "Female 1");
    }

    #[test]
    fn clothing_preset_holds_its_items_including_built_ins() {
        let out = filter_clothing(&person());
        assert_eq!(ids(&out), ["geometry", "Author:TopSim", "Author:TopItemControl", "HeatUpTopMaterial"]);
        let geo = out[0].as_object().unwrap();
        assert_eq!(geo.keys().collect::<Vec<_>>(), ["clothing", "id"]);
        assert_eq!(geo["clothing"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn hair_preset_holds_hair_items_and_built_in_hair_storables() {
        let out = filter_hair(&person());
        assert_eq!(ids(&out), ["geometry", "Author:BunSim", "Sim2HairStyle"]);
        assert!(out[0].get("clothing").is_none());
    }

    #[test]
    fn morph_preset_holds_set_morphs() {
        let out = filter_morphs(&person(), false);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["morphs"].as_array().unwrap().len(), 2);
        assert_eq!(filter_morphs(&person(), true)[0]["morphs"].as_array().unwrap().len(), 3);
        // A person with no set morphs gives no preset at all.
        let bare = vec![json!({ "id": "geometry", "morphs": [{ "uid": "A", "value": "0" }] })];
        assert!(build_preset(&bare, PresetKind::Morphs, false).is_none());
        let preset = build_preset(&person(), PresetKind::Morphs, false).unwrap();
        assert_eq!(preset["setUnlistedParamsToDefault"], "true");
    }

    #[test]
    fn expression_morphs_by_name() {
        for name in ["Smile Open Full Face", "Eyelids Top Down Left", "Brow Inner Up", "Pain", "Mouth Open", "OpenXXL"] {
            assert!(is_expression_morph(name), "{name}");
        }
        for name in ["Painter Jaw", "Breast Size", "Eyelids Top Height", "Browser"] {
            assert!(!is_expression_morph(name), "{name}");
        }
    }

    fn write_var(path: &Path, files: &[(&str, &[u8])]) {
        let mut zip = ZipWriter::new(fs::File::create(path).unwrap());
        for (name, data) in files {
            zip.start_file(*name, SimpleFileOptions::default()).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
    }

    #[test]
    fn extract_writes_presets_with_self_rewritten_and_thumbnails() {
        let root = std::env::temp_dir().join(format!("vam_extract_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let addon = root.join("AddonPackages");
        fs::create_dir_all(&addon).unwrap();
        let var = addon.join("Author.Pkg.3.var");
        let scene = json!({ "atoms": [
            { "id": "Person", "type": "Person", "storables": [
                { "id": "geometry",
                  "clothing": [{ "id": "SELF:/Custom/Clothing/Female/Author/Top/Top.vam", "internalId": "Author:Top", "enabled": "true" }],
                  "hair": [],
                  "morphs": [{ "uid": "SELF:/Custom/Atom/Person/Morphs/female/Author/Body.vmi", "name": "Body", "value": "0.7" }] },
                { "id": "Author:TopSim", "simEnabled": "true" } ] },
            { "id": "Cam", "type": "WindowCamera", "storables": [] } ] });
        // VaM's SimpleJSON leaves trailing commas; the reader copes.
        let text = serde_json::to_string_pretty(&scene)
            .unwrap()
            .replacen("\"value\": \"0.7\"", "\"value\": \"0.7\",", 1);
        write_var(
            &var,
            &[("Saves/scene/Demo.json", text.as_bytes()), ("Saves/scene/Demo.jpg", b"JPG"), ("meta.json", b"{}")],
        );

        let probe = probe_package(&var, &root);
        assert_eq!(probe.sources.len(), 1);
        let atom = &probe.sources[0].atoms[0];
        assert_eq!((atom.clothing, atom.hair, atom.morphs), (1, 0, 1));
        assert_eq!(
            atom.targets.keys().collect::<Vec<_>>(),
            ["appearance", "clothing", "morphs"],
            "no hair, no hair preset"
        );
        assert!(probe.sources[0].has_thumb);

        let request = ExtractRequest {
            vam_dir: root.display().to_string(),
            items: vec![ExtractItem {
                file_path: var.display().to_string(),
                internal_path: "Saves/scene/Demo.json".into(),
                atom_ids: vec![],
            }],
            kinds: vec![PresetKind::Clothing, PresetKind::Hair, PresetKind::Morphs],
            overwrite: false,
            keep_expressions: false,
        };
        let first = run_extract(&request);
        assert!(first.errors.is_empty(), "{:?}", first.errors);
        assert_eq!(first.written.len(), 2);
        assert_eq!(first.empty, 1, "hair: nothing to write");
        let clothing = root.join("Custom/Atom/Person/Clothing/extracted/Preset_Author - Pkg - Demo.vap");
        let written = fs::read_to_string(&clothing).unwrap();
        assert!(written.contains("Author.Pkg.latest:/Custom/Clothing"), "{written}");
        assert!(!written.contains("SELF:/"));
        assert!(written.starts_with("{\n   \""), "three-space indent like VaM");
        assert_eq!(fs::read(clothing.with_extension("jpg")).unwrap(), b"JPG");
        let morphs =
            fs::read_to_string(root.join("Custom/Atom/Person/Morphs/extracted/Preset_Author - Pkg - Demo.vap")).unwrap();
        assert!(morphs.contains("Author.Pkg.latest:/Custom/Atom/Person/Morphs/female/Author/Body.vmi"));

        // Existing presets are skipped unless overwrite is asked for.
        let second = run_extract(&request);
        assert_eq!((second.written.len(), second.skipped.len()), (0, 2));
        assert!(probe_package(&var, &root).sources[0].atoms[0].targets["clothing"].exists);
        let third = run_extract(&ExtractRequest { overwrite: true, ..request });
        assert_eq!(third.written.len(), 2);

        let _ = fs::remove_dir_all(&root);
    }

    /// Real-library check: probes every package under `VAM_EXTRACT_CHECK_ADDON`
    /// (an AddonPackages folder) and extracts every kind into a scratch VaM dir
    /// — never the real one. `cargo test extract_real_library -- --ignored --nocapture`
    #[test]
    #[ignore = "needs VAM_EXTRACT_CHECK_ADDON pointing at an AddonPackages folder"]
    fn extract_real_library() {
        let addon = PathBuf::from(std::env::var("VAM_EXTRACT_CHECK_ADDON").expect("set VAM_EXTRACT_CHECK_ADDON"));
        let out = std::env::temp_dir().join(format!("vam_extract_real_{}", std::process::id()));
        let _ = fs::remove_dir_all(&out);
        let vars: Vec<PathBuf> = walkdir::WalkDir::new(&addon)
            .into_iter()
            .filter_map(Result::ok)
            .map(|e| e.into_path())
            .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("var")))
            .collect();
        let paths: Vec<String> = vars.iter().map(|p| p.display().to_string()).collect();
        let started = std::time::Instant::now();
        let probe = extract_probe(paths, out.display().to_string()).unwrap();
        let probe_ms = started.elapsed().as_millis();
        let (mut sources, mut people, mut bad) = (0, 0, 0);
        let mut items = Vec::new();
        for pkg in &probe {
            for src in &pkg.sources {
                sources += 1;
                people += src.atoms.len();
                if let Some(e) = &src.error {
                    bad += 1;
                    println!("unreadable {}:/{} — {e}", pkg.package_id, src.internal_path);
                }
                items.push(ExtractItem {
                    file_path: pkg.file_path.clone(),
                    internal_path: src.internal_path.clone(),
                    atom_ids: vec![],
                });
            }
        }
        let result = run_extract(&ExtractRequest {
            vam_dir: out.display().to_string(),
            items,
            kinds: PresetKind::ALL.to_vec(),
            overwrite: true,
            keep_expressions: false,
        });
        println!(
            "{} packages, {sources} sources ({bad} unreadable), {people} people; probe {probe_ms} ms;              written {}, empty {}, errors {}",
            probe.len(),
            result.written.len(),
            result.empty,
            result.errors.len()
        );
        for e in result.errors.iter().take(10) {
            println!("error {}: {}", e.source, e.reason);
        }
        // Everything written must read back as a VaM preset.
        for path in &result.written {
            let v: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
            assert!(v["storables"].as_array().is_some_and(|s| !s.is_empty()), "{path}");
        }
        println!("output kept in {}", out.display());
    }
}
