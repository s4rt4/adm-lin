//! Setelan aplikasi yang persist (tema, folder unduhan, antrian, limit) di
//! `$XDG_DATA_HOME/adm/settings.json` (sejajar `downloads.json`). Pengganti
//! `%APPDATA%\ADM\settings.json` versi Windows.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Settings {
    /// `true` = tema gelap (One Dark); `false` = terang.
    pub dark: bool,
    /// Folder unduhan kustom (None = default XDG ~/Downloads).
    pub download_dir: Option<String>,
    /// Batas unduhan antrian bersamaan.
    pub queue_max: usize,
    /// Batas kecepatan global (KB/s; 0 = tanpa batas).
    pub limit_kbps: u64,
    /// Tampilkan notifikasi desktop saat unduhan selesai.
    pub notify_complete: bool,
    /// Post-action: buka berkas otomatis tiap unduhan selesai.
    pub post_open: bool,
    /// Post-action: perintah shell dijalankan tiap unduhan selesai
    /// (`{}` diganti path berkas; kosong = nonaktif).
    pub post_run_cmd: String,
    /// Post-action saat SELURUH antrian selesai:
    /// `"none"` | `"shutdown"` | `"hibernate"` | `"sleep"` | `"exit"`.
    pub post_all_action: String,
    /// Pantau clipboard; tawarkan dialog Add saat URL berkas disalin.
    pub monitor_clipboard: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            dark: false,
            download_dir: None,
            queue_max: 1,
            limit_kbps: 0,
            notify_complete: true,
            post_open: false,
            post_run_cmd: String::new(),
            post_all_action: "none".to_string(),
            monitor_clipboard: false,
        }
    }
}

fn settings_file() -> PathBuf {
    crate::store::data_dir().join("settings.json")
}

/// Muat setelan dari disk (atau default bila absen/rusak).
pub fn load() -> Settings {
    std::fs::read(settings_file())
        .ok()
        .and_then(|b| serde_json::from_slice::<Settings>(&b).ok())
        .unwrap_or_default()
}

/// Simpan setelan secara atomik (.tmp + rename).
pub fn save(s: &Settings) {
    let file = settings_file();
    if let Some(parent) = file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_vec_pretty(s) {
        let tmp = file.with_extension("json.tmp");
        if std::fs::write(&tmp, &json).is_ok() {
            let _ = std::fs::rename(&tmp, &file);
        }
    }
}
