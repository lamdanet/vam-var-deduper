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
/// twice is a no-op.
#[tauri::command]
pub(crate) fn add_download_link(
    filename: String,
    url: String,
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
    let row = crate::tasks::parse_link_line(&format!("{name} {url}"))
        .ok_or_else(|| format!("{name} isn't a package file name."))?;
    crate::db::insert_download_links(&db, std::slice::from_ref(&row)).map_err(|e| e.to_string())?;
    Ok(row)
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
