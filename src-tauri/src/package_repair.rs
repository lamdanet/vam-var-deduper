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

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::State;
use zip::{write::SimpleFileOptions, CompressionMethod, System, ZipArchive, ZipWriter};

use crate::{
    execute::rewrite_external_text_payload,
    fix_var::{collect_pkg_refs, free_backup_path, is_text_path},
    models::{AppState, ProgressPayload, TaskHandle, META_PATH},
    tasks::{new_progress_payload, set_task_progress},
    utils::{byte_step, copy_with_progress, decode_text, dump_json_bytes, normalize_zip_path, read_json_bytes, Progress},
};

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

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct RepairReport {
    pub(crate) package_id: String,
    pub(crate) deps: Vec<RepairDep>,
    /// Packages its files refer to that meta.json doesn't list.
    pub(crate) undeclared: Vec<RepairUndeclared>,
    pub(crate) names: Vec<RepairName>,
    /// References to other packages for files this package has.
    pub(crate) redirectable: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct RepairRequest {
    pub(crate) file_path: String,
    /// meta.json keys to remove. Only ones nothing would still refer to after
    /// the repair are removed; the rest are reported as kept.
    #[serde(default)]
    pub(crate) remove_deps: Vec<String>,
    /// Point references to files this package has at its own copy.
    #[serde(default)]
    pub(crate) redirect: bool,
    #[serde(default)]
    pub(crate) backup: bool,
    #[serde(default)]
    pub(crate) backup_dir: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct RepairResult {
    pub(crate) removed_deps: Vec<String>,
    pub(crate) kept_deps: Vec<String>,
    pub(crate) redirected: u32,
    pub(crate) renamed: u32,
    pub(crate) backup_path: Option<String>,
}

fn base_lc(id: &str) -> String {
    crate::naming::package_base(id).to_ascii_lowercase()
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
        samples: Vec<String>,
    }
    let mut groups: HashMap<String, Group> = HashMap::new();
    let mut redirects = BTreeMap::new();
    let mut meta: Option<Value> = None;
    for i in 0..archive.len() {
        let mut e = archive.by_index(i)?;
        if e.is_dir() {
            continue;
        }
        let inner = normalize_zip_path(e.name());
        if inner.eq_ignore_ascii_case(META_PATH) {
            let mut raw = Vec::new();
            e.read_to_end(&mut raw)?;
            meta = read_json_bytes(&raw, "meta.json").ok();
            continue;
        }
        if !is_text_path(&inner) {
            continue;
        }
        let mut raw = Vec::new();
        e.read_to_end(&mut raw)?;
        let Some((text, _)) = decode_text(&raw) else { continue };
        for (pkg, ref_path) in collect_pkg_refs(&text) {
            if pkg == "SELF" || base_lc(&pkg) == own {
                continue;
            }
            let g = groups.entry(base_lc(&pkg)).or_default();
            if g.display.is_empty() {
                g.display = pkg.clone();
            }
            g.refs += 1;
            if own_paths.contains(&normalize_zip_path(&ref_path).to_lowercase()) {
                g.inside += 1;
                redirects.insert(format!("{pkg}:/{ref_path}"), format!("SELF:/{ref_path}"));
            } else {
                match ref_kind(&ref_path) {
                    "plugin" => g.plugins += 1,
                    "morph" => g.morphs += 1,
                    _ => g.others += 1,
                }
                if g.samples.len() < SAMPLE_CAP {
                    g.samples.push(format!("{pkg}:/{ref_path}"));
                }
            }
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
            let status = if b == own {
                "self"
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

    let report = RepairReport {
        package_id,
        deps,
        undeclared,
        names: names.values().map(|(stored, fixed)| RepairName { stored: stored.clone(), fixed: fixed.clone() }).collect(),
        redirectable: redirects.len() as u32,
    };
    Ok(Scan { report, redirects, names })
}

/// What could be repaired in a package. Reads its text files once.
#[tauri::command(async)]
pub(crate) fn inspect_package_repair(file_path: String) -> Result<RepairReport, String> {
    scan(Path::new(&file_path)).map(|s| s.report).map_err(|e| e.to_string())
}

/// Rewrites the package with the chosen repairs (file names are always put
/// right when it's rewritten). Binary entries are copied without
/// recompressing; the new file replaces the old one only once it's complete.
pub(crate) fn repair_package(request: &RepairRequest, progress: Progress<'_>) -> Result<RepairResult> {
    let target = Path::new(&request.file_path);
    progress(0.01, "Reading the package");
    let found = scan(target)?;
    let redirects = if request.redirect { found.redirects.clone() } else { BTreeMap::new() };
    let mut result = RepairResult::default();

    // Only what nothing would still use goes.
    let wanted: HashSet<&str> = request.remove_deps.iter().map(String::as_str).collect();
    let mut remove: BTreeSet<String> = BTreeSet::new();
    for d in &found.report.deps {
        if !wanted.contains(d.key.as_str()) {
            continue;
        }
        let free = matches!(d.status.as_str(), "self" | "unused") || (d.status == "covered" && request.redirect);
        if free {
            remove.insert(d.key.clone());
        } else {
            result.kept_deps.push(d.key.clone());
        }
    }
    if remove.is_empty() && redirects.is_empty() && found.names.is_empty() {
        return Err(anyhow!("Nothing to repair."));
    }

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
                let mut meta = read_json_bytes(&raw, "meta.json")?;
                if let Some(deps) = meta.get_mut("dependencies").and_then(Value::as_object_mut) {
                    for key in &remove {
                        if deps.remove(key).is_some() {
                            result.removed_deps.push(key.clone());
                        }
                    }
                }
                out.start_file(&name, options)?;
                out.write_all(&dump_json_bytes(&meta)?)?;
            } else if !redirects.is_empty() && is_text_path(&inner) {
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

fn finish(tasks: &Arc<Mutex<HashMap<u64, ProgressPayload>>>, task_id: u64, result: Result<RepairResult>) {
    if let Ok(mut guard) = tasks.lock() {
        if let Some(task) = guard.get_mut(&task_id) {
            task.done = true;
            task.progress = 1.0;
            match result {
                Ok(r) => {
                    task.phase = "repair_complete".to_string();
                    task.message = "Repaired".to_string();
                    task.repair_result = Some(r);
                    task.error = None;
                }
                Err(e) => {
                    task.phase = "repair_failed".to_string();
                    task.message = "Repair failed".to_string();
                    task.error = Some(e.to_string());
                }
            }
        }
    }
}

#[tauri::command]
pub(crate) fn start_repair_package_task(request: RepairRequest, state: State<'_, AppState>) -> Result<TaskHandle, String> {
    let task_id = crate::db_location::next_task_id(&state)?;
    state
        .tasks
        .lock()
        .map_err(|_| "task state poisoned".to_string())?
        .insert(task_id, new_progress_payload("repair_starting", "Starting the repair"));
    let tasks = Arc::clone(&state.tasks);
    let folder_cache = Arc::clone(&state.var_packages_folder_cache);
    let generation = Arc::clone(&state.var_packages_cache_generation);
    thread::spawn(move || {
        let result = repair_package(&request, &|f, m| set_task_progress(&tasks, task_id, "repair_writing", f, m));
        // Its dependencies changed: the listing has to read it again.
        generation.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if let Ok(mut cache) = folder_cache.lock() {
            *cache = None;
        }
        finish(&tasks, task_id, result);
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
                redirect: true,
                backup: true,
                backup_dir: Some(dir.join("backup").display().to_string()),
            },
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
}
