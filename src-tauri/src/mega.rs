//! MEGA links. MEGA encrypts everything client-side and the link carries the
//! key after the `#`:
//!
//! - a file: `mega.nz/file/<handle>#<key>` (or the older `#!<handle>!<key>`),
//!   whose 256-bit key decrypts the file directly;
//! - a folder: `mega.nz/folder/<handle>#<key>` (or `#F!<handle>!<key>`),
//!   whose 128-bit key unwraps the key of every file inside;
//! - a file inside a folder: `mega.nz/folder/<handle>#<key>/file/<node>` —
//!   MEGA's own form, and what a file picked from a folder is saved as.
//!
//! The public API lists a folder (`a: "f"`) and hands out a temporary address
//! for a file's encrypted bytes (`a: "g"`); names and contents are decrypted
//! here with AES-128 — ECB to unwrap keys, CBC for names, CTR for content.

use std::collections::HashMap;
use std::time::Duration;

use aes::cipher::{generic_array::GenericArray, BlockDecrypt, BlockEncrypt, KeyInit};
use aes::Aes128;
use base64::Engine;
use serde::Serialize;
use serde_json::{json, Value};

const API_URL: &str = "https://g.api.mega.co.nz/cs";

pub(crate) fn is_mega(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.contains("://mega.nz") || lower.contains("://mega.co.nz") || lower.contains("://www.mega.nz")
}

/// A file's 256-bit key: the AES key is its halves XORed, the CTR nonce its
/// third quarter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileKey([u8; 32]);

impl FileKey {
    fn aes_key(&self) -> [u8; 16] {
        let mut k = [0u8; 16];
        for (i, b) in k.iter_mut().enumerate() {
            *b = self.0[i] ^ self.0[i + 16];
        }
        k
    }

    pub(crate) fn decryptor(&self) -> CtrDecryptor {
        self.decryptor_at(0)
    }

    /// A decryptor for the bytes from `offset` on — CTR can start anywhere,
    /// which is what lets a file download in parallel ranges.
    pub(crate) fn decryptor_at(&self, offset: u64) -> CtrDecryptor {
        let mut nonce = [0u8; 8];
        nonce.copy_from_slice(&self.0[16..24]);
        let mut d = CtrDecryptor::new(self.aes_key(), nonce);
        d.seek(offset);
        d
    }
}

/// A parsed MEGA link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MegaRef {
    File { handle: String, key: FileKey },
    /// A shared folder, optionally narrowed to one of its subfolders.
    Folder { handle: String, key: [u8; 16], key_text: String, sub: Option<String> },
    /// One file inside a shared folder.
    FolderFile { handle: String, key: [u8; 16], node: String },
}

fn id_chars(s: &str) -> String {
    s.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect()
}

fn decode_key(text: &str) -> Result<Vec<u8>, String> {
    if text.is_empty() {
        return Err(missing_key());
    }
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(text.trim_end_matches('=').as_bytes())
        .map_err(|_| "The MEGA link's key is malformed.".to_string())
}

fn missing_key() -> String {
    "This MEGA link has no key (the part after #) — copy the full link, key included.".to_string()
}

fn wrong_length() -> String {
    "The MEGA link's key has the wrong length — copy the whole link.".to_string()
}

/// Parses every MEGA link form above.
pub(crate) fn parse(url: &str) -> Result<MegaRef, String> {
    let url = url.trim();
    // Folder: /folder/<h>#<k>[/file/<n> | /folder/<sub>] or #F!<h>!<k>[!<n>].
    let folder = if let Some(pos) = url.find("/folder/") {
        let rest = &url[pos + "/folder/".len()..];
        let (handle, after) = rest.split_once('#').ok_or_else(missing_key)?;
        Some((id_chars(handle), after.to_string()))
    } else if let Some(pos) = url.find("#F!") {
        let rest = &url[pos + 3..];
        let (handle, after) = rest.split_once('!').ok_or_else(missing_key)?;
        // The legacy form names a file inside with a trailing `!<node>`.
        let after = after.replacen('!', "/file/", 1);
        Some((id_chars(handle), after))
    } else {
        None
    };
    if let Some((handle, after)) = folder {
        let key_text = id_chars(&after);
        let key: [u8; 16] = decode_key(&key_text)?.try_into().map_err(|_| wrong_length())?;
        if handle.is_empty() {
            return Err("The MEGA link has no folder id.".to_string());
        }
        let tail = &after[key_text.len()..];
        if let Some(rest) = tail.strip_prefix("/file/") {
            let node = id_chars(rest);
            if !node.is_empty() {
                return Ok(MegaRef::FolderFile { handle, key, node });
            }
        }
        let sub = tail.strip_prefix("/folder/").map(id_chars).filter(|s| !s.is_empty());
        return Ok(MegaRef::Folder { handle, key, key_text, sub });
    }
    let (handle, key_text) = if let Some(pos) = url.find("/file/") {
        url[pos + "/file/".len()..].split_once('#').ok_or_else(missing_key)?
    } else if let Some(pos) = url.find("#!") {
        url[pos + 2..].split_once('!').ok_or_else(missing_key)?
    } else {
        return Err("That isn't a MEGA file or folder link.".to_string());
    };
    let handle = id_chars(handle);
    if handle.is_empty() {
        return Err("The MEGA link has no file id.".to_string());
    }
    let key: [u8; 32] = decode_key(&id_chars(key_text))?.try_into().map_err(|_| wrong_length())?;
    Ok(MegaRef::File { handle, key: FileKey(key) })
}

/// MEGA's link for a file inside a shared folder.
fn folder_file_url(handle: &str, key_text: &str, node: &str) -> String {
    format!("https://mega.nz/folder/{handle}#{key_text}/file/{node}")
}

// ----------------------------------------------------------------------------
// Crypto
// ----------------------------------------------------------------------------

fn aes(key: &[u8; 16]) -> Aes128 {
    Aes128::new(GenericArray::from_slice(key))
}

/// The name in a node's encrypted attributes: AES-CBC (zero IV) over
/// `MEGA{"n":"<name>",...}`, zero-padded.
fn decrypt_name(aes_key: &[u8; 16], at: &str) -> Option<String> {
    let mut data = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(at.trim_end_matches('=').as_bytes())
        .ok()?;
    if data.is_empty() || data.len() % 16 != 0 {
        return None;
    }
    let cipher = aes(aes_key);
    let mut prev = [0u8; 16];
    for chunk in data.chunks_mut(16) {
        let mut cur = [0u8; 16];
        cur.copy_from_slice(chunk);
        cipher.decrypt_block(GenericArray::from_mut_slice(chunk));
        for (b, p) in chunk.iter_mut().zip(prev.iter()) {
            *b ^= p;
        }
        prev = cur;
    }
    let text = String::from_utf8_lossy(&data);
    let json = text.strip_prefix("MEGA")?.trim_end_matches('\0');
    let attrs: Value = serde_json::from_str(json).ok()?;
    attrs.get("n").and_then(Value::as_str).map(str::to_string)
}

/// A node key from a folder listing, unwrapped with the folder key (AES-ECB).
/// `k` is `<id>:<key>`, sometimes several joined by `/`; the one wrapped with
/// this folder's key is the one whose attributes then decrypt.
fn unwrap_node_key(folder_key: &[u8; 16], k: &str, at: &str, is_file: bool) -> Option<(Vec<u8>, String)> {
    let cipher = aes(folder_key);
    for part in k.split('/') {
        let Some((_, wrapped)) = part.split_once(':') else { continue };
        let Ok(mut key) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(wrapped.trim_end_matches('=').as_bytes()) else {
            continue;
        };
        if key.len() != if is_file { 32 } else { 16 } {
            continue;
        }
        for block in key.chunks_mut(16) {
            cipher.decrypt_block(GenericArray::from_mut_slice(block));
        }
        let aes_key: [u8; 16] = if is_file {
            FileKey(key.clone().try_into().ok()?).aes_key()
        } else {
            key.clone().try_into().ok()?
        };
        if let Some(name) = decrypt_name(&aes_key, at) {
            return Some((key, name));
        }
    }
    None
}

/// AES-128-CTR over a byte stream, fed chunk by chunk in order. The counter
/// block is the 8-byte nonce followed by the big-endian 64-bit block index.
pub(crate) struct CtrDecryptor {
    cipher: Aes128,
    nonce: [u8; 8],
    offset: u64,
    keystream: [u8; 16],
}

impl CtrDecryptor {
    fn new(key: [u8; 16], nonce: [u8; 8]) -> Self {
        Self { cipher: aes(&key), nonce, offset: 0, keystream: [0; 16] }
    }

    fn block_keystream(&self, block_index: u64) -> [u8; 16] {
        let mut block = [0u8; 16];
        block[..8].copy_from_slice(&self.nonce);
        block[8..].copy_from_slice(&block_index.to_be_bytes());
        self.cipher.encrypt_block(GenericArray::from_mut_slice(&mut block));
        block
    }

    fn seek(&mut self, offset: u64) {
        self.offset = offset;
        if offset % 16 != 0 {
            self.keystream = self.block_keystream(offset / 16);
        }
    }

    pub(crate) fn apply(&mut self, data: &mut [u8]) {
        for byte in data.iter_mut() {
            let pos = (self.offset % 16) as usize;
            if pos == 0 {
                self.keystream = self.block_keystream(self.offset / 16);
            }
            *byte ^= self.keystream[pos];
            self.offset += 1;
        }
    }
}

// ----------------------------------------------------------------------------
// API (blocking: callers run on a plain thread)
// ----------------------------------------------------------------------------

fn error_text(code: i64) -> String {
    match code {
        -9 => "MEGA: not found (removed, or a wrong link).".to_string(),
        -11 => "MEGA: access denied.".to_string(),
        -16 => "MEGA: taken down.".to_string(),
        -17 | -4 => "MEGA: transfer quota exceeded — try again later, or open the link in your browser.".to_string(),
        -18 | -3 => "MEGA is temporarily unavailable — try again shortly.".to_string(),
        other => format!("MEGA API error {other}."),
    }
}

/// One API command; `folder` scopes it to a shared folder (`?n=`).
fn api(command: Value, folder: Option<&str>) -> Result<Value, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;
    let id = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let mut url = format!("{API_URL}?id={id}");
    if let Some(folder) = folder {
        url.push_str(&format!("&n={folder}"));
    }
    let value: Value = client
        .post(url)
        .json(&json!([command]))
        .send()
        .map_err(|e| format!("Couldn't reach MEGA: {e}"))?
        .json()
        .map_err(|e| format!("MEGA sent an unexpected answer: {e}"))?;
    let first = match value {
        Value::Array(mut items) if !items.is_empty() => items.swap_remove(0),
        other => other,
    };
    if let Some(code) = first.as_i64() {
        return Err(error_text(code));
    }
    if let Some(code) = first.get("e").and_then(Value::as_i64) {
        return Err(error_text(code));
    }
    Ok(first)
}

/// A temporary download address, from an `a: "g"` answer.
fn download_address(answer: &Value) -> Result<(String, Option<u64>), String> {
    let url = answer
        .get("g")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "MEGA gave no download address (transfer quota?).".to_string())?;
    Ok((url.to_string(), answer.get("s").and_then(Value::as_u64)))
}

/// One `.var` in a shared folder.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub(crate) struct FolderEntry {
    pub(crate) name: String,
    /// Folders between the shared folder and the file, `/`-joined.
    pub(crate) path: String,
    pub(crate) size: Option<u64>,
    /// MEGA's link for this file inside the folder — what gets saved.
    pub(crate) url: String,
}

struct Node {
    handle: String,
    parent: String,
    is_file: bool,
    name: String,
    key: Vec<u8>,
    size: Option<u64>,
}

fn list_nodes(handle: &str, key: &[u8; 16]) -> Result<Vec<Node>, String> {
    let answer = api(json!({ "a": "f", "c": 1, "ca": 1, "r": 1 }), Some(handle))?;
    let nodes = answer.get("f").and_then(Value::as_array).cloned().unwrap_or_default();
    Ok(nodes
        .iter()
        .filter_map(|n| {
            let is_file = n.get("t").and_then(Value::as_i64)? == 0;
            let (node_key, name) = unwrap_node_key(
                key,
                n.get("k").and_then(Value::as_str)?,
                n.get("a").and_then(Value::as_str)?,
                is_file,
            )?;
            Some(Node {
                handle: n.get("h").and_then(Value::as_str)?.to_string(),
                parent: n.get("p").and_then(Value::as_str).unwrap_or_default().to_string(),
                is_file,
                name,
                key: node_key,
                size: n.get("s").and_then(Value::as_u64),
            })
        })
        .collect())
}

/// The `.var` files of a shared folder (or of one of its subfolders), with
/// their path inside it, sorted by path.
fn folder_vars(handle: &str, key: &[u8; 16], key_text: &str, sub: Option<&str>) -> Result<Vec<FolderEntry>, String> {
    let nodes = list_nodes(handle, key)?;
    let by_handle: HashMap<&str, &Node> = nodes.iter().map(|n| (n.handle.as_str(), n)).collect();
    let path_of = |node: &Node| -> (String, bool) {
        let mut parts: Vec<&str> = Vec::new();
        let mut under_sub = sub.is_none();
        let mut cur = by_handle.get(node.parent.as_str()).copied();
        while let Some(dir) = cur {
            if Some(dir.handle.as_str()) == sub {
                under_sub = true;
            }
            parts.push(dir.name.as_str());
            cur = by_handle.get(dir.parent.as_str()).copied();
        }
        parts.reverse();
        (parts.join("/"), under_sub)
    };
    let mut out: Vec<FolderEntry> = nodes
        .iter()
        .filter(|n| n.is_file && n.name.to_ascii_lowercase().ends_with(".var"))
        .filter_map(|n| {
            let (path, under_sub) = path_of(n);
            under_sub.then(|| FolderEntry {
                name: n.name.clone(),
                path,
                size: n.size,
                url: folder_file_url(handle, key_text, &n.handle),
            })
        })
        .collect();
    out.sort_by(|a, b| (a.path.to_lowercase(), a.name.to_lowercase()).cmp(&(b.path.to_lowercase(), b.name.to_lowercase())));
    Ok(out)
}

/// A file inside a shared folder: its key and name, from the folder listing.
fn folder_file(handle: &str, key: &[u8; 16], node: &str) -> Result<(FileKey, String, Option<u64>), String> {
    let nodes = list_nodes(handle, key)?;
    let n = nodes
        .into_iter()
        .find(|n| n.handle == node && n.is_file)
        .ok_or_else(|| "MEGA: that file is no longer in the folder.".to_string())?;
    let key: [u8; 32] = n.key.try_into().map_err(|_| wrong_length())?;
    Ok((FileKey(key), n.name, n.size))
}

/// What a link points at, for the Add download source dialog.
#[derive(Debug, Clone, Default)]
pub(crate) struct Inspected {
    pub(crate) name: Option<String>,
    pub(crate) size: Option<u64>,
    /// For a folder link: the `.var` files in it.
    pub(crate) files: Option<Vec<FolderEntry>>,
}

pub(crate) fn inspect(url: &str) -> Result<Inspected, String> {
    match parse(url)? {
        MegaRef::File { handle, key } => {
            let answer = api(json!({ "a": "g", "p": handle }), None)?;
            Ok(Inspected {
                name: answer.get("at").and_then(Value::as_str).and_then(|at| decrypt_name(&key.aes_key(), at)),
                size: answer.get("s").and_then(Value::as_u64),
                files: None,
            })
        }
        MegaRef::FolderFile { handle, key, node } => {
            let (_, name, size) = folder_file(&handle, &key, &node)?;
            Ok(Inspected { name: Some(name), size, files: None })
        }
        MegaRef::Folder { handle, key, key_text, sub } => Ok(Inspected {
            files: Some(folder_vars(&handle, &key, &key_text, sub.as_deref())?),
            ..Default::default()
        }),
    }
}

/// Where to fetch a file's encrypted bytes, its key and its size.
pub(crate) struct MegaDownload {
    /// The temporary address; `<address>/<start>-<end>` fetches a byte range.
    pub(crate) address: String,
    pub(crate) key: FileKey,
    pub(crate) size: Option<u64>,
}

pub(crate) fn open_download(url: &str) -> Result<MegaDownload, String> {
    match parse(url)? {
        MegaRef::File { handle, key } => {
            let (address, size) = download_address(&api(json!({ "a": "g", "g": 1, "ssl": 2, "p": handle }), None)?)?;
            Ok(MegaDownload { address, key, size })
        }
        MegaRef::FolderFile { handle, key, node } => {
            let (file_key, _, node_size) = folder_file(&handle, &key, &node)?;
            let (address, size) =
                download_address(&api(json!({ "a": "g", "g": 1, "ssl": 2, "n": node }), Some(&handle))?)?;
            Ok(MegaDownload { address, key: file_key, size: size.or(node_size) })
        }
        MegaRef::Folder { .. } => {
            Err("That's a MEGA folder — pick the .var inside it in Add download source.".to_string())
        }
    }
}

// ----------------------------------------------------------------------------
// Test helpers: the encrypting side of the schemes above.
// ----------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    pub(crate) fn file_key(bytes: [u8; 32]) -> FileKey {
        FileKey(bytes)
    }

    pub(crate) fn download(address: String, key: FileKey, size: u64) -> MegaDownload {
        MegaDownload { address, key, size: Some(size) }
    }

    pub(crate) fn encrypt_name(aes_key: &[u8; 16], name: &str) -> String {
        let mut data = format!(r#"MEGA{{"n":"{name}"}}"#).into_bytes();
        while data.len() % 16 != 0 {
            data.push(0);
        }
        let cipher = aes(aes_key);
        let mut prev = [0u8; 16];
        for chunk in data.chunks_mut(16) {
            for (b, p) in chunk.iter_mut().zip(prev.iter()) {
                *b ^= p;
            }
            cipher.encrypt_block(GenericArray::from_mut_slice(chunk));
            prev.copy_from_slice(chunk);
        }
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data)
    }

    pub(crate) fn wrap_key(folder_key: &[u8; 16], key: &[u8]) -> String {
        let mut out = key.to_vec();
        let cipher = aes(folder_key);
        for block in out.chunks_mut(16) {
            cipher.encrypt_block(GenericArray::from_mut_slice(block));
        }
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(out)
    }

    pub(crate) fn aes_key_of(key: &FileKey) -> [u8; 16] {
        key.aes_key()
    }

    pub(crate) fn unwrap(folder_key: &[u8; 16], k: &str, at: &str, is_file: bool) -> Option<String> {
        unwrap_node_key(folder_key, k, at, is_file).map(|(_, name)| name)
    }
}
