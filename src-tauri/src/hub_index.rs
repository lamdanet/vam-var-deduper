//! The Hub's package index, after VaM Backstage's `hub/packages-json.js`.
//!
//! `https://s3cdn.virtamate.com/data/packages.json` maps every `.var` the Hub
//! serves (exact file name) to its resource id. From it the Library learns,
//! without a request per package:
//!
//! - which packages have a newer version on the Hub (the newest local
//!   version of a family against the Hub's newest),
//! - which aren't on the Hub at all ("Not on Hub": once deleted they can't be
//!   downloaded again),
//! - each package's Hub resource, for "View on Hub".
//!
//! The CDN edge keeps whatever it last served for a predictable URL, sometimes
//! for days, so every request carries a random `?cb=` and an `If-None-Match`
//! with the stored ETag (a 304 keeps what we have). Body and ETag are cached
//! in the app config folder so the index works offline from the first launch
//! on.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use serde_json::Value;

use crate::{
    models::VarPackageListItem,
    naming::{package_base, package_version},
};

const PACKAGES_JSON_URL: &str = "https://s3cdn.virtamate.com/data/packages.json";
/// A refresh within this long of the last one is skipped unless forced.
const FRESH_FOR: Duration = Duration::from_secs(30 * 60);

/// The parsed index.
#[derive(Debug, Default)]
pub(crate) struct HubIndex {
    /// Lowercased file name (`creator.name.3.var`) → resource id.
    files: HashMap<String, String>,
    /// Lowercased package family (`creator.name`) → its newest numbered version
    /// on the Hub: (version, file name, resource id).
    families: HashMap<String, (u64, String, String)>,
}

impl HubIndex {
    pub(crate) fn parse(body: &[u8]) -> Result<Self, String> {
        let value: Value = serde_json::from_slice(body).map_err(|e| format!("packages.json is not JSON: {e}"))?;
        let map = value.as_object().ok_or("packages.json is not an object")?;
        let mut index = HubIndex::default();
        for (file, rid) in map {
            let rid = match rid {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                _ => continue,
            };
            let lc = file.to_ascii_lowercase();
            let stem = lc.strip_suffix(".var").unwrap_or(&lc);
            if let Some(version) = package_version(stem) {
                let base = package_base(stem).to_string();
                let newer = index.families.get(&base).is_none_or(|(v, _, _)| version > *v);
                if newer {
                    index.families.insert(base, (version, file.clone(), rid.clone()));
                }
            }
            index.files.insert(lc, rid);
        }
        if index.files.is_empty() {
            return Err("packages.json lists no packages".into());
        }
        Ok(index)
    }

    pub(crate) fn len(&self) -> usize {
        self.files.len()
    }

    /// Resource id for an exact file name.
    pub(crate) fn file_resource(&self, file_name: &str) -> Option<&str> {
        self.files.get(&file_name.to_ascii_lowercase()).map(String::as_str)
    }

    /// The Hub's newest numbered version of a package's family.
    pub(crate) fn newest(&self, package_id: &str) -> Option<&(u64, String, String)> {
        self.families.get(package_base(&package_id.to_ascii_lowercase()))
    }
}

#[derive(Default)]
struct IndexState {
    index: Option<Arc<HubIndex>>,
    etag: Option<String>,
    fetched_at_ms: Option<u64>,
    last_error: Option<String>,
}

static STATE: OnceLock<Mutex<IndexState>> = OnceLock::new();
static CACHE_DIR: OnceLock<PathBuf> = OnceLock::new();
/// Bumped whenever the loaded index changes, so annotated library items know
/// to refresh.
static GENERATION: AtomicU64 = AtomicU64::new(1);

fn state() -> &'static Mutex<IndexState> {
    STATE.get_or_init(Default::default)
}

pub(crate) fn generation() -> u64 {
    GENERATION.load(Ordering::SeqCst)
}

pub(crate) fn current() -> Option<Arc<HubIndex>> {
    state().lock().ok().and_then(|s| s.index.clone())
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

fn cache_files() -> Option<(PathBuf, PathBuf)> {
    let dir = CACHE_DIR.get()?;
    Some((dir.join("hub_packages.json"), dir.join("hub_packages.etag")))
}

fn install(index: HubIndex, etag: Option<String>, fetched_at_ms: Option<u64>) {
    if let Ok(mut s) = state().lock() {
        s.index = Some(Arc::new(index));
        s.etag = etag;
        if fetched_at_ms.is_some() {
            s.fetched_at_ms = fetched_at_ms;
        }
        s.last_error = None;
    }
    GENERATION.fetch_add(1, Ordering::SeqCst);
}

/// Remember where the cache lives and load it in the background, so the
/// Library has an index before the first network round trip.
pub(crate) fn init(config_dir: PathBuf) {
    let _ = CACHE_DIR.set(config_dir);
    std::thread::spawn(|| {
        let Some((body_path, etag_path)) = cache_files() else { return };
        let Ok(body) = std::fs::read(&body_path) else { return };
        if let Ok(index) = HubIndex::parse(&body) {
            let etag = std::fs::read_to_string(etag_path).ok().filter(|e| !e.trim().is_empty());
            if current().is_none() {
                install(index, etag, None);
            }
        }
    });
}

/// A cache-busting token: the CDN must not answer from its edge copy.
fn cache_buster() -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    format!("{nanos:x}{:x}", SEQ.fetch_add(1, Ordering::SeqCst))
}

fn fetch(force: bool) -> Result<(), String> {
    let (etag, have_index) = {
        let s = state().lock().map_err(|_| "index state poisoned")?;
        (s.etag.clone(), s.index.is_some())
    };
    let client = crate::hub::hub_client().map_err(|e| e.to_string())?;
    let mut req = client.get(format!("{PACKAGES_JSON_URL}?cb={}", cache_buster()));
    if let (false, true, Some(tag)) = (force, have_index, etag.as_deref()) {
        req = req.header(reqwest::header::IF_NONE_MATCH, tag);
    }
    let resp = req.send().map_err(|e| format!("couldn't reach the Hub package index: {e}"))?;
    if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
        if let Ok(mut s) = state().lock() {
            s.fetched_at_ms = Some(now_ms());
            s.last_error = None;
        }
        return Ok(());
    }
    if !resp.status().is_success() {
        return Err(format!("the Hub package index answered HTTP {}", resp.status()));
    }
    let new_etag = resp
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let body = resp.bytes().map_err(|e| format!("reading the Hub package index failed: {e}"))?;
    let index = HubIndex::parse(&body)?;
    if let Some((body_path, etag_path)) = cache_files() {
        let _ = std::fs::write(body_path, &body);
        let _ = std::fs::write(etag_path, new_etag.as_deref().unwrap_or(""));
    }
    install(index, new_etag, Some(now_ms()));
    Ok(())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HubIndexStatus {
    pub(crate) loaded: bool,
    pub(crate) packages: usize,
    /// When it was last checked against the Hub (ms since the epoch); `None`
    /// while only the on-disk copy is loaded.
    pub(crate) fetched_at_ms: Option<u64>,
    pub(crate) error: Option<String>,
    pub(crate) generation: u64,
}

fn status() -> HubIndexStatus {
    let s = state().lock().ok();
    HubIndexStatus {
        loaded: s.as_ref().is_some_and(|s| s.index.is_some()),
        packages: s.as_ref().and_then(|s| s.index.as_ref().map(|i| i.len())).unwrap_or(0),
        fetched_at_ms: s.as_ref().and_then(|s| s.fetched_at_ms),
        error: s.as_ref().and_then(|s| s.last_error.clone()),
        generation: generation(),
    }
}

/// Bring the index up to date (skipped when checked within the last 30
/// minutes, unless `force`). Never fails: a network problem is reported in
/// `error` and the cached index stays in use.
#[tauri::command]
pub(crate) async fn hub_index_refresh(force: bool) -> HubIndexStatus {
    let fresh = state()
        .lock()
        .ok()
        .and_then(|s| s.fetched_at_ms)
        .is_some_and(|at| now_ms().saturating_sub(at) < FRESH_FOR.as_millis() as u64);
    if force || !fresh {
        let result = crate::library::on_hub_thread(move || fetch(force)).await;
        if let Err(e) = result {
            if let Ok(mut s) = state().lock() {
                s.last_error = Some(e);
            }
        }
    }
    status()
}

#[tauri::command]
pub(crate) fn hub_index_status() -> HubIndexStatus {
    status()
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HubIndexHit {
    pub(crate) package_id: String,
    /// `None` when the index isn't loaded (unknown), else whether this exact
    /// file is on the Hub.
    pub(crate) on_hub: Option<bool>,
    pub(crate) resource_id: Option<String>,
}

/// Per package id: is this exact version on the Hub, and its resource.
#[tauri::command]
pub(crate) fn hub_index_lookup(package_ids: Vec<String>) -> Vec<HubIndexHit> {
    let index = current();
    package_ids
        .into_iter()
        .map(|id| {
            let file = format!("{id}.var");
            let exact = index.as_ref().and_then(|i| i.file_resource(&file).map(str::to_string));
            let family = index.as_ref().and_then(|i| i.newest(&id).map(|(_, _, rid)| rid.clone()));
            HubIndexHit {
                on_hub: index.as_ref().map(|_| exact.is_some()),
                resource_id: exact.or(family),
                package_id: id,
            }
        })
        .collect()
}

/// Fill the Hub fields of library items: `on_hub`, `hub_resource_id`, and
/// `hub_update_*` on the newest local version of each family whose Hub
/// version is newer still.
pub(crate) fn annotate(items: &mut [VarPackageListItem]) {
    let index = current();
    let mut newest_local: HashMap<String, u64> = HashMap::new();
    for item in items.iter() {
        if let Some(v) = package_version(&item.package_id) {
            let base = package_base(&item.package_id).to_ascii_lowercase();
            let e = newest_local.entry(base).or_insert(v);
            *e = (*e).max(v);
        }
    }
    for item in items.iter_mut() {
        item.on_hub = None;
        item.hub_resource_id = None;
        item.hub_update_version = None;
        item.hub_update_file = None;
        let Some(index) = index.as_ref() else { continue };
        let exact = index.file_resource(&item.file_name).map(str::to_string);
        item.on_hub = Some(exact.is_some());
        let newest = index.newest(&item.package_id);
        item.hub_resource_id = exact.or_else(|| newest.map(|(_, _, rid)| rid.clone()));
        let (Some(version), Some((hub_version, hub_file, _))) = (package_version(&item.package_id), newest) else {
            continue;
        };
        let base = package_base(&item.package_id).to_ascii_lowercase();
        let local_max = newest_local.get(&base).copied().unwrap_or(version);
        if version == local_max && *hub_version > local_max {
            item.hub_update_version = Some(*hub_version);
            item.hub_update_file = Some(hub_file.clone());
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HubExactFile {
    pub(crate) filename: String,
    pub(crate) url: String,
    pub(crate) size: Option<u64>,
}

/// A download link for exactly `file_name` (`Creator.Name.7.var`). The Hub
/// answers with the nearest version it has; anything but the asked-for file
/// is refused, so an update never quietly downloads something else.
#[tauri::command]
pub(crate) async fn hub_exact_download(file_name: String) -> Result<Option<HubExactFile>, String> {
    let stem = file_name
        .trim()
        .strip_suffix(".var")
        .or_else(|| file_name.trim().strip_suffix(".VAR"))
        .unwrap_or(file_name.trim())
        .to_string();
    crate::library::on_hub_thread(move || {
        let client = crate::hub::hub_client().map_err(|e| e.to_string())?;
        let (found, error) = crate::hub::resolve_download_urls(&client, std::slice::from_ref(&stem));
        if let Some(hit) = found.get(&stem) {
            let got = hit.filename.trim().to_ascii_lowercase();
            let want = format!("{}.var", stem.to_ascii_lowercase());
            if got == want || got == stem.to_ascii_lowercase() {
                return Ok(Some(HubExactFile {
                    filename: format!("{stem}.var"),
                    url: hit.download_url.clone(),
                    size: hit.file_size,
                }));
            }
            return Ok(None);
        }
        match error {
            Some(e) => Err(e),
            None => Ok(None),
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(file: &str) -> VarPackageListItem {
        VarPackageListItem {
            file_name: format!("{file}.var"),
            package_id: file.to_string(),
            ..VarPackageListItem::default()
        }
    }

    #[test]
    fn parses_files_and_newest_family_versions() {
        let index = HubIndex::parse(
            br#"{"A.Pkg.1.var": 10, "A.Pkg.10.var": 10, "A.Pkg.9.var": "10", "B.Other.latest.var": 5, "junk": null}"#,
        )
        .unwrap();
        assert_eq!(index.file_resource("a.pkg.9.var"), Some("10"));
        assert_eq!(index.newest("A.Pkg.2").map(|e| e.0), Some(10), "10 beats 9 numerically");
        assert!(index.newest("B.Other.1").is_none(), "no numbered version");
        assert!(HubIndex::parse(b"{}").is_err());
    }

    #[test]
    fn annotate_marks_updates_on_the_newest_local_only() {
        install(
            HubIndex::parse(br#"{"A.Pkg.5.var": 1, "A.Pkg.3.var": 1, "C.Gone.2.var": 3}"#).unwrap(),
            None,
            None,
        );
        let mut items = vec![item("A.Pkg.2"), item("A.Pkg.3"), item("B.Local.1"), item("C.Gone.4")];
        annotate(&mut items);
        assert_eq!(items[0].hub_update_version, None, "an older local copy isn't the one to update");
        assert_eq!(items[1].hub_update_version, Some(5));
        assert_eq!(items[1].hub_update_file.as_deref(), Some("A.Pkg.5.var"));
        assert_eq!(items[1].on_hub, Some(true));
        assert_eq!(items[0].on_hub, Some(false), "A.Pkg.2 itself isn't on the Hub");
        assert_eq!(items[0].hub_resource_id.as_deref(), Some("1"), "but its family is");
        assert_eq!(items[2].on_hub, Some(false));
        assert_eq!(items[2].hub_resource_id, None);
        assert_eq!(items[3].hub_update_version, None, "local is newer than the Hub");
    }
}
