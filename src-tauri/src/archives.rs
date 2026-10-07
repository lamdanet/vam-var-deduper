//! `.zip` archives as download sources: a link to a zip (often
//! password-protected, as shared on forums) whose `.var` files are each saved
//! as a source with their path inside it.
//!
//! Listing reads only the zip's central directory through range requests —
//! HTTP `Range` (Pixeldrain and most hosts), or MEGA's `<address>/<a>-<b>`
//! decrypted from that offset — so a 2 GB archive is listed in a few small
//! requests. Extracting downloads the whole zip once into a session
//! cache, so several packages from one archive cost one download.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{atomic::AtomicBool, Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ArchiveEntry {
    /// The path inside the archive.
    pub(crate) name: String,
    pub(crate) size: u64,
    /// Needs the archive's password to extract.
    pub(crate) encrypted: bool,
}

/// The archive kind a file name names, when it is one.
pub(crate) fn archive_kind(name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".zip") {
        Some("zip")
    } else if lower.ends_with(".7z") {
        Some("7z")
    } else if lower.ends_with(".rar") {
        Some("rar")
    } else {
        None
    }
}

pub(crate) const UNSUPPORTED_ARCHIVE: &str =
    "7z and RAR archives aren't supported yet — only .zip. Extract it yourself and drop the .var files into AddonPackages.";

// ----------------------------------------------------------------------------
// Reading a remote file through HTTP ranges
// ----------------------------------------------------------------------------

const BLOCK: u64 = 256 * 1024;

/// `Read + Seek` over a remote file, fetching 256 KB blocks on demand with
/// range requests. Enough for the zip crate to read an archive's directory.
struct HttpRangeReader {
    client: reqwest::blocking::Client,
    url: String,
    len: u64,
    pos: u64,
    blocks: HashMap<u64, Vec<u8>>,
    /// A MEGA file: `url` is its temporary address, ranges are requested as
    /// `<url>/<a>-<b>` and decrypted with this key.
    mega: Option<crate::mega::FileKey>,
}

impl HttpRangeReader {
    fn open(url: &str) -> Result<Self, String> {
        let client = reqwest::blocking::Client::builder()
            .user_agent(crate::hub::CHROME_USER_AGENT)
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| e.to_string())?;
        let resp = client
            .get(url)
            .header(reqwest::header::RANGE, "bytes=0-0")
            .send()
            .map_err(|e| format!("Couldn't reach the archive: {e}"))?;
        if resp.status() != reqwest::StatusCode::PARTIAL_CONTENT {
            return Err(format!("the host doesn't serve parts of files (HTTP {})", resp.status()));
        }
        let len = resp
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.rsplit('/').next())
            .and_then(|n| n.trim().parse::<u64>().ok())
            .ok_or_else(|| "the host didn't say how big the archive is".to_string())?;
        Ok(Self { client, url: url.to_string(), len, pos: 0, blocks: HashMap::new(), mega: None })
    }

    /// A MEGA file, through its temporary address and key.
    fn open_mega(m: crate::mega::MegaDownload) -> Result<Self, String> {
        let len = m.size.ok_or_else(|| "MEGA didn't say how big the archive is".to_string())?;
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self { client, url: m.address, len, pos: 0, blocks: HashMap::new(), mega: Some(m.key) })
    }

    fn block(&mut self, index: u64) -> io::Result<&Vec<u8>> {
        if !self.blocks.contains_key(&index) {
            let start = index * BLOCK;
            let end = ((index + 1) * BLOCK).min(self.len) - 1;
            let bytes = match self.mega {
                Some(key) => {
                    let resp = self
                        .client
                        .get(format!("{}/{start}-{end}", self.url))
                        .send()
                        .map_err(io::Error::other)?;
                    if !resp.status().is_success() {
                        return Err(io::Error::other(format!("MEGA range request returned HTTP {}", resp.status())));
                    }
                    let mut bytes = resp.bytes().map_err(io::Error::other)?.to_vec();
                    key.decryptor_at(start).apply(&mut bytes);
                    bytes
                }
                None => {
                    let resp = self
                        .client
                        .get(&self.url)
                        .header(reqwest::header::RANGE, format!("bytes={start}-{end}"))
                        .send()
                        .map_err(io::Error::other)?;
                    if resp.status() != reqwest::StatusCode::PARTIAL_CONTENT {
                        return Err(io::Error::other(format!("range request returned HTTP {}", resp.status())));
                    }
                    resp.bytes().map_err(io::Error::other)?.to_vec()
                }
            };
            self.blocks.insert(index, bytes);
        }
        Ok(&self.blocks[&index])
    }
}

impl Read for HttpRangeReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.len || buf.is_empty() {
            return Ok(0);
        }
        let index = self.pos / BLOCK;
        let within = (self.pos % BLOCK) as usize;
        let block = self.block(index)?;
        if within >= block.len() {
            return Ok(0);
        }
        let n = buf.len().min(block.len() - within);
        buf[..n].copy_from_slice(&block[within..within + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for HttpRangeReader {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let pos = match to {
            SeekFrom::Start(p) => p as i128,
            SeekFrom::End(d) => self.len as i128 + d as i128,
            SeekFrom::Current(d) => self.pos as i128 + d as i128,
        };
        if pos < 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "seek before start"));
        }
        self.pos = pos as u64;
        Ok(self.pos)
    }
}

// ----------------------------------------------------------------------------
// Listing
// ----------------------------------------------------------------------------

fn list_entries<R: Read + Seek>(reader: R) -> Result<Vec<ArchiveEntry>, String> {
    let mut zip = zip::ZipArchive::new(reader).map_err(|e| format!("not a readable .zip: {e}"))?;
    let mut out = Vec::new();
    for i in 0..zip.len() {
        let Ok(file) = zip.by_index_raw(i) else { continue };
        if file.is_dir() {
            continue;
        }
        out.push(ArchiveEntry { name: file.name().to_string(), size: file.size(), encrypted: file.encrypted() });
    }
    Ok(out)
}

/// The files in a remote zip. Reads just its directory through range requests
/// when the host allows (MEGA always does); otherwise downloads it into the
/// cache and lists that.
pub(crate) fn list_remote(url: &str) -> Result<Vec<ArchiveEntry>, String> {
    let reader = if crate::mega::is_mega(url) {
        crate::mega::open_download(url).and_then(HttpRangeReader::open_mega)
    } else {
        HttpRangeReader::open(url)
    };
    if let Ok(reader) = reader {
        return list_entries(reader);
    }
    let cancel = AtomicBool::new(false);
    let path = ensure_cached(url, &cancel, &mut |_, _| {})?;
    let file = fs::File::open(&path).map_err(|e| e.to_string())?;
    list_entries(io::BufReader::new(file))
}

// ----------------------------------------------------------------------------
// Session cache
// ----------------------------------------------------------------------------

fn cache_dir() -> PathBuf {
    std::env::temp_dir().join("vam-var-archives")
}

fn cache_path(url: &str) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut h);
    cache_dir().join(format!("{:016x}.zip", h.finish()))
}

/// One lock per archive URL, so two packages from one zip don't download it
/// twice at the same time — the second waits and reuses the first's copy.
fn url_lock(url: &str) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();
    let mut map = LOCKS.get_or_init(|| Mutex::new(HashMap::new())).lock().unwrap_or_else(|p| p.into_inner());
    Arc::clone(map.entry(url.to_string()).or_default())
}

/// Drops cached archives older than a day.
fn prune_cache() {
    let Ok(entries) = fs::read_dir(cache_dir()) else { return };
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > Duration::from_secs(24 * 3600));
        if old {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// The archive at `url` on disk, downloading it (with `progress`) unless an
/// intact copy is already cached.
pub(crate) fn ensure_cached(
    url: &str,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<PathBuf, String> {
    let lock = url_lock(url);
    let _guard = lock.lock().unwrap_or_else(|p| p.into_inner());
    let path = cache_path(url);
    if fs::File::open(&path).ok().and_then(|f| zip::ZipArchive::new(f).ok()).is_some() {
        return Ok(path);
    }
    prune_cache();
    fs::create_dir_all(cache_dir()).map_err(|e| e.to_string())?;
    let part = path.with_extension("part");
    match crate::hub::download_to_file(url, &part, cancel, progress) {
        Ok(true) => {}
        Ok(false) => {
            let _ = fs::remove_file(&part);
            return Err("cancelled".to_string());
        }
        Err(e) => {
            let _ = fs::remove_file(&part);
            return Err(e.to_string());
        }
    }
    if zip::ZipArchive::new(fs::File::open(&part).map_err(|e| e.to_string())?).is_err() {
        let _ = fs::remove_file(&part);
        return Err("the download isn't a .zip archive (the link may point to a web page)".to_string());
    }
    let _ = fs::remove_file(&path);
    fs::rename(&part, &path).map_err(|e| e.to_string())?;
    Ok(path)
}

// ----------------------------------------------------------------------------
// Extracting
// ----------------------------------------------------------------------------

fn base_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// Writes the archive member `entry` (matched by path, else by file name) to
/// `out`, decrypting with `password` when it is encrypted.
pub(crate) fn extract_entry(zip_path: &Path, entry: &str, password: Option<&str>, out: &Path) -> Result<(), String> {
    let file = fs::File::open(zip_path).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(io::BufReader::new(file)).map_err(|e| format!("not a readable .zip: {e}"))?;
    let wanted = base_name(entry).to_ascii_lowercase();
    let index = zip.index_for_name(entry).or_else(|| {
        (0..zip.len()).find(|&i| zip.name_for_index(i).is_some_and(|n| base_name(n).to_ascii_lowercase() == wanted))
    });
    let index = index.ok_or_else(|| format!("{entry} isn't in the archive"))?;
    let encrypted = zip.by_index_raw(index).map(|f| f.encrypted()).unwrap_or(false);
    let password = password.map(str::trim).filter(|p| !p.is_empty());
    if encrypted && password.is_none() {
        return Err("the archive is password-protected — add its password to the source".to_string());
    }
    let wrong_password = || "wrong password for the archive".to_string();
    let mut member = match (encrypted, password) {
        (true, Some(pw)) => zip.by_index_decrypt(index, pw.as_bytes()).map_err(|e| match e {
            zip::result::ZipError::InvalidPassword => wrong_password(),
            other => format!("couldn't open {entry}: {other}"),
        })?,
        _ => zip.by_index(index).map_err(|e| format!("couldn't open {entry}: {e}"))?,
    };
    let mut target = fs::File::create(out).map_err(|e| format!("cannot create {}: {e}", out.display()))?;
    if let Err(e) = io::copy(&mut member, &mut target) {
        drop(target);
        let _ = fs::remove_file(out);
        // ZipCrypto only notices a wrong password at the checksum.
        return Err(if encrypted { wrong_password() } else { format!("couldn't extract {entry}: {e}") });
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn list_local_for_test(path: &Path) -> Result<Vec<ArchiveEntry>, String> {
    list_entries(fs::File::open(path).map_err(|e| e.to_string())?)
}

#[cfg(test)]
pub(crate) fn list_http_for_test(url: &str) -> Result<Vec<ArchiveEntry>, String> {
    list_entries(HttpRangeReader::open(url)?)
}

#[cfg(test)]
pub(crate) fn list_mega_for_test(m: crate::mega::MegaDownload) -> Result<Vec<ArchiveEntry>, String> {
    list_entries(HttpRangeReader::open_mega(m)?)
}
