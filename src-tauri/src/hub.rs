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
const CHROME_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
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
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| anyhow!("failed to start download runtime: {e}"))?;
    rt.block_on(download_async(url, dest_path, cancel, progress))
}

async fn download_async(
    url: &str,
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
