//! Download sources the user adds by hand: a Pixeldrain (or MediaFire, or any
//! direct) link for one `.var`, stored in the same `download_links` table the
//! imported mirror lists fill, so every download path picks it up.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::{db::Db, models::DownloadLinkRow};

/// Normalizes a pasted link and names its host: `"pixeldrain"`, `"mediafire"`,
/// `"mega"` or `"other"`. A Pixeldrain share page (`/u/<id>`) becomes the file's direct
/// download URL — the page itself is HTML, not the `.var`. `None` when it isn't
/// an http(s) link.
pub(crate) fn normalize_link(raw: &str) -> Option<(String, &'static str)> {
    let url = raw.trim().trim_matches(|c| c == '"' || c == '\'' || c == '<' || c == '>');
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return None;
    }
    if lower.contains("pixeldrain.") || lower.contains("pixeldra.in") {
        return Some(match pixeldrain_id(url) {
            Some(id) => (format!("https://pixeldrain.com/api/file/{id}?download"), "pixeldrain"),
            None => (url.to_string(), "pixeldrain"),
        });
    }
    if lower.contains("mediafire.com") {
        return Some((url.to_string(), "mediafire"));
    }
    if crate::mega::is_mega(url) {
        return Some((url.to_string(), "mega"));
    }
    Some((url.to_string(), "other"))
}

/// The file id in a Pixeldrain `/u/<id>` or `/api/file/<id>` link.
fn pixeldrain_id(url: &str) -> Option<&str> {
    for marker in ["/u/", "/api/file/"] {
        if let Some(pos) = url.find(marker) {
            let rest = &url[pos + marker.len()..];
            let end = rest.find(|c: char| !c.is_ascii_alphanumeric()).unwrap_or(rest.len());
            if end > 0 {
                return Some(&rest[..end]);
            }
        }
    }
    None
}

/// `%20`-style escapes in a URL path segment, decoded (lossy on bad bytes).
fn percent_decode(segment: &str) -> String {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct LinkInfo {
    /// The link as it will be stored (normalized).
    pub(crate) url: String,
    pub(crate) host: String,
    /// The `.var` the link serves, when it could be told: Pixeldrain's file
    /// info, or a URL ending in `.var`.
    pub(crate) filename: Option<String>,
    pub(crate) size: Option<u64>,
    /// Why the link can't be used, or what couldn't be checked.
    pub(crate) error: Option<String>,
    /// A MEGA folder link: the `.var` files inside, to pick from.
    pub(crate) folder_files: Option<Vec<crate::mega::FolderEntry>>,
}

#[derive(Deserialize)]
struct PixeldrainInfo {
    name: String,
    size: Option<u64>,
}

/// Looks a pasted link over before it is saved: normalizes it and, for
/// Pixeldrain, asks for the file's real name and size. The blocking HTTP
/// client must not run on the async runtime's threads, so the work goes to a
/// plain thread (as the Hub calls do).
#[tauri::command]
pub(crate) async fn inspect_download_link(url: String) -> Result<LinkInfo, String> {
    crate::library::on_hub_thread(move || Ok(inspect_link(&url))).await
}

pub(crate) fn inspect_link(url: &str) -> LinkInfo {
    let Some((url, host)) = normalize_link(url) else {
        return LinkInfo {
            error: Some("Paste an http(s) link.".to_string()),
            ..Default::default()
        };
    };
    let mut info = LinkInfo { url: url.clone(), host: host.to_string(), ..Default::default() };
    if host == "mega" {
        match crate::mega::inspect(&url) {
            Ok(found) => {
                info.filename = found.name;
                info.size = found.size;
                if let Some(files) = found.files {
                    if files.is_empty() {
                        info.error = Some("That MEGA folder has no .var files.".to_string());
                    }
                    info.folder_files = Some(files);
                }
            }
            Err(err) => info.error = Some(err),
        }
    } else if host == "pixeldrain" {
        match pixeldrain_id(&url) {
            Some(id) => match fetch_pixeldrain_info(id) {
                Ok(pd) => {
                    info.filename = Some(pd.name);
                    info.size = pd.size;
                }
                Err(err) => info.error = Some(format!("Couldn't read the file's details from Pixeldrain: {err}")),
            },
            None => {
                info.error = Some(
                    "That Pixeldrain link isn't to a single file (expected …/u/<id>).".to_string(),
                )
            }
        }
    } else if let Some(last) = url.split(['?', '#']).next().and_then(|p| p.rsplit('/').next()) {
        let name = percent_decode(last);
        if name.to_ascii_lowercase().ends_with(".var") {
            info.filename = Some(name);
        }
    }
    if let Some(name) = &info.filename {
        if !name.to_ascii_lowercase().ends_with(".var") {
            info.error = Some(format!("The link serves {name}, not a .var package."));
        }
    }
    info
}

fn fetch_pixeldrain_info(id: &str) -> Result<PixeldrainInfo, String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(crate::hub::CHROME_USER_AGENT)
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .get(format!("https://pixeldrain.com/api/file/{id}/info"))
        .send()
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    resp.json::<PixeldrainInfo>().map_err(|e| e.to_string())
}

/// Saves `url` as a download source for the `.var` named `filename`
/// (`Creator.Package.3.var`). Returns the stored row; adding the same link
/// twice is a no-op, except that an archive's member and password are
/// updated. `archive_entry` marks the link as a `.zip` holding the `.var` at
/// that path.
#[tauri::command]
pub(crate) fn add_download_link(
    filename: String,
    url: String,
    archive_entry: Option<String>,
    archive_password: Option<String>,
    db: State<'_, Db>,
) -> Result<DownloadLinkRow, String> {
    let (url, host) = normalize_link(&url).ok_or_else(|| "Paste an http(s) link.".to_string())?;
    if host == "mega" {
        // Without its key a MEGA link can't be decrypted, and a whole folder
        // isn't one file — refuse both now.
        if let crate::mega::MegaRef::Folder { .. } = crate::mega::parse(&url)? {
            return Err("Pick the .var inside the MEGA folder — a folder link isn't one file.".to_string());
        }
    }
    let mut name = filename.trim().to_string();
    if !name.to_ascii_lowercase().ends_with(".var") {
        name.push_str(".var");
    }
    let stem = &name[..name.len() - 4];
    if crate::naming::package_version(stem).is_none() {
        return Err(format!(
            "{name} has no version number — use the package's full file name, e.g. Creator.Package.3.var."
        ));
    }
    let mut row = crate::tasks::parse_link_line(&format!("{name} {url}"))
        .ok_or_else(|| format!("{name} isn't a package file name."))?;
    row.archive_entry = archive_entry.map(|e| e.trim().to_string()).filter(|e| !e.is_empty());
    row.archive_password = archive_password.filter(|p| !p.trim().is_empty());
    crate::db::insert_download_links(&db, std::slice::from_ref(&row)).map_err(|e| e.to_string())?;
    if row.archive_entry.is_some() {
        crate::db::set_link_archive(&db, &row).map_err(|e| e.to_string())?;
    }
    Ok(row)
}

/// Stored sources whose file name contains `query` (all when empty), newest
/// first, at most `limit`; with the total that match.
#[tauri::command(async)]
pub(crate) fn search_download_links(
    query: Option<String>,
    limit: Option<u32>,
    db: State<'_, Db>,
) -> Result<(Vec<DownloadLinkRow>, u64), String> {
    crate::db::search_download_links(&db, query.as_deref().unwrap_or("").trim(), limit.unwrap_or(300).min(2000))
        .map_err(|e| e.to_string())
}

// ----------------------------------------------------------------------------
// Scanning pasted text for links
// ----------------------------------------------------------------------------

/// A `.var` some pasted link delivers: directly, inside a MEGA folder or
/// Pixeldrain list, or inside a `.zip`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct FoundVar {
    pub(crate) filename: String,
    /// The link to save as its source (a file link, MEGA's link to the file
    /// inside a folder, or the archive's link).
    pub(crate) url: String,
    pub(crate) host: String,
    pub(crate) size: Option<u64>,
    /// Inside a zip: the path in the archive, and the archive's name.
    pub(crate) archive_entry: Option<String>,
    pub(crate) archive_name: Option<String>,
    /// The zip member is password-protected.
    pub(crate) encrypted: bool,
    /// The pasted link it was found through.
    pub(crate) origin: String,
    /// Inside a MEGA folder: the subfolders the file sits in.
    #[serde(default)]
    pub(crate) folder_path: Option<String>,
    /// A protected archive's password, when one from the post (or the
    /// password box) was verified against it.
    #[serde(default)]
    pub(crate) password: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct LinkProblem {
    pub(crate) link: String,
    pub(crate) error: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct SourceScanResult {
    pub(crate) links: usize,
    pub(crate) found: Vec<FoundVar>,
    pub(crate) problems: Vec<LinkProblem>,
    /// Files that are neither a .var nor an archive (preview images, ...).
    pub(crate) skipped_files: usize,
    pub(crate) was_cancelled: bool,
}

/// Every link worth following in `text`: Pixeldrain, MEGA, MediaFire, and
/// anything pointing straight at a .var or an archive. In order, deduplicated.
pub(crate) fn extract_links(text: &str) -> Vec<String> {
    let re = regex::Regex::new(r#"https?://[^\s"'<>\[\](){}|\\^`]+"#).expect("valid regex");
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for m in re.find_iter(text) {
        let link = m.as_str().trim_end_matches(['.', ',', ';', ':', '?']);
        let lower = link.to_ascii_lowercase();
        let path = lower.split(['?', '#']).next().unwrap_or("");
        let wanted = lower.contains("pixeldrain.")
            || lower.contains("pixeldra.in")
            || crate::mega::is_mega(link)
            || lower.contains("mediafire.com")
            || path.ends_with(".var")
            || crate::archives::archive_kind(path).is_some();
        if wanted && seen.insert(link.to_string()) {
            out.push(link.to_string());
        }
    }
    out
}

#[derive(Deserialize)]
struct PixeldrainListFile {
    id: String,
    name: String,
    size: Option<u64>,
}

#[derive(Deserialize)]
struct PixeldrainList {
    files: Vec<PixeldrainListFile>,
}

fn fetch_pixeldrain_list(id: &str) -> Result<Vec<PixeldrainListFile>, String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(crate::hub::CHROME_USER_AGENT)
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .get(format!("https://pixeldrain.com/api/list/{id}"))
        .send()
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    Ok(resp.json::<PixeldrainList>().map_err(|e| e.to_string())?.files)
}

/// The id in a Pixeldrain list link (`/l/<id>`).
fn pixeldrain_list_id(url: &str) -> Option<&str> {
    let pos = url.find("/l/")?;
    let rest = &url[pos + 3..];
    let end = rest.find(|c: char| !c.is_ascii_alphanumeric()).unwrap_or(rest.len());
    (end > 0).then(|| &rest[..end])
}

/// The file name a URL path ends in (MediaFire: the segment after the id).
fn name_from_url(url: &str) -> Option<String> {
    let path = url.split(['?', '#']).next()?;
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let pick = if url.to_ascii_lowercase().contains("mediafire.com") {
        segments.iter().position(|s| *s == "file").and_then(|i| segments.get(i + 2))
    } else {
        segments.last()
    };
    pick.map(|s| percent_decode(s))
}

/// Passwords written in a post — `Password: x`, `pass - x`, `pw: x`, also
/// through HTML tags — with where they appear, in order.
pub(crate) fn extract_passwords(text: &str) -> Vec<(usize, String)> {
    const NOT_A_PASSWORD: &[&str] = &[
        "is", "for", "the", "to", "of", "in", "on", "and", "required", "protected", "none", "no", "below",
        "above", "here", "same", "needed", "please", "if", "it",
    ];
    let re = regex::Regex::new(
        r#"(?i)\b(?:password|passwort|passwd|pass|pwd|pw)\b(?:\s|<[^>]*>)*(?:is(?:\s|<[^>]*>)+)?[:=：\-–]?(?:\s|<[^>]*>)*["'`“”]?([^\s"'`“”<>]+)"#,
    )
    .expect("valid regex");
    re.captures_iter(text)
        .filter_map(|c| {
            let m = c.get(1)?;
            let pw = m.as_str().trim_end_matches(['.', ',', ';', ')', ']', '!']);
            let lower = pw.to_lowercase();
            (pw.len() >= 2 && !lower.starts_with("http") && !NOT_A_PASSWORD.contains(&lower.as_str()))
                .then(|| (m.start(), pw.to_string()))
        })
        .collect()
}

/// The passwords to try on archives behind the link at `link_pos`: the typed
/// one first, then the post's, nearest to the link first.
fn password_candidates(typed: Option<&str>, found: &[(usize, String)], link_pos: Option<usize>) -> Vec<String> {
    let mut by_distance: Vec<&(usize, String)> = found.iter().collect();
    if let Some(at) = link_pos {
        by_distance.sort_by_key(|(pos, _)| pos.abs_diff(at));
    }
    let mut out: Vec<String> = Vec::new();
    for pw in typed.into_iter().map(str::to_string).chain(by_distance.into_iter().map(|(_, p)| p.clone())) {
        if !pw.trim().is_empty() && !out.contains(&pw) {
            out.push(pw);
        }
    }
    out
}

struct Scan<'a> {
    out: &'a mut SourceScanResult,
    origin: String,
    /// Passwords to try on protected archives behind this link, best first.
    candidates: Vec<String>,
}

impl Scan<'_> {
    /// One file a link delivers: a .var is a find; a .zip is opened and its
    /// .var members are finds; anything else is skipped.
    fn file(&mut self, name: &str, size: Option<u64>, url: &str, host: &str) {
        self.file_in(name, size, url, host, None);
    }

    fn file_in(&mut self, name: &str, size: Option<u64>, url: &str, host: &str, folder_path: Option<&str>) {
        let folder_path = folder_path.filter(|p| !p.is_empty()).map(str::to_string);
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".var") {
            self.out.found.push(FoundVar {
                filename: name.rsplit(['/', '\\']).next().unwrap_or(name).to_string(),
                url: url.to_string(),
                host: host.to_string(),
                size,
                origin: self.origin.clone(),
                folder_path,
                ..Default::default()
            });
            return;
        }
        match crate::archives::archive_kind(&lower) {
            Some("zip") if host == "mediafire" => self.problem(format!(
                "{name}: MediaFire downloads only work in the browser — download and extract it yourself."
            )),
            Some("zip") => match crate::archives::inspect_remote(url, &self.candidates) {
                Ok((entries, password)) => {
                    let before = self.out.found.len();
                    for e in entries.iter().filter(|e| e.name.to_ascii_lowercase().ends_with(".var")) {
                        self.out.found.push(FoundVar {
                            filename: e.name.rsplit(['/', '\\']).next().unwrap_or(&e.name).to_string(),
                            url: url.to_string(),
                            host: host.to_string(),
                            size: Some(e.size),
                            archive_entry: Some(e.name.clone()),
                            archive_name: Some(name.to_string()),
                            encrypted: e.encrypted,
                            origin: self.origin.clone(),
                            folder_path: folder_path.clone(),
                            password: if e.encrypted { password.clone() } else { None },
                        });
                    }
                    if self.out.found.len() == before {
                        self.problem(format!("{name}: no .var files inside"));
                    }
                    self.out.skipped_files += entries.len() - (self.out.found.len() - before);
                }
                Err(e) => self.problem(format!("{name}: {e}")),
            },
            Some(_) => self.problem(format!("{name}: {}", crate::archives::UNSUPPORTED_ARCHIVE)),
            None => self.out.skipped_files += 1,
        }
    }

    fn problem(&mut self, error: String) {
        self.out.problems.push(LinkProblem { link: self.origin.clone(), error });
    }

    fn link(&mut self, link: &str) {
        let Some((url, host)) = normalize_link(link) else {
            return self.problem("not an http(s) link".to_string());
        };
        match host {
            "pixeldrain" => {
                if let Some(id) = pixeldrain_list_id(link) {
                    match fetch_pixeldrain_list(id) {
                        Ok(files) => {
                            for f in files {
                                self.file(&f.name, f.size, &format!("https://pixeldrain.com/api/file/{}?download", f.id), host);
                            }
                        }
                        Err(e) => self.problem(format!("Couldn't read the Pixeldrain list: {e}")),
                    }
                } else if let Some(id) = pixeldrain_id(&url) {
                    match fetch_pixeldrain_info(id) {
                        Ok(info) => self.file(&info.name, info.size, &url, host),
                        Err(e) => self.problem(format!("Couldn't read the Pixeldrain file: {e}")),
                    }
                } else {
                    self.problem("unrecognized Pixeldrain link".to_string());
                }
            }
            "mega" => match crate::mega::parse(&url) {
                Ok(crate::mega::MegaRef::Folder { .. }) => match crate::mega::list_folder(&url, &[".var", ".zip", ".7z", ".rar"]) {
                    Ok(files) => {
                        for f in files {
                            self.file_in(&f.name, f.size, &f.url, host, Some(&f.path));
                        }
                    }
                    Err(e) => self.problem(e),
                },
                Ok(_) => match crate::mega::inspect(&url) {
                    Ok(found) => match found.name {
                        Some(name) => self.file(&name, found.size, &url, host),
                        None => self.problem("MEGA didn't give the file's name".to_string()),
                    },
                    Err(e) => self.problem(e),
                },
                Err(e) => self.problem(e),
            },
            _ => match name_from_url(&url) {
                Some(name) => self.file(&name, None, &url, host),
                None => self.problem("the link doesn't name a file".to_string()),
            },
        }
    }
}

/// Follows every link in `text` (see `extract_links`) as a background task
/// and lists the .var files they deliver.
#[tauri::command]
pub(crate) fn start_scan_source_links_task(
    text: String,
    // The page's password box: tried first on every protected archive.
    password: Option<String>,
    state: State<'_, crate::models::AppState>,
) -> Result<crate::models::TaskHandle, String> {
    let links = extract_links(&text);
    let found_passwords = extract_passwords(&text);
    if links.is_empty() {
        return Err("No Pixeldrain, MEGA, MediaFire or .var / .zip links in that text.".to_string());
    }
    let (task_id, tasks, cancel) = crate::packages::begin_task(&state, "source_scan_starting", "Reading links")?;
    std::thread::spawn(move || {
        let mut result = SourceScanResult { links: links.len(), ..Default::default() };
        for (i, link) in links.iter().enumerate() {
            if cancel.load(std::sync::atomic::Ordering::SeqCst) {
                result.was_cancelled = true;
                break;
            }
            crate::tasks::set_task_progress(
                &tasks,
                task_id,
                "source_scan_link",
                i as f64 / links.len() as f64,
                format!("Reading link {} of {}", i + 1, links.len()),
            );
            let candidates = password_candidates(password.as_deref(), &found_passwords, text.find(link.as_str()));
            Scan { out: &mut result, origin: link.clone(), candidates }.link(link);
        }
        if let Ok(mut guard) = tasks.lock() {
            if let Some(task) = guard.get_mut(&task_id) {
                task.done = true;
                task.progress = 1.0;
                task.phase = "source_scan_complete".to_string();
                task.message = format!("{} .var file(s) found", result.found.len());
                task.source_scan_result = Some(result);
            }
        }
    });
    Ok(crate::models::TaskHandle { id: task_id })
}

/// Whether `password` opens the protected members of the zip at `url` (checked
/// against the member's header through range requests).
#[tauri::command]
pub(crate) async fn check_archive_password(url: String, password: String) -> Result<bool, String> {
    crate::library::on_hub_thread(move || {
        let (entries, found) = crate::archives::inspect_remote(&url, &[password])?;
        Ok(found.is_some() || !entries.iter().any(|e| e.encrypted))
    })
    .await
}

/// Every stored source for the package family of `package_id`.
#[tauri::command(async)]
pub(crate) fn list_download_links(package_id: String, db: State<'_, Db>) -> Result<Vec<DownloadLinkRow>, String> {
    let base = crate::naming::package_base(package_id.trim().trim_end_matches(".var")).to_ascii_lowercase();
    crate::db::find_download_links(&db, &base).map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) fn remove_download_link(filename: String, url: String, db: State<'_, Db>) -> Result<bool, String> {
    crate::db::remove_download_link(&db, &filename, &url).map_err(|e| e.to_string())
}
