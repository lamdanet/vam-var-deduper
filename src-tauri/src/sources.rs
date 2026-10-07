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
    let host = url_host(url);
    if is_f95_masked(url) {
        return Some((url.to_string(), "f95"));
    }
    if host == "pixeldrain.com" || host == "pixeldrain.net" || host == "pixeldra.in" || host.ends_with(".pixeldrain.com") {
        // Only a plain share page or file link is rewritten: a link with more
        // after the id (`/api/file/<id>/info/zip/<path>`, one file inside a
        // zip) is a different file and stays as written.
        return Some(match pixeldrain_plain_id(url) {
            Some(id) => (format!("https://pixeldrain.com/api/file/{id}?download"), "pixeldrain"),
            None => (url.to_string(), "pixeldrain"),
        });
    }
    if host == "mediafire.com" || host.ends_with(".mediafire.com") {
        return Some((url.to_string(), "mediafire"));
    }
    if crate::mega::is_mega(url) {
        return Some((url.to_string(), "mega"));
    }
    Some((url.to_string(), "other"))
}

/// The host of an http(s) URL, lowercased, without a leading `www.`.
pub(crate) fn url_host(url: &str) -> String {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.rsplit('@').next().unwrap_or(host).split(':').next().unwrap_or("");
    host.to_ascii_lowercase().trim_start_matches("www.").to_string()
}

/// F95zone hides external links behind `f95zone.to/masked/<host>/...`.
pub(crate) fn is_f95_masked(url: &str) -> bool {
    url_host(url) == "f95zone.to" && url.contains("/masked/")
}

/// Undoes the HTML escaping a copied page carries (`&amp;`, `&quot;`, ...), so
/// links and passwords read as written.
pub(crate) fn decode_html_entities(text: &str) -> String {
    let re = regex::Regex::new(r"&(#x[0-9a-fA-F]+|#[0-9]+|amp|quot|apos|lt|gt|nbsp);").expect("valid regex");
    re.replace_all(text, |c: &regex::Captures| {
        let e = &c[1];
        let ch = match e {
            "amp" => Some('&'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "nbsp" => Some(' '),
            _ if e.starts_with("#x") => u32::from_str_radix(&e[2..], 16).ok().and_then(char::from_u32),
            _ => e[1..].parse::<u32>().ok().and_then(char::from_u32),
        };
        ch.map(String::from).unwrap_or_else(|| c[0].to_string())
    })
    .into_owned()
}

/// The file id of a Pixeldrain link that is just the file: `/u/<id>` or
/// `/api/file/<id>` with nothing but a query after it.
fn pixeldrain_plain_id(url: &str) -> Option<&str> {
    let id = pixeldrain_id(url)?;
    let after = &url[url.find(id)? + id.len()..];
    (after.is_empty() || after.starts_with(['?', '#']) || after == "/").then_some(id)
}

/// What a link fetches, however it is written: two links with the same key
/// download the same file. Pixeldrain by file id (plus any path inside a zip),
/// MediaFire by quick key, MEGA by file (or folder + file) id; anything else
/// by its address without the scheme and with the host lowercased.
pub(crate) fn link_key(url: &str) -> String {
    let (url, host) = normalize_link(url).unwrap_or_else(|| (url.trim().to_string(), "other"));
    match host {
        "pixeldrain" => match (pixeldrain_plain_id(&url), url.find("/api/file/")) {
            (Some(id), _) => format!("pixeldrain:{id}"),
            (None, Some(pos)) => format!("pixeldrain:{}", percent_decode(&url[pos + "/api/file/".len()..])),
            _ => format!("pixeldrain:{url}"),
        },
        "mediafire" => match (mediafire_quick_key(&url), mediafire_folder_key(&url)) {
            (Some(qk), _) => format!("mediafire:{qk}"),
            (None, Some(fk)) => format!("mediafire-folder:{fk}"),
            _ => format!("mediafire:{url}"),
        },
        "mega" => match crate::mega::parse(&url) {
            Ok(crate::mega::MegaRef::File { handle, .. }) => format!("mega:{handle}"),
            Ok(crate::mega::MegaRef::FolderFile { handle, node, .. }) => format!("mega:{handle}/{node}"),
            Ok(crate::mega::MegaRef::Folder { handle, sub, .. }) => format!("mega-folder:{handle}/{}", sub.unwrap_or_default()),
            Err(_) => format!("mega:{url}"),
        },
        _ => {
            let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(&url);
            let (host_part, path) = rest.split_once('/').unwrap_or((rest, ""));
            format!("{}/{}", host_part.to_ascii_lowercase().trim_start_matches("www."), path.trim_end_matches('/'))
        }
    }
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
    } else if host == "pixeldrain" && pixeldrain_plain_id(&url).is_some() {
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

#[derive(Debug, Clone, Serialize)]
pub(crate) struct AddedLink {
    pub(crate) row: DownloadLinkRow,
    /// False when the file already had this link (however it was written).
    pub(crate) added: bool,
}

/// Saves `url` as a download source for the `.var` named `filename`
/// (`Creator.Package.3.var`). A link the file already has — however it is
/// written (see `link_key`) — isn't added again; an archive link's member and
/// password are still updated. `archive_entry` marks the link as a `.zip`
/// holding the `.var` at that path.
#[tauri::command]
pub(crate) fn add_download_link(
    filename: String,
    url: String,
    archive_entry: Option<String>,
    archive_password: Option<String>,
    db: State<'_, Db>,
) -> Result<AddedLink, String> {
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
    let added = crate::db::insert_download_links(&db, std::slice::from_ref(&row)).map_err(|e| e.to_string())? > 0;
    if row.archive_entry.is_some() {
        crate::db::set_link_archive(&db, &row).map_err(|e| e.to_string())?;
    }
    Ok(AddedLink { row, added })
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
    /// What the post calls the link it came from — the text before it on
    /// its line, e.g. "Collection Update 2025-05-13 (Pack 1)".
    #[serde(default)]
    pub(crate) label: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct LinkProblem {
    pub(crate) link: String,
    pub(crate) error: String,
    /// `"masked"` (F95 hid the link), `"archive"` (a 7z/RAR), `"empty"` (no
    /// .var inside), or `"unreachable"` (anything else).
    #[serde(default)]
    pub(crate) kind: String,
    /// The file it is about, when known.
    #[serde(default)]
    pub(crate) name: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct SourceScanResult {
    pub(crate) links: usize,
    pub(crate) found: Vec<FoundVar>,
    pub(crate) problems: Vec<LinkProblem>,
    /// Files that are neither a .var nor an archive (preview images, ...).
    pub(crate) skipped_files: usize,
    pub(crate) was_cancelled: bool,
    /// Links on hosts the app can't download from, by host, e.g.
    /// `("vikingfile.com", 35)`.
    #[serde(default)]
    pub(crate) ignored_hosts: Vec<(String, usize)>,
}

/// The host an F95 masked link hides (`f95zone.to/masked/<host>/...`).
fn masked_host(link: &str) -> String {
    link.split("/masked/").nth(1).and_then(|r| r.split('/').next()).unwrap_or("").to_ascii_lowercase()
}

fn masked_host_supported(link: &str) -> bool {
    matches!(
        normalize_link(&format!("https://{}/", masked_host(link))),
        Some((_, "pixeldrain" | "mega" | "mediafire"))
    )
}

/// File hosts whose links open a download page (often with a captcha or a
/// wait) rather than the file — even when the URL ends in the file's name.
fn is_page_host(host: &str) -> bool {
    const PAGE_HOSTS: &[&str] = &[
        "datanodes.to", "gofile.io", "vikingfile.com", "1fichier.com", "krakenfiles.com", "workupload.com",
        "uploadhaven.com", "buzzheavier.com", "sendspace.com", "uploadrar.com", "rapidgator.net", "nitroflare.com",
        "katfile.com", "ddownload.com", "send.cm", "filefactory.com", "uptobox.com", "racaty.io", "anonfiles.com",
        "zippyshare.com", "drive.google.com", "dropbox.com", "onedrive.live.com", "1drv.ms",
    ];
    PAGE_HOSTS.iter().any(|h| host == *h || host.ends_with(&format!(".{h}")))
}

/// Download hosts in the post the app doesn't support, with how many links
/// each has. Pages of the forum itself and image links aren't counted.
pub(crate) fn unsupported_hosts(text: &str) -> Vec<(String, usize)> {
    let re = regex::Regex::new(r#"href\s*=\s*["']?(https?://[^"'\s<>]+)"#).expect("valid regex");
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for c in re.captures_iter(text) {
        let link = &c[1];
        let host = if is_f95_masked(link) {
            masked_host(link)
        } else {
            url_host(link)
        };
        let supported = matches!(normalize_link(&format!("https://{host}/")), Some((_, "pixeldrain" | "mega" | "mediafire")));
        let path = link.to_ascii_lowercase();
        let direct = path.split(['?', '#']).next().is_some_and(|p| p.ends_with(".var") || crate::archives::archive_kind(p).is_some());
        if host.is_empty() || supported || (direct && !is_page_host(&host)) || host == "f95zone.to" || host.ends_with("duckduckgo.com") {
            continue;
        }
        *counts.entry(host).or_default() += 1;
    }
    counts.into_iter().collect()
}

/// The text before `pos` on its line, without HTML — how a post labels the
/// links on that line ("Pack 2: MEGA - MEDIAFIRE - ...").
pub(crate) fn link_label(text: &str, pos: usize) -> Option<String> {
    let start = text[..pos].rfind(['\n', '\r']).map(|i| i + 1).unwrap_or(0);
    let mut before = &text[start..pos];
    // The link usually sits inside a tag (`<a href="`): drop that open tag.
    if let Some(lt) = before.rfind('<') {
        if before.rfind('>').is_none_or(|gt| gt < lt) {
            before = &before[..lt];
        }
    }
    let tags = regex::Regex::new(r"<[^>]*>").expect("valid regex");
    let plain = tags.replace_all(before, " ");
    // Up to the first link marker on the line: the label, not "MEGA - ".
    let label = plain.split(" - ").next().unwrap_or("").split("http").next().unwrap_or("");
    let label = label.split_whitespace().collect::<Vec<_>>().join(" ");
    let label = label.trim_end_matches([':', '-', '–', ' ']).trim();
    (label.len() >= 3 && label.len() <= 120).then(|| label.to_string())
}

/// Every link worth following in `text`: Pixeldrain, MEGA, MediaFire, and
/// anything pointing straight at a .var or an archive. In order, deduplicated.
pub(crate) fn extract_links(text: &str) -> Vec<String> {
    let re = regex::Regex::new(r#"https?://[^\s"'<>\[\](){}|\\^`]+"#).expect("valid regex");
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    const IMAGE_EXTS: &[&str] = &[".ico", ".png", ".jpg", ".jpeg", ".gif", ".webp", ".svg", ".bmp"];
    for m in re.find_iter(text) {
        let link = m.as_str().trim_end_matches(['.', ',', ';', ':', '?', '&']);
        let lower = link.to_ascii_lowercase();
        let path = lower.split(['?', '#']).next().unwrap_or("");
        // Favicons, thumbnails and image proxies are page decoration.
        if IMAGE_EXTS.iter().any(|e| path.ends_with(e)) {
            continue;
        }
        // A file host's page, even one whose URL ends in `.zip`, isn't the file.
        if is_page_host(&url_host(link)) {
            continue;
        }
        // A masked link to a host the app can't use isn't worth unmasking.
        if is_f95_masked(link) && !masked_host_supported(link) {
            continue;
        }
        let wanted = matches!(normalize_link(link), Some((_, "pixeldrain" | "mega" | "mediafire" | "f95")))
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
    label: Option<String>,
    /// Set once F95 refuses to unmask (it wants a log-in): the remaining
    /// masked links aren't asked about again.
    f95_refused: &'a std::sync::atomic::AtomicBool,
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
                label: self.label.clone(),
                ..Default::default()
            });
            return;
        }
        match crate::archives::archive_kind(&lower) {
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
                            label: self.label.clone(),
                            password: if e.encrypted { password.clone() } else { None },
                        });
                    }
                    if self.out.found.len() == before {
                        self.problem_of("empty", Some(name), "no .var files inside".to_string());
                    }
                    self.out.skipped_files += entries.len() - (self.out.found.len() - before);
                }
                Err(e) => self.problem_of("unreachable", Some(name), e),
            },
            Some(_) => self.problem_of("archive", Some(name), crate::archives::UNSUPPORTED_ARCHIVE.to_string()),
            None => self.out.skipped_files += 1,
        }
    }

    fn problem(&mut self, error: String) {
        self.problem_of("unreachable", None, error);
    }

    fn problem_of(&mut self, kind: &str, name: Option<&str>, error: String) {
        self.out.problems.push(LinkProblem {
            link: self.origin.clone(),
            error,
            kind: kind.to_string(),
            name: name.map(str::to_string),
        });
    }

    fn link(&mut self, link: &str) {
        let Some((url, host)) = normalize_link(link) else {
            return self.problem("not an http(s) link".to_string());
        };
        match host {
            "f95" => {
                let hidden_host = Some(masked_host(&url)).filter(|h| !h.is_empty());
                let refused = self.f95_refused.load(std::sync::atomic::Ordering::SeqCst);
                let unmasked = if refused { Err(String::new()) } else { unmask_f95(&url) };
                match unmasked {
                    Ok(real) if !is_f95_masked(&real) => self.link(&real),
                    _ => {
                        self.f95_refused.store(true, std::sync::atomic::Ordering::SeqCst);
                        self.problem_of(
                            "masked",
                            hidden_host.as_deref(),
                            "F95 only reveals masked links to logged-in members.".to_string(),
                        );
                    }
                }
            }
            "mediafire" => {
                if let Some(key) = mediafire_folder_key(&url) {
                    let mut files = Vec::new();
                    match mediafire_list_folder(key, "", 0, &mut files) {
                        Ok(()) if files.is_empty() => {
                            self.problem_of("empty", None, "the MediaFire folder is empty".to_string())
                        }
                        Ok(()) => {
                            for f in files {
                                self.file_in(&f.name, f.size, &f.url, host, Some(&f.path));
                            }
                        }
                        Err(e) => self.problem(format!("Couldn't read the MediaFire folder: {e}")),
                    }
                } else if let Some(key) = mediafire_quick_key(&url) {
                    match mediafire_file_info(key) {
                        Ok((name, size)) => self.file(&name, size, &url, host),
                        Err(e) => self.problem(format!("Couldn't read the MediaFire file: {e}")),
                    }
                } else {
                    self.problem("unrecognized MediaFire link".to_string());
                }
            }
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
                } else if let Some(id) = pixeldrain_plain_id(&url) {
                    match fetch_pixeldrain_info(id) {
                        Ok(info) => self.file(&info.name, info.size, &url, host),
                        Err(e) => self.problem(format!("Couldn't read the Pixeldrain file: {e}")),
                    }
                } else if let Some(name) = name_from_url(&url) {
                    // One file inside a zip (`/api/file/<id>/info/zip/<path>`).
                    self.file(&name, None, &url, host);
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

// ----------------------------------------------------------------------------
// MediaFire and F95
// ----------------------------------------------------------------------------

fn browser_client(timeout_secs: u64) -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .user_agent(crate::hub::CHROME_USER_AGENT)
        .timeout(Duration::from_secs(timeout_secs))
        .build()
        .map_err(|e| e.to_string())
}

fn path_segment_after<'a>(url: &'a str, marker: &str) -> Option<&'a str> {
    let pos = url.find(marker)?;
    let rest = &url[pos + marker.len()..];
    let end = rest.find(['/', '?', '#', '&']).unwrap_or(rest.len());
    (end > 0).then(|| &rest[..end])
}

fn mediafire_folder_key(url: &str) -> Option<&str> {
    path_segment_after(url, "/folder/")
}

/// A MediaFire file's quick key: `/file/<key>`, `/view/<key>`, `/download/<key>`
/// or the old `mediafire.com/?<key>`.
fn mediafire_quick_key(url: &str) -> Option<&str> {
    ["/file/", "/view/", "/download/", "mediafire.com/?"].iter().find_map(|m| path_segment_after(url, m))
}

pub(crate) fn is_mediafire_page(url: &str) -> bool {
    let host = url_host(url);
    (host == "mediafire.com" || host.ends_with(".mediafire.com"))
        && !host.starts_with("download")
        && mediafire_quick_key(url).is_some()
}

fn mediafire_api(path: &str) -> Result<serde_json::Value, String> {
    let value: serde_json::Value = browser_client(20)?
        .get(format!("https://www.mediafire.com/api/1.5/{path}&response_format=json"))
        .send()
        .map_err(|e| e.to_string())?
        .json()
        .map_err(|e| format!("unexpected answer: {e}"))?;
    let response = value.get("response").cloned().unwrap_or_default();
    if response.get("result").and_then(|r| r.as_str()) != Some("Success") {
        let msg = response.get("message").and_then(|m| m.as_str()).unwrap_or("not found");
        return Err(msg.to_string());
    }
    Ok(response)
}

fn mediafire_file_info(key: &str) -> Result<(String, Option<u64>), String> {
    let r = mediafire_api(&format!("file/get_info.php?quick_key={key}"))?;
    let info = r.get("file_info").ok_or("no file info")?;
    let name = info.get("filename").and_then(|v| v.as_str()).ok_or("no file name")?.to_string();
    let size = info.get("size").and_then(|v| v.as_str()).and_then(|s| s.parse().ok());
    Ok((name, size))
}

struct FolderFile {
    name: String,
    size: Option<u64>,
    url: String,
    path: String,
}

/// Every file in a public MediaFire folder and its subfolders (a few levels).
fn mediafire_list_folder(key: &str, path: &str, depth: u32, out: &mut Vec<FolderFile>) -> Result<(), String> {
    for kind in ["files", "folders"] {
        for chunk in 1..=50 {
            let r = mediafire_api(&format!(
                "folder/get_content.php?folder_key={key}&content_type={kind}&chunk={chunk}"
            ))?;
            let content = r.get("folder_content").cloned().unwrap_or_default();
            for item in content.get(kind).and_then(|v| v.as_array()).cloned().unwrap_or_default() {
                if kind == "files" {
                    let (Some(name), Some(qk)) = (
                        item.get("filename").and_then(|v| v.as_str()),
                        item.get("quickkey").and_then(|v| v.as_str()),
                    ) else {
                        continue;
                    };
                    out.push(FolderFile {
                        name: name.to_string(),
                        size: item.get("size").and_then(|v| v.as_str()).and_then(|s| s.parse().ok()),
                        url: format!("https://www.mediafire.com/file/{qk}/file"),
                        path: path.to_string(),
                    });
                } else if depth < 4 {
                    let (Some(sub), Some(name)) = (
                        item.get("folderkey").and_then(|v| v.as_str()),
                        item.get("name").and_then(|v| v.as_str()),
                    ) else {
                        continue;
                    };
                    let sub_path = if path.is_empty() { name.to_string() } else { format!("{path}/{name}") };
                    mediafire_list_folder(sub, &sub_path, depth + 1, out)?;
                }
            }
            if content.get("more_chunks").and_then(|v| v.as_str()) != Some("yes") {
                break;
            }
        }
    }
    Ok(())
}

/// The direct (`download####.mediafire.com`) address behind a MediaFire file
/// page. It expires, so it is looked up each time a file is fetched.
pub(crate) fn mediafire_direct(page_url: &str) -> Result<String, String> {
    let key = mediafire_quick_key(page_url).ok_or("not a MediaFire file link")?;
    let html = browser_client(30)?
        .get(format!("https://www.mediafire.com/file/{key}/file"))
        .send()
        .map_err(|e| format!("Couldn't reach MediaFire: {e}"))?
        .text()
        .map_err(|e| e.to_string())?;
    let re = regex::Regex::new(r#"https?://download\d+\.mediafire\.com/[^"'\s<>]+"#).expect("valid regex");
    re.find(&html).map(|m| m.as_str().to_string()).ok_or_else(|| {
        "MediaFire didn't give a download address (removed, or it wants a captcha) — open it in your browser".to_string()
    })
}

/// The real address behind an F95 masked link, the way F95's own page asks
/// for it. F95 may refuse (log-in or captcha); the caller then asks the user.
fn unmask_f95(url: &str) -> Result<String, String> {
    let resp = browser_client(20)?
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header("X-Requested-With", "XMLHttpRequest")
        .body("xhr=1&download=1")
        .send()
        .map_err(|e| e.to_string())?;
    let value: serde_json::Value = resp.json().map_err(|_| "F95 didn't unmask the link".to_string())?;
    match (value.get("status").and_then(|v| v.as_str()), value.get("msg").and_then(|v| v.as_str())) {
        (Some("ok"), Some(link)) if link.starts_with("http") => Ok(link.to_string()),
        (_, Some(msg)) => Err(msg.to_string()),
        _ => Err("F95 didn't unmask the link".to_string()),
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
    let text = decode_html_entities(&text);
    let links = extract_links(&text);
    let ignored_hosts = unsupported_hosts(&text);
    let found_passwords = extract_passwords(&text);
    if links.is_empty() {
        return Err("No Pixeldrain, MEGA, MediaFire, F95 or .var / .zip links in that text.".to_string());
    }
    let (task_id, tasks, cancel) = crate::packages::begin_task(&state, "source_scan_starting", "Reading links")?;
    std::thread::spawn(move || {
        use rayon::prelude::*;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        let mut result = SourceScanResult { links: links.len(), ignored_hosts, ..Default::default() };
        let f95_refused = AtomicBool::new(false);
        let read = AtomicUsize::new(0);
        crate::tasks::set_task_progress(&tasks, task_id, "source_scan_link", 0.0, format!("Reading {} links", links.len()));
        // Hosts answer slowly but independently: read six links at a time.
        // Results are merged in the post's order.
        let read_one = |link: &String| -> SourceScanResult {
            let mut part = SourceScanResult::default();
            if cancel.load(Ordering::SeqCst) {
                part.was_cancelled = true;
                return part;
            }
            let candidates = password_candidates(password.as_deref(), &found_passwords, text.find(link.as_str()));
            let label = text.find(link.as_str()).and_then(|pos| link_label(&text, pos));
            Scan { out: &mut part, origin: link.clone(), candidates, label, f95_refused: &f95_refused }.link(link);
            let n = read.fetch_add(1, Ordering::SeqCst) + 1;
            crate::tasks::set_task_progress(
                &tasks,
                task_id,
                "source_scan_link",
                n as f64 / links.len() as f64,
                format!("Read {n} of {} links", links.len()),
            );
            part
        };
        let parts: Vec<SourceScanResult> = match rayon::ThreadPoolBuilder::new().num_threads(6).build() {
            Ok(pool) => pool.install(|| links.par_iter().map(read_one).collect()),
            Err(_) => links.iter().map(read_one).collect(),
        };
        for part in parts {
            result.found.extend(part.found);
            result.problems.extend(part.problems);
            result.skipped_files += part.skipped_files;
            result.was_cancelled |= part.was_cancelled;
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
