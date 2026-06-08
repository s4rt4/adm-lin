//! Sidecar `.adm` — state resume tahan-crash (plan §7).
//!
//! Disimpan di sebelah file output (`<file>.adm`). Berisi URL, total, validator
//! (ETag/Last-Modified), dan progres per-segmen. Ditulis atomik (tmp + rename).

use crate::probe::Probe;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sidecar {
    pub url: String,
    pub total: u64,
    #[serde(default)]
    pub etag: Option<String>,
    #[serde(default)]
    pub last_modified: Option<String>,
    pub segments: Vec<SegRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegRecord {
    pub start: u64,
    /// inklusif.
    pub end: u64,
    pub downloaded: u64,
}

/// Path sidecar untuk file output tertentu.
pub fn path_for(output: &Path) -> PathBuf {
    let mut s = output.as_os_str().to_os_string();
    s.push(".adm");
    PathBuf::from(s)
}

/// Muat sidecar bila ada & valid JSON.
pub fn load(path: &Path) -> Option<Sidecar> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Tulis sidecar secara atomik.
pub fn save(path: &Path, sc: &Sidecar) -> std::io::Result<()> {
    let tmp = {
        let mut s = path.as_os_str().to_os_string();
        s.push(".tmp");
        PathBuf::from(s)
    };
    let data = serde_json::to_vec(sc).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, &data)?;
    std::fs::rename(&tmp, path)?; // MoveFileEx REPLACE_EXISTING di Windows
    Ok(())
}

pub fn remove(path: &Path) {
    let _ = std::fs::remove_file(path);
}

impl Sidecar {
    /// Apakah sidecar masih cocok dengan kondisi server saat ini (resume aman).
    pub fn is_compatible(&self, url: &str, probe: &Probe) -> bool {
        if self.url != url {
            return false;
        }
        if Some(self.total) != probe.total {
            return false;
        }
        // Validator: bila kedua sisi punya ETag, harus sama; idem Last-Modified.
        if let (Some(a), Some(b)) = (&self.etag, &probe.etag) {
            if a != b {
                return false;
            }
        }
        if let (Some(a), Some(b)) = (&self.last_modified, &probe.last_modified) {
            if a != b {
                return false;
            }
        }
        true
    }
}
