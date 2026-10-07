//! VaM Hub integration for the Download VARs page.
//!
//! Hub-only port of what Sharp VaM Tools does: resolve a VAR package id to a Hub
//! download URL via the public `findPackages` API, then stream the file to disk
//! with the headers VaM itself sends. No auth, no external binaries, no mirrors.
//!
//! Must be called from a plain OS thread (the task layer uses `std::thread`),
//! never from a tokio worker — `reqwest::blocking` spins its own runtime.

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, Result};

const API_URL: &str = "https://hub.virtamate.com/citizenx/api.php";
const VAM_USER_AGENT: &str = "UnityPlayer/2018.1.9f2 (UnityWebRequest/1.0, libcurl/7.51.0-DEV)";
const X_UNITY_VERSION: &str = "2018.1.9f2";
pub(crate) const CHROME_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
     (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

/// Timeout for the small Hub API (findPackages) requests.
const API_TIMEOUT_SECS: u64 = 60;
/// Hard cap on a whole download (generous so genuinely large downloads on slow
/// links aren't killed).
const TOTAL_TIMEOUT_SECS: u64 = 3600;
/// Idle timeout: if no bytes arrive for this long mid-download, the transfer is
/// treated as stalled and fails fast (instead of hanging until the total cap),
/// so the caller can clear the partial file and let the user retry.
pub(crate) const IDLE_TIMEOUT_SECS: u64 = 60;

/// A resolved download target for one requested package id.
#[derive(Debug, Clone)]
pub(crate) struct HubResolution {
    pub(crate) filename: String,
    pub(crate) download_url: String,
    pub(crate) file_size: Option<u64>,
}

/// Builds the blocking HTTP client for the Hub API calls (findPackages),
/// carrying the headers VaM itself sends.
pub(crate) fn hub_client() -> Result<reqwest::blocking::Client> {
    use reqwest::header::{HeaderMap, HeaderValue, ACCEPT_ENCODING, COOKIE, USER_AGENT};
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static(VAM_USER_AGENT));
    headers.insert("X-Unity-Version", HeaderValue::from_static(X_UNITY_VERSION));
    headers.insert(ACCEPT_ENCODING, HeaderValue::from_static("identity"));
    // VaM's hub-consent cookie. Set on the client (not per-request) so it also
    // rides along when a hub download URL 302-redirects to the CDN host.
    headers.insert(COOKIE, HeaderValue::from_static("vamhubconsent=1"));
    reqwest::blocking::Client::builder()
        .default_headers(headers)
        .timeout(Duration::from_secs(API_TIMEOUT_SECS))
        .build()
        .map_err(|e| anyhow!("failed to build HTTP client: {e}"))
}

/// Async client used for downloads. Pixeldrain gets a browser User-Agent (it
/// rejects non-browser agents); the Hub/CDN gets the VaM-mimic headers + cookie.
fn download_client(is_pixeldrain: bool) -> Result<reqwest::Client> {
    use reqwest::header::{HeaderMap, HeaderValue, ACCEPT_ENCODING, COOKIE, USER_AGENT};
    let mut headers = HeaderMap::new();
    if is_pixeldrain {
        headers.insert(USER_AGENT, HeaderValue::from_static(CHROME_USER_AGENT));
    } else {
        headers.insert(USER_AGENT, HeaderValue::from_static(VAM_USER_AGENT));
        headers.insert("X-Unity-Version", HeaderValue::from_static(X_UNITY_VERSION));
        headers.insert(ACCEPT_ENCODING, HeaderValue::from_static("identity"));
        headers.insert(COOKIE, HeaderValue::from_static("vamhubconsent=1"));
    }
    reqwest::Client::builder()
        .default_headers(headers)
        .timeout(Duration::from_secs(TOTAL_TIMEOUT_SECS))
        .build()
        .map_err(|e| anyhow!("failed to build HTTP client: {e}"))
}

/// True when the id already carries a trailing version (".3") or ".latest".
fn has_version_suffix(id: &str) -> bool {
    crate::naming::has_package_version(id)
}

/// Strips a trailing version / `.latest` so the package family can be re-queried.
fn strip_version(id: &str) -> String {
    crate::naming::package_base(id).to_string()
}

/// Family base, lowercased — used to confirm the hub returned the right package.
fn base_lc(id: &str) -> String {
    crate::naming::package_base(id).to_ascii_lowercase()
}

fn parse_size(v: &serde_json::Value) -> Option<u64> {
    match v {
        serde_json::Value::String(s) => s.trim().parse::<u64>().ok(),
        serde_json::Value::Number(n) => n.as_u64(),
        _ => None,
    }
}

/// Hub-link sanity: `hub.virtamate.com` download links must carry `file=<digits>`;
/// other hosts (e.g. cdn77 attachments) pass through.
fn is_valid_hub_link(url: &str) -> bool {
    if url.starts_with("https://hub.virtamate.com") {
        if let Some(idx) = url.find("file=") {
            return url[idx + 5..]
                .chars()
                .next()
                .map(|c| c.is_ascii_digit())
                .unwrap_or(false);
        }
        return false;
    }
    !url.is_empty() && url != "null"
}

/// POSTs `findPackages` for a comma-joined batch; returns the raw `packages` map.
fn find_packages(client: &reqwest::blocking::Client, joined: &str) -> Result<serde_json::Value> {
    // Byte-match VaM's request body: compact JSON but with a space after each
    // field-separating comma. Package ids never contain `"`/`\`, so direct
    // interpolation is safe. (The Hub sits behind Cloudflare, which can filter
    // requests that don't look like VaM's.)
    let body = format!(
        "{{\"source\":\"VaM\", \"action\":\"findPackages\", \"packages\":\"{joined}\"}}"
    );
    let resp = client
        .post(API_URL)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(reqwest::header::ACCEPT, "application/json")
        .body(body)
        .send()
        .map_err(|e| anyhow!("hub request failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(anyhow!("hub returned HTTP {}", resp.status()));
    }
    let text = resp
        .text()
        .map_err(|e| anyhow!("reading hub response failed: {e}"))?;
    let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
        anyhow!(
            "hub response was not JSON ({e}); starts with: {}",
            text.chars().take(120).collect::<String>()
        )
    })?;
    Ok(json
        .get("packages")
        .cloned()
        .unwrap_or(serde_json::Value::Null))
}

/// Reads a resolution out of a `packages` map for the given query key, validating
/// that the returned filename matches the requested family and the link is usable.
fn extract(packages: &serde_json::Value, query_key: &str, family_lc: &str) -> Option<HubResolution> {
    let entry = packages.get(query_key)?;
    let url = entry.get("downloadUrl").and_then(|v| v.as_str()).unwrap_or("");
    let filename = entry.get("filename").and_then(|v| v.as_str()).unwrap_or("");
    if url.is_empty() || url == "null" || filename.is_empty() {
        return None;
    }
    // The hub can return a different/older version; confirm the family matches.
    if !filename.to_ascii_lowercase().starts_with(&format!("{family_lc}.")) {
        return None;
    }
    if !is_valid_hub_link(url) {
        return None;
    }
    Some(HubResolution {
        filename: filename.to_string(),
        download_url: url.to_string(),
        file_size: entry.get("file_size").and_then(parse_size),
    })
}

/// Resolves a set of package ids to Hub download targets. Versionless/`.latest`
/// ids are queried with a `.latest` suffix (the hub resolves it to a concrete
/// version); unresolved ids retry against the version-stripped family base.
/// Returns the resolved targets plus the first request error encountered (if
/// any) so the caller can distinguish "genuinely not on the Hub" from "couldn't
/// reach the Hub".
pub(crate) fn resolve_download_urls(
    client: &reqwest::blocking::Client,
    ids: &[String],
) -> (HashMap<String, HubResolution>, Option<String>) {
    let mut out: HashMap<String, HubResolution> = HashMap::new();
    let mut first_error: Option<String> = None;

    // (query_key we send, original id it stands for). Skip ids containing ',' —
    // the API uses comma as the batch separator, so those can't be batched.
    let mut query_for: Vec<(String, String)> = Vec::new();
    for id in ids {
        let id = id.trim();
        if id.is_empty() || id.contains(',') {
            continue;
        }
        let q = if has_version_suffix(id) {
            id.to_string()
        } else {
            format!("{id}.latest")
        };
        query_for.push((q, id.to_string()));
    }

    // First pass: batched findPackages.
    for chunk in query_for.chunks(50) {
        let joined = chunk
            .iter()
            .map(|(q, _)| q.as_str())
            .collect::<Vec<_>>()
            .join(",");
        match find_packages(client, &joined) {
            Ok(packages) => {
                for (q, id) in chunk {
                    if let Some(res) = extract(&packages, q, &base_lc(id)) {
                        out.insert(id.clone(), res);
                    }
                }
            }
            Err(e) => {
                if first_error.is_none() {
                    first_error = Some(e.to_string());
                }
            }
        }
    }

    // Second pass: retry the unresolved ones against the version-stripped base
    // (the hub sometimes only answers the bare family name).
    for (q, id) in &query_for {
        if out.contains_key(id) {
            continue;
        }
        let base = strip_version(q);
        if base == *q {
            continue;
        }
        match find_packages(client, &base) {
            Ok(packages) => {
                if let Some(res) = extract(&packages, &base, &base_lc(id)) {
                    out.insert(id.clone(), res);
                }
            }
            Err(e) => {
                if first_error.is_none() {
                    first_error = Some(e.to_string());
                }
            }
        }
    }

    (out, first_error)
}

/// Streams `url` to `dest_path`, invoking `progress(downloaded, total)` as bytes
/// arrive and polling `cancel` ~once a second. Returns `Ok(true)` when the file
/// completed, `Ok(false)` when cancelled mid-stream, and `Err` (with the partial
/// left for the caller to clean up) on failure — including a stall, i.e. no
/// bytes for `IDLE_TIMEOUT_SECS`.
///
/// Uses reqwest's async streaming so each chunk can be wrapped in a timeout
/// (the blocking client has no per-read idle timeout). Runs its own
/// current-thread runtime; safe because the caller is a plain `std::thread`.
pub(crate) fn download_to_file(
    url: &str,
    dest_path: &Path,
    cancel: &std::sync::atomic::AtomicBool,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<bool> {
    // A MEGA link is resolved first (blocking API calls, so before the async
    // runtime starts) to a temporary address for the encrypted bytes, which
    // are decrypted as they stream in.
    // A MediaFire file page is resolved to its (expiring) direct address.
    let resolved;
    let url = if crate::sources::is_mediafire_page(url) {
        resolved = crate::sources::mediafire_direct(url).map_err(|e| anyhow!(e))?;
        resolved.as_str()
    } else {
        url
    };
    let mega = if crate::mega::is_mega(url) {
        Some(crate::mega::open_download(url).map_err(|e| anyhow!(e))?)
    } else {
        None
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| anyhow!("failed to start download runtime: {e}"))?;
    match mega {
        // MEGA limits each connection's speed, so a sizeable file is fetched as
        // several ranges at once, as MEGA's own apps do.
        Some(m) if m.size.is_some_and(|s| s >= MEGA_PARALLEL_MIN) => {
            rt.block_on(download_mega_parallel(&m, dest_path, cancel, progress))
        }
        Some(m) => rt.block_on(download_async(&m.address, Some(m.key.decryptor()), dest_path, cancel, progress)),
        None => rt.block_on(download_async(url, None, dest_path, cancel, progress)),
    }
}

#[cfg(test)]
pub(crate) fn download_mega_for_test(m: &crate::mega::MegaDownload, dest: &Path) -> Result<bool> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let cancel = std::sync::atomic::AtomicBool::new(false);
    rt.block_on(download_mega_parallel(m, dest, &cancel, &mut |_, _| {}))
}

/// Files below this download over one connection.
const MEGA_PARALLEL_MIN: u64 = 4 * 1024 * 1024;
/// Parallel connections for one MEGA file.
const MEGA_CONNECTIONS: u64 = 6;

/// Writes `buf` at `offset` without moving a shared cursor, so several ranges
/// can write into one file.
fn write_at(file: &std::fs::File, mut buf: &[u8], mut offset: u64) -> std::io::Result<()> {
    while !buf.is_empty() {
        #[cfg(windows)]
        let n = std::os::windows::fs::FileExt::seek_write(file, buf, offset)?;
        #[cfg(unix)]
        let n = std::os::unix::fs::FileExt::write_at(file, buf, offset)?;
        if n == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::WriteZero, "wrote nothing"));
        }
        buf = &buf[n..];
        offset += n as u64;
    }
    Ok(())
}

/// A MEGA file as `MEGA_CONNECTIONS` concurrent ranges (`<address>/<a>-<b>`,
/// inclusive), each decrypted from its own offset and written in place.
/// Progress and cancel are polled ~twice a second; a range that gets no bytes
/// for `IDLE_TIMEOUT_SECS` fails the download.
async fn download_mega_parallel(
    m: &crate::mega::MegaDownload,
    dest_path: &Path,
    cancel: &std::sync::atomic::AtomicBool,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<bool> {
    use futures_util::future::{select, try_join_all, Either};
    use futures_util::StreamExt;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    let size = m.size.unwrap_or(0);
    let client = download_client(false)?;
    let file = std::fs::File::create(dest_path)
        .map_err(|e| anyhow!("cannot create {}: {e}", dest_path.display()))?;
    file.set_len(size).map_err(|e| anyhow!("cannot size {}: {e}", dest_path.display()))?;
    let done = AtomicU64::new(0);
    let stop = AtomicBool::new(false);
    // Range starts on 16-byte boundaries (not required by CTR, but tidy).
    let part = ((size / MEGA_CONNECTIONS) / 16 + 1) * 16;
    let ranges: Vec<(u64, u64)> = (0..MEGA_CONNECTIONS)
        .map(|i| (i * part, ((i + 1) * part).min(size)))
        .filter(|(a, b)| a < b)
        .collect();

    let fetch = |start: u64, end: u64| {
        let (client, file, done, stop) = (&client, &file, &done, &stop);
        async move {
            let resp = client
                .get(format!("{}/{}-{}", m.address, start, end - 1))
                .send()
                .await
                .map_err(|e| anyhow!("download request failed: {e}"))?;
            if !resp.status().is_success() {
                return Err(anyhow!("download returned HTTP {}", resp.status()));
            }
            let mut decrypt = m.key.decryptor_at(start);
            let mut offset = start;
            let mut stream = resp.bytes_stream();
            let idle_limit = Duration::from_secs(IDLE_TIMEOUT_SECS);
            loop {
                if stop.load(Ordering::Relaxed) {
                    return Ok(());
                }
                match tokio::time::timeout(idle_limit, stream.next()).await {
                    Err(_) => {
                        return Err(anyhow!("download stalled — no data received for {IDLE_TIMEOUT_SECS}s"))
                    }
                    Ok(None) => break,
                    Ok(Some(Err(e))) => return Err(anyhow!("download read error: {e}")),
                    Ok(Some(Ok(chunk))) => {
                        let mut plain = chunk.to_vec();
                        decrypt.apply(&mut plain);
                        write_at(file, &plain, offset).map_err(|e| anyhow!("write error: {e}"))?;
                        offset += plain.len() as u64;
                        done.fetch_add(plain.len() as u64, Ordering::Relaxed);
                    }
                }
            }
            if offset != end {
                return Err(anyhow!("MEGA ended a range early ({offset} of {end} bytes)"));
            }
            Ok(())
        }
    };

    let mut work = Box::pin(try_join_all(ranges.iter().map(|&(a, b)| fetch(a, b))));
    loop {
        let tick = Box::pin(tokio::time::sleep(Duration::from_millis(400)));
        match select(work, tick).await {
            Either::Left((result, _)) => {
                result?;
                break;
            }
            Either::Right((_, pending)) => {
                work = pending;
                progress(done.load(Ordering::Relaxed), Some(size));
                if cancel.load(Ordering::Relaxed) {
                    stop.store(true, Ordering::Relaxed);
                    return Ok(false);
                }
            }
        }
    }
    progress(size, Some(size));
    Ok(true)
}

async fn download_async(
    url: &str,
    mut decrypt: Option<crate::mega::CtrDecryptor>,
    dest_path: &Path,
    cancel: &std::sync::atomic::AtomicBool,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<bool> {
    use futures_util::StreamExt;
    use std::sync::atomic::Ordering;

    let client = download_client(url.contains("pixeldrain.com"))?;
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| anyhow!("download request failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(anyhow!("download returned HTTP {}", resp.status()));
    }
    let total = resp.content_length();
    let mut file = std::fs::File::create(dest_path)
        .map_err(|e| anyhow!("cannot create {}: {e}", dest_path.display()))?;
    let mut stream = resp.bytes_stream();
    let mut downloaded: u64 = 0;
    let tick = Duration::from_secs(1);
    let idle_limit = Duration::from_secs(IDLE_TIMEOUT_SECS);
    let mut idle = Duration::ZERO;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(false);
        }
        match tokio::time::timeout(tick, stream.next()).await {
            // No chunk in the last second — accumulate idle time, bail if stalled.
            Err(_) => {
                idle += tick;
                if idle >= idle_limit {
                    return Err(anyhow!(
                        "download stalled — no data received for {IDLE_TIMEOUT_SECS}s"
                    ));
                }
            }
            Ok(None) => break, // stream finished
            Ok(Some(Ok(chunk))) => {
                idle = Duration::ZERO;
                let chunk = match decrypt.as_mut() {
                    Some(d) => {
                        let mut plain = chunk.to_vec();
                        d.apply(&mut plain);
                        plain.into()
                    }
                    None => chunk,
                };
                file.write_all(&chunk)
                    .map_err(|e| anyhow!("write error: {e}"))?;
                downloaded += chunk.len() as u64;
                progress(downloaded, total);
            }
            Ok(Some(Err(e))) => return Err(anyhow!("download read error: {e}")),
        }
    }
    file.flush().ok();
    Ok(true)
}

/// Port of the reference's `IsVarCorruptedOrFake`: a real .var is a readable zip
/// with at least one entry under `Custom/` or `Saves/`.
pub(crate) fn is_valid_var(path: &Path) -> bool {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return false,
    };
    let mut archive = match zip::ZipArchive::new(file) {
        Ok(a) => a,
        Err(_) => return false,
    };
    for i in 0..archive.len() {
        if let Ok(entry) = archive.by_index(i) {
            let name = entry.name().replace('\\', "/").to_ascii_lowercase();
            if name.starts_with("custom/") || name.starts_with("saves/") {
                return true;
            }
        }
    }
    false
}

// ----------------------------------------------------------------------------
// Package card data for dependency lists (title, author, thumbnail, size)
// ----------------------------------------------------------------------------

/// Hub resource-icon CDN; icons are sharded into folders by floor(id / 1000).
/// Same source VaM Backstage uses for its Hub thumbnails.
const RESOURCE_ICON_CDN: &str = "https://1424104733.rsc.cdn77.org/data/resource_icons";

/// What a dependency row shows for a package that isn't on disk.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub(crate) struct HubPackageMeta {
    /// The Hub has a resource for this package family.
    pub(crate) found: bool,
    pub(crate) resource_id: Option<String>,
    pub(crate) title: Option<String>,
    /// The Hub author (can differ from the id's creator segment).
    pub(crate) username: Option<String>,
    pub(crate) tag_line: Option<String>,
    /// Hub resource type: "Scenes", "Looks", "Clothing", ...
    pub(crate) resource_type: Option<String>,
    /// "Free" | "Paid".
    pub(crate) category: Option<String>,
    pub(crate) license: Option<String>,
    pub(crate) file_size: Option<u64>,
    pub(crate) filename: Option<String>,
    pub(crate) hub_url: Option<String>,
    /// `data:image/...;base64,...` of the resource icon (or its latest image).
    pub(crate) image_data: Option<String>,
    /// Set when the Hub couldn't be reached; "not found" is `found: false`
    /// with no error.
    pub(crate) error: Option<String>,
}

fn str_field(v: &serde_json::Value, key: &str) -> Option<String> {
    match v.get(key)? {
        serde_json::Value::String(s) if !s.trim().is_empty() && s != "null" => Some(s.trim().to_string()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn get_resource_detail(client: &reqwest::blocking::Client, package_name: &str) -> Result<serde_json::Value> {
    // Same body shape as `find_packages` (VaM's spacing, for Cloudflare).
    // Package ids never contain `"`/`\`, so direct interpolation is safe.
    let body = format!(
        "{{\"source\":\"VaM\", \"action\":\"getResourceDetail\", \"latest_image\":\"Y\", \"package_name\":\"{package_name}\"}}"
    );
    let resp = client
        .post(API_URL)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(reqwest::header::ACCEPT, "application/json")
        .body(body)
        .send()
        .map_err(|e| anyhow!("hub request failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(anyhow!("hub returned HTTP {}", resp.status()));
    }
    let text = resp.text().map_err(|e| anyhow!("reading hub response failed: {e}"))?;
    serde_json::from_str(&text).map_err(|e| anyhow!("hub response was not JSON ({e})"))
}

/// Backstage's guard: a name lookup can return a resource that merely lists the
/// package as a dependency. Accept it only if one of its files (or a dependency
/// key, for multi-var listings) is the requested family.
pub(crate) fn detail_matches_family(detail: &serde_json::Value, family_lc: &str) -> bool {
    let matches = |name: &str| {
        let stem = name.trim().trim_end_matches(".var").trim_end_matches(".VAR");
        crate::naming::package_base(stem).eq_ignore_ascii_case(family_lc)
    };
    let in_files = detail
        .get("hubFiles")
        .and_then(|f| f.as_array())
        .is_some_and(|files| {
            files
                .iter()
                .filter_map(|f| f.get("filename").and_then(|n| n.as_str()))
                .any(matches)
        });
    in_files
        || detail
            .get("dependencies")
            .and_then(|d| d.as_object())
            .is_some_and(|deps| deps.keys().any(|k| matches(k)))
}

fn fetch_image_data(client: &reqwest::blocking::Client, url: &str) -> Option<String> {
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
    let resp = client.get(url).send().ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let mime = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .filter(|v| v.starts_with("image/"))
        .unwrap_or("image/jpeg")
        .to_string();
    let bytes = resp.bytes().ok()?;
    // A thumbnail, not a download: refuse anything implausibly large.
    if bytes.is_empty() || bytes.len() > 4 * 1024 * 1024 {
        return None;
    }
    Some(format!("data:{mime};base64,{}", BASE64.encode(&bytes)))
}

/// Looks a package family up on the Hub (`getResourceDetail` by package name,
/// as VaM Backstage does) and returns what a dependency row shows, including
/// the resource icon as a data URL.
pub(crate) fn fetch_package_meta(client: &reqwest::blocking::Client, package_id: &str) -> HubPackageMeta {
    let family = crate::naming::package_base(package_id.trim()).to_string();
    let family_lc = family.to_ascii_lowercase();
    let detail = match get_resource_detail(client, &format!("{family}.latest")) {
        Ok(d) => d,
        Err(e) => {
            return HubPackageMeta {
                error: Some(e.to_string()),
                ..HubPackageMeta::default()
            }
        }
    };
    if detail.get("status").and_then(|s| s.as_str()) == Some("error")
        || str_field(&detail, "resource_id").is_none()
        || !detail_matches_family(&detail, &family_lc)
    {
        return HubPackageMeta::default();
    }

    let resource_id = str_field(&detail, "resource_id");
    let file = detail
        .get("hubFiles")
        .and_then(|f| f.as_array())
        .and_then(|files| {
            files.iter().find(|f| {
                f.get("filename")
                    .and_then(|n| n.as_str())
                    .is_some_and(|n| n.to_ascii_lowercase().starts_with(&format!("{family_lc}.")))
            })
        });
    let icon_url = resource_id
        .as_deref()
        .and_then(|id| id.parse::<u64>().ok())
        .map(|n| format!("{RESOURCE_ICON_CDN}/{}/{n}.jpg", n / 1000));
    let image_data = icon_url
        .as_deref()
        .and_then(|u| fetch_image_data(client, u))
        .or_else(|| str_field(&detail, "image_url").and_then(|u| fetch_image_data(client, &u)));

    HubPackageMeta {
        found: true,
        hub_url: resource_id
            .as_deref()
            .map(|id| format!("https://hub.virtamate.com/resources/{id}/")),
        resource_id,
        title: str_field(&detail, "title"),
        username: str_field(&detail, "username"),
        tag_line: str_field(&detail, "tag_line"),
        resource_type: str_field(&detail, "type"),
        category: str_field(&detail, "category"),
        license: file
            .and_then(|f| str_field(f, "licenseType"))
            .or_else(|| str_field(&detail, "licenseType")),
        file_size: file.and_then(|f| f.get("file_size")).and_then(parse_size),
        filename: file.and_then(|f| str_field(f, "filename")),
        image_data,
        error: None,
    }
}

// ----------------------------------------------------------------------------
// Hub page (browse resources, as VaM Backstage does)
// ----------------------------------------------------------------------------

/// Read-only Hub API actions the Hub page may call.
pub(crate) const BROWSE_ACTIONS: &[&str] = &["getInfo", "getResources", "getResourceDetail", "findPackages"];

/// Builds a request body the way VaM writes it: compact JSON with ", " between
/// fields (the Hub sits behind Cloudflare, which can filter bodies that don't
/// look like VaM's). Keys and values are JSON-encoded, so any string is safe.
pub(crate) fn vam_body(action: &str, params: &serde_json::Map<String, serde_json::Value>) -> String {
    let mut parts = vec![
        "\"source\":\"VaM\"".to_string(),
        format!("\"action\":{}", serde_json::Value::String(action.to_string())),
    ];
    for (key, value) in params {
        if key == "source" || key == "action" {
            continue;
        }
        parts.push(format!("{}:{}", serde_json::Value::String(key.clone()), value));
    }
    format!("{{{}}}", parts.join(", "))
}

/// POSTs one Hub API action and returns the parsed JSON response.
pub(crate) fn api_request(
    client: &reqwest::blocking::Client,
    action: &str,
    params: &serde_json::Map<String, serde_json::Value>,
) -> Result<serde_json::Value> {
    let resp = client
        .post(API_URL)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(reqwest::header::ACCEPT, "application/json")
        .body(vam_body(action, params))
        .send()
        .map_err(|e| anyhow!("hub request failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(anyhow!("hub returned HTTP {}", resp.status()));
    }
    let text = resp.text().map_err(|e| anyhow!("reading hub response failed: {e}"))?;
    serde_json::from_str(&text).map_err(|e| {
        anyhow!(
            "hub response was not JSON ({e}); starts with: {}",
            text.chars().take(120).collect::<String>()
        )
    })
}

/// Fetches an image (resource icon / screenshot) as a data URL.
pub(crate) fn image_data_url(client: &reqwest::blocking::Client, url: &str) -> Option<String> {
    fetch_image_data(client, url)
}
