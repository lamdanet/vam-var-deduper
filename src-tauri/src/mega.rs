//! MEGA file links. MEGA encrypts every file client-side: a link
//! `https://mega.nz/file/<handle>#<key>` carries the key after the `#`. The
//! public API (`a: "g"`) hands out a temporary download URL for the encrypted
//! bytes plus the encrypted attributes (the file name); both are decrypted
//! here with AES-128 — CTR for the content, CBC for the attributes.
//!
//! Folder links are not supported: their files' keys are wrapped in the folder
//! key, and the file's own link is one click away in MEGA's page.

use aes::cipher::{generic_array::GenericArray, BlockDecrypt, BlockEncrypt, KeyInit};
use aes::Aes128;
use anyhow::{anyhow, Result};
use base64::Engine;
use serde_json::{json, Value};

const API_URL: &str = "https://g.api.mega.co.nz/cs";

pub(crate) fn is_mega(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.contains("://mega.nz") || lower.contains("://mega.co.nz") || lower.contains("://www.mega.nz")
}

/// A parsed MEGA file link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MegaLink {
    pub(crate) handle: String,
    /// The 256-bit node key from the link.
    key: [u8; 32],
}

impl MegaLink {
    /// The AES-128 key: the two halves of the node key XORed together.
    fn aes_key(&self) -> [u8; 16] {
        let mut k = [0u8; 16];
        for (i, b) in k.iter_mut().enumerate() {
            *b = self.key[i] ^ self.key[i + 16];
        }
        k
    }

    /// The CTR nonce: the third quarter of the node key.
    fn nonce(&self) -> [u8; 8] {
        let mut n = [0u8; 8];
        n.copy_from_slice(&self.key[16..24]);
        n
    }

    pub(crate) fn decryptor(&self) -> CtrDecryptor {
        CtrDecryptor::new(self.aes_key(), self.nonce())
    }
}

/// Parses `mega.nz/file/<handle>#<key>` and the older `mega.nz/#!<handle>!<key>`.
pub(crate) fn parse_link(url: &str) -> Result<MegaLink, String> {
    let url = url.trim();
    if url.contains("/folder/") || url.contains("#F!") {
        return Err(
            "MEGA folder links aren't supported — open the folder in your browser and copy the link of the .var file itself."
                .to_string(),
        );
    }
    let (handle, key) = if let Some(pos) = url.find("/file/") {
        let rest = &url[pos + "/file/".len()..];
        let (handle, key) = rest.split_once('#').ok_or_else(missing_key)?;
        (handle, key)
    } else if let Some(pos) = url.find("#!") {
        let rest = &url[pos + 2..];
        rest.split_once('!').ok_or_else(missing_key)?
    } else {
        return Err("That isn't a MEGA file link (expected mega.nz/file/…#key).".to_string());
    };
    let handle: String = handle.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
    let key: String = key.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
    if handle.is_empty() {
        return Err("The MEGA link has no file id.".to_string());
    }
    if key.is_empty() {
        return Err(missing_key());
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(key.as_bytes())
        .map_err(|_| "The MEGA link's key is malformed.".to_string())?;
    let key: [u8; 32] = bytes
        .try_into()
        .map_err(|_| "The MEGA link's key has the wrong length — copy the whole link.".to_string())?;
    Ok(MegaLink { handle, key })
}

fn missing_key() -> String {
    "This MEGA link has no key (the part after #) — copy the full link, key included.".to_string()
}

/// What the API says about one file.
#[derive(Debug, Clone)]
pub(crate) struct MegaFile {
    pub(crate) download_url: String,
    pub(crate) size: Option<u64>,
    pub(crate) name: Option<String>,
}

fn request_body(link: &MegaLink) -> Value {
    json!([{ "a": "g", "g": 1, "ssl": 2, "p": link.handle }])
}

fn error_text(code: i64) -> String {
    match code {
        -9 => "MEGA: the file doesn't exist (removed, or a wrong link).".to_string(),
        -11 => "MEGA: access denied.".to_string(),
        -16 => "MEGA: the file was taken down.".to_string(),
        -17 | -4 => "MEGA: transfer quota exceeded — try again later, or open the link in your browser.".to_string(),
        -18 | -3 => "MEGA is temporarily unavailable — try again shortly.".to_string(),
        other => format!("MEGA API error {other}."),
    }
}

fn parse_response(link: &MegaLink, value: &Value) -> Result<MegaFile, String> {
    let first = match value {
        Value::Array(items) => items.first().cloned().unwrap_or(Value::Null),
        other => other.clone(),
    };
    if let Some(code) = first.as_i64() {
        return Err(error_text(code));
    }
    if let Some(code) = first.get("e").and_then(Value::as_i64) {
        return Err(error_text(code));
    }
    let download_url = first
        .get("g")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "MEGA gave no download address (transfer quota?).".to_string())?
        .to_string();
    let size = first.get("s").and_then(Value::as_u64);
    let name = first
        .get("at")
        .and_then(Value::as_str)
        .and_then(|at| decrypt_attributes(link, at));
    Ok(MegaFile { download_url, size, name })
}

/// The file name from the encrypted attributes: AES-CBC with a zero IV over
/// `MEGA{"n":"<name>",...}` zero-padded to the block size.
fn decrypt_attributes(link: &MegaLink, at: &str) -> Option<String> {
    let mut data = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(at.trim_end_matches('=').as_bytes())
        .ok()?;
    if data.is_empty() || data.len() % 16 != 0 {
        return None;
    }
    let cipher = Aes128::new(GenericArray::from_slice(&link.aes_key()));
    let mut prev = [0u8; 16];
    for chunk in data.chunks_mut(16) {
        let mut cur = [0u8; 16];
        cur.copy_from_slice(chunk);
        let block = GenericArray::from_mut_slice(chunk);
        cipher.decrypt_block(block);
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

/// Looks a file up with the blocking client (callers run on a plain thread).
pub(crate) fn file_info_blocking(link: &MegaLink) -> Result<MegaFile, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;
    let value: Value = client
        .post(format!("{API_URL}?id={}", request_id()))
        .json(&request_body(link))
        .send()
        .map_err(|e| format!("Couldn't reach MEGA: {e}"))?
        .json()
        .map_err(|e| format!("MEGA sent an unexpected answer: {e}"))?;
    parse_response(link, &value)
}

/// The same lookup for the async download path.
pub(crate) async fn file_info(client: &reqwest::Client, link: &MegaLink) -> Result<MegaFile> {
    let value: Value = client
        .post(format!("{API_URL}?id={}", request_id()))
        .json(&request_body(link))
        .send()
        .await
        .map_err(|e| anyhow!("Couldn't reach MEGA: {e}"))?
        .json()
        .await
        .map_err(|e| anyhow!("MEGA sent an unexpected answer: {e}"))?;
    parse_response(link, &value).map_err(|e| anyhow!(e))
}

fn request_id() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0)
}

/// AES-128-CTR over a byte stream, fed chunk by chunk in order. The counter
/// block is the 8-byte nonce followed by the big-endian 16-byte block index.
pub(crate) struct CtrDecryptor {
    cipher: Aes128,
    nonce: [u8; 8],
    offset: u64,
    keystream: [u8; 16],
}

impl CtrDecryptor {
    fn new(key: [u8; 16], nonce: [u8; 8]) -> Self {
        Self {
            cipher: Aes128::new(GenericArray::from_slice(&key)),
            nonce,
            offset: 0,
            keystream: [0; 16],
        }
    }

    pub(crate) fn apply(&mut self, data: &mut [u8]) {
        for byte in data.iter_mut() {
            let pos = (self.offset % 16) as usize;
            if pos == 0 {
                let mut block = [0u8; 16];
                block[..8].copy_from_slice(&self.nonce);
                block[8..].copy_from_slice(&(self.offset / 16).to_be_bytes());
                let ga = GenericArray::from_mut_slice(&mut block);
                self.cipher.encrypt_block(ga);
                self.keystream = block;
            }
            *byte ^= self.keystream[pos];
            self.offset += 1;
        }
    }
}

#[cfg(test)]
pub(crate) fn parse_response_for_test(link: &MegaLink, value: &Value) -> Result<MegaFile, String> {
    parse_response(link, value)
}

#[cfg(test)]
pub(crate) fn test_link(key: [u8; 32]) -> MegaLink {
    MegaLink { handle: "h".to_string(), key }
}

#[cfg(test)]
pub(crate) fn encrypt_attributes_for_test(link: &MegaLink, json: &str) -> String {
    let mut data = format!("MEGA{json}").into_bytes();
    while data.len() % 16 != 0 {
        data.push(0);
    }
    let cipher = Aes128::new(GenericArray::from_slice(&link.aes_key()));
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
