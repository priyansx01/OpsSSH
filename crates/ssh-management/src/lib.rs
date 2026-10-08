//! Authorized-key documents are byte-preserving; remote writes use compare-and-replace.
use base64::{Engine, engine::general_purpose::STANDARD};
use opsssh_ssh_core::{ExecOutput, ExecRequest, SecretString};
use russh::keys::{HashAlg, PublicKey};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::Duration;

pub const MAX_DOCUMENT: usize = 1024 * 1024;
pub const HELPER: &str = include_str!("helper.py");
#[derive(Debug, Clone)]
pub struct KeyEntry {
    pub line: usize,
    pub algorithm: String,
    pub fingerprint: String,
    pub comment: String,
    pub restrictions: String,
    pub public_key: String,
    comment_offset: usize,
}
#[derive(Debug, Clone)]
pub struct Registry {
    pub account: String,
    pub path: String,
    pub uid: u64,
    pub revision: String,
    pub entries: Vec<KeyEntry>,
    pub unrecognized: usize,
    pub writable: bool,
    pub can_create: bool,
    raw: Vec<u8>,
}
fn fields(line: &str) -> Vec<(usize, usize)> {
    let mut result = Vec::new();
    let (mut start, mut quoted, mut escape) = (None, false, false);
    for (index, ch) in line.char_indices() {
        if escape {
            escape = false;
            continue;
        }
        if ch == '\\' && quoted {
            escape = true;
            continue;
        }
        if ch == '"' {
            quoted = !quoted;
        }
        if ch.is_ascii_whitespace() && !quoted {
            if let Some(begin) = start.take() {
                result.push((begin, index));
            }
            if result.len() == 3 {
                break;
            }
        } else if start.is_none() {
            start = Some(index);
        }
    }
    if let Some(begin) = start {
        result.push((begin, line.len()));
    }
    result
}
fn parse_entry(line: &[u8], index: usize) -> Option<KeyEntry> {
    let text = std::str::from_utf8(line)
        .ok()?
        .trim_end_matches(['\r', '\n']);
    if text.trim_start().starts_with('#') || text.trim().is_empty() {
        return None;
    }
    let tokens = fields(text);
    let is_type = |value: &str| {
        value.starts_with("ssh-") || value.starts_with("ecdsa-") || value.starts_with("sk-")
    };
    let first = *tokens.first()?;
    let key_token = if is_type(&text[first.0..first.1]) {
        0
    } else {
        1
    };
    let kind = *tokens.get(key_token)?;
    let blob = *tokens.get(key_token + 1)?;
    let key = PublicKey::from_openssh(&text[kind.0..]).ok()?;
    let offset = text[blob.1..]
        .find(|c: char| !c.is_ascii_whitespace())
        .map(|i| blob.1 + i)
        .unwrap_or(text.len());
    Some(KeyEntry {
        line: index,
        algorithm: key.algorithm().to_string(),
        fingerprint: key.fingerprint(HashAlg::Sha256).to_string(),
        comment: text[offset..].to_string(),
        restrictions: text[..kind.0].trim().to_string(),
        public_key: key.to_openssh().ok()?,
        comment_offset: blob.1,
    })
}
pub fn public_key(value: &str) -> Result<KeyEntry, String> {
    let value = value.trim();
    if value.len() > 8192 || value.contains(['\r', '\n', '\0']) || value.contains("PRIVATE KEY") {
        return Err("Enter one public key, not a private key or multiple lines".into());
    }
    let entry = parse_entry(value.as_bytes(), 0).ok_or("Invalid or unsupported SSH public key")?;
    if !entry.restrictions.is_empty() {
        return Err(
            "Import a plain .pub key; existing registry restrictions are preserved separately"
                .into(),
        );
    }
    Ok(entry)
}
pub fn revision(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
impl Registry {
    pub fn from_document(
        account: &str,
        path: &str,
        uid: u64,
        bytes: &[u8],
    ) -> Result<Self, String> {
        Self::from_reply(
            &json!({"account":account,"path":path,"uid":uid,"content":STANDARD.encode(bytes),"revision":revision(bytes),"writable":true,"can_create":true}),
        )
    }
    pub fn from_reply(value: &Value) -> Result<Self, String> {
        let raw = STANDARD
            .decode(
                value["content"]
                    .as_str()
                    .ok_or("Missing registry content")?,
            )
            .map_err(|_| "Invalid registry encoding")?;
        if raw.len() > MAX_DOCUMENT {
            return Err("Authorized keys file exceeds the 1 MiB management limit".into());
        }
        let mut entries = Vec::new();
        let mut unrecognized = 0;
        for (index, line) in raw.split_inclusive(|b| *b == b'\n').enumerate() {
            if let Some(entry) = parse_entry(line, index) {
                entries.push(entry);
            } else if !line.iter().all(u8::is_ascii_whitespace)
                && !line
                    .iter()
                    .find(|b| !b.is_ascii_whitespace())
                    .is_some_and(|b| *b == b'#')
            {
                unrecognized += 1;
            }
        }
        let string = |key| {
            value[key]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("Missing {key}"))
        };
        let hash = revision(&raw);
        if value["revision"].as_str() != Some(hash.as_str()) {
            return Err("Registry revision did not match its content".into());
        }
        Ok(Self {
            account: string("account")?,
            path: string("path")?,
            uid: value["uid"].as_u64().ok_or("Missing account UID")?,
            revision: hash,
            entries,
            unrecognized,
            writable: value["writable"].as_bool().unwrap_or(false),
            can_create: value["can_create"].as_bool().unwrap_or(false),
            raw,
        })
    }
    pub fn add(&self, input: &str) -> Result<Vec<u8>, String> {
        let key = public_key(input)?;
        if self
            .entries
            .iter()
            .any(|entry| entry.fingerprint == key.fingerprint)
        {
            return Err("This public key already exists in the registry".into());
        }
        let mut bytes = self.raw.clone();
        if !bytes.is_empty() && !bytes.ends_with(b"\n") {
            bytes.push(b'\n');
        }
        bytes.extend_from_slice(key.public_key.as_bytes());
        bytes.push(b'\n');
        if bytes.len() > MAX_DOCUMENT {
            return Err("Authorized keys file would exceed 1 MiB".into());
        }
        Ok(bytes)
    }
    pub fn change(
        &self,
        entry: &KeyEntry,
        comment: Option<&str>,
        active: Option<&str>,
    ) -> Result<Vec<u8>, String> {
        if active == Some(entry.fingerprint.as_str()) {
            return Err(
                "Reconnect using another key or password before changing the current login key"
                    .into(),
            );
        }
        if comment.is_some_and(|text| text.len() > 1024 || text.chars().any(char::is_control)) {
            return Err("Use a single-line comment of up to 1024 bytes".into());
        }
        let original = self
            .entries
            .iter()
            .find(|row| row.line == entry.line && row.fingerprint == entry.fingerprint)
            .ok_or("Key is no longer in this snapshot")?;
        let mut bytes = Vec::new();
        for (index, line) in self.raw.split_inclusive(|b| *b == b'\n').enumerate() {
            if index != original.line {
                bytes.extend_from_slice(line);
                continue;
            }
            if let Some(comment) = comment {
                bytes.extend_from_slice(&line[..original.comment_offset]);
                if !comment.trim().is_empty() {
                    bytes.push(b' ');
                    bytes.extend_from_slice(comment.trim().as_bytes());
                }
                if line.ends_with(b"\r\n") {
                    bytes.extend_from_slice(b"\r\n");
                } else if line.ends_with(b"\n") {
                    bytes.push(b'\n');
                }
            }
        }
        if bytes.len() > MAX_DOCUMENT {
            return Err("Authorized keys file would exceed 1 MiB".into());
        }
        Ok(bytes)
    }
    pub fn replacement(&self, bytes: Vec<u8>) -> Value {
        json!({"op":"replace","account":self.account,"expected":self.revision,"content":STANDARD.encode(bytes)})
    }
}
pub fn request(payload: Value, sudo: bool, password: Option<&str>) -> Result<ExecRequest, String> {
    if password.is_some_and(|value| value.len() > 8192 || value.contains(['\r', '\n', '\0'])) {
        return Err("The sudo password must not contain a newline".into());
    }
    // This embedded, trusted script needs multiline shell quoting; path quoting rejects newlines.
    let script = HELPER.replace('\'', "'\"'\"'");
    let command = format!(
        "{}python3 -c '{script}'",
        if sudo {
            if password.is_some_and(|p| !p.is_empty()) {
                "sudo -S -p '' -- "
            } else {
                "sudo -n -- "
            }
        } else {
            ""
        }
    );
    let prefix = password
        .filter(|p| sudo && !p.is_empty())
        .map(|p| format!("{p}\n"))
        .unwrap_or_default();
    let mut request = ExecRequest::new(command);
    request.stdin = Some(SecretString::new(format!(
        "{prefix}OPSSSH-MANAGEMENT-1\n{}",
        payload
    )));
    request.timeout = Duration::from_secs(60);
    Ok(request)
}
pub fn reply(output: ExecOutput) -> Result<Value, String> {
    let value = response(&output)?;
    if value["ok"].as_bool() != Some(true) {
        return Err(value["error"]
            .as_str()
            .unwrap_or("SSH management operation failed")
            .to_owned());
    }
    if output.exit_status != Some(0) {
        return Err(
            "Management command did not complete successfully; refresh before trying again".into(),
        );
    }
    Ok(value)
}
pub fn response(output: &ExecOutput) -> Result<Value, String> {
    if output.exit_status.is_none() {
        return Err("Management result is unknown. Refresh and verify the server before retrying; no action was replayed.".into());
    }
    serde_json::from_slice(&output.stdout).map_err(|_| "SSH management helper unavailable. Linux and Python 3 are required; sudo actions require administrator authorization.".into())
}
pub fn username(value: &str) -> bool {
    (1..=32).contains(&value.len())
        && value
            .as_bytes()
            .first()
            .is_some_and(|c| c.is_ascii_lowercase() || *c == b'_')
        && value
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_' || c == b'-')
}
