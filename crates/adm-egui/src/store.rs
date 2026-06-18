//! Persistensi daftar unduhan (M2). Daftar disimpan sebagai JSON di lokasi XDG
//! `$XDG_DATA_HOME/adm/downloads.json` (default `~/.local/share/adm/...`) —
//! pengganti `%APPDATA%\ADM\downloads.json` di versi Windows (`adm-app/store.rs`).
//!
//! Hanya field durabel yang ditulis; field transien (kecepatan, pesan error,
//! segmen) tak ikut. Saat startup daftar dipulihkan dan id terbesar dikembalikan
//! agar engine menyetel id berikutnya supaya tak bentrok (`reserve_ids`).

use crate::category::Category;
use crate::{Row, Status};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::SystemTime;

/// Subset baris yang dipersist. Field transien (speed_bps/error) tak disimpan;
/// dipulihkan ke nilai default saat dimuat.
#[derive(Serialize, Deserialize)]
struct PersistRow {
    id: u64,
    url: String,
    filename: String,
    category: Category,
    downloaded: u64,
    total: Option<u64>,
    status: Status,
    #[serde(default = "SystemTime::now")]
    last_try: SystemTime,
}

impl From<&Row> for PersistRow {
    fn from(r: &Row) -> Self {
        Self {
            id: r.id,
            url: r.url.clone(),
            filename: r.filename.clone(),
            category: r.category,
            downloaded: r.downloaded,
            total: r.total,
            status: r.status,
            last_try: r.last_try,
        }
    }
}

impl PersistRow {
    fn into_row(self) -> Row {
        // Tak ada yang berjalan saat startup → "Downloading" jadi Paused (Stopped).
        let status = match self.status {
            Status::Active => Status::Paused,
            s => s,
        };
        Row {
            id: self.id,
            url: self.url,
            filename: self.filename,
            category: self.category,
            downloaded: self.downloaded,
            total: self.total,
            speed_bps: 0,
            status,
            error: None,
            last_try: self.last_try,
            segments: Vec::new(),
        }
    }
}

/// `$XDG_DATA_HOME/adm/downloads.json` (default `~/.local/share/adm/...`).
fn store_file() -> PathBuf {
    data_dir().join("downloads.json")
}

pub(crate) fn data_dir() -> PathBuf {
    let base = std::env::var("XDG_DATA_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            PathBuf::from(home).join(".local").join("share")
        });
    base.join("adm")
}

/// Serialisasi penulisan berkas (save bisa terpicu dari beberapa tempat).
static SAVE_LOCK: Mutex<()> = Mutex::new(());

/// Simpan daftar unduhan ke disk secara atomik (tulis `.tmp` lalu rename).
/// Dipanggil pada perubahan struktural (tambah/hapus/ubah status).
pub fn save(rows: &[Row]) {
    let snapshot: Vec<PersistRow> = rows.iter().map(PersistRow::from).collect();
    let _guard = SAVE_LOCK.lock().unwrap();
    let file = store_file();
    if let Some(parent) = file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_vec_pretty(&snapshot) {
        let tmp = file.with_extension("json.tmp");
        if std::fs::write(&tmp, &json).is_ok() {
            let _ = std::fs::rename(&tmp, &file);
        }
    }
}

/// Muat daftar unduhan dari disk saat startup. Mengembalikan baris yang
/// dipulihkan + id terbesar yang dipakai (0 bila kosong/tak ada berkas).
pub fn load() -> (Vec<Row>, u64) {
    let Ok(bytes) = std::fs::read(store_file()) else {
        return (Vec::new(), 0);
    };
    let Ok(persisted) = serde_json::from_slice::<Vec<PersistRow>>(&bytes) else {
        return (Vec::new(), 0);
    };
    let mut max_id = 0;
    let rows: Vec<Row> = persisted
        .into_iter()
        .map(|p| {
            max_id = max_id.max(p.id);
            p.into_row()
        })
        .collect();
    (rows, max_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: u64, name: &str, status: Status) -> Row {
        Row {
            id,
            url: format!("https://example.com/{name}"),
            filename: name.into(),
            category: Category::from_filename(name),
            downloaded: 100,
            total: Some(1000),
            speed_bps: 9999, // transien — tak boleh ikut tersimpan
            status,
            error: Some("boom".into()), // transien
            last_try: SystemTime::UNIX_EPOCH,
            segments: Vec::new(),
        }
    }

    #[test]
    fn roundtrip_persists_durable_drops_transient_and_pauses_active() {
        let tmp = std::env::temp_dir().join(format!("adm-store-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        // SAFETY: test berjalan serial (satu test) — aman menyetel env proses.
        unsafe { std::env::set_var("XDG_DATA_HOME", &tmp) };

        let saved = vec![
            row(3, "movie.mkv", Status::Active),
            row(7, "doc.pdf", Status::Completed),
        ];
        save(&saved);

        let (loaded, max_id) = load();
        assert_eq!(max_id, 7);
        assert_eq!(loaded.len(), 2);

        // Active → Paused saat startup (tak ada yang berjalan).
        assert_eq!(loaded[0].status, Status::Paused);
        assert_eq!(loaded[1].status, Status::Completed);

        // Field durabel dipulihkan; transien direset.
        assert_eq!(loaded[0].filename, "movie.mkv");
        assert_eq!(loaded[0].category, Category::Video);
        assert_eq!(loaded[0].downloaded, 100);
        assert_eq!(loaded[0].speed_bps, 0);
        assert!(loaded[0].error.is_none());

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
