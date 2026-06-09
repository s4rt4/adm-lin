//! Model daftar unduhan untuk ListView (plan §9.9). Diperbarui dari event
//! engine (thread tokio), dibaca WndProc (UI thread) — dilindungi Mutex.

use crate::category::Category;
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Queued,
    Downloading,
    Complete,
    Paused,
    Error,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::Queued => "Queued",
            Status::Downloading => "Downloading",
            Status::Complete => "Complete",
            Status::Paused => "Stopped",
            Status::Error => "Error",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Row {
    pub id: u64,
    pub url: String,
    pub output: PathBuf,
    pub name: String,
    pub size: Option<u64>,
    pub downloaded: u64,
    pub speed_bps: u64,
    pub status: Status,
    /// (start, end, downloaded) per segmen/koneksi — untuk SegmentBar (§9.11).
    pub segments: Vec<(u64, u64, u64)>,
    /// Dialog "Download complete" sudah ditampilkan untuk baris ini.
    pub complete_announced: bool,
    pub category: Category,
}

impl Row {
    pub fn filename(&self) -> String {
        self.output
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.name.clone())
    }

    /// Estimasi sisa waktu (detik) bila sedang mengunduh.
    pub fn eta_secs(&self) -> Option<u64> {
        if self.status != Status::Downloading || self.speed_bps == 0 {
            return None;
        }
        self.size
            .map(|t| t.saturating_sub(self.downloaded) / self.speed_bps.max(1))
    }
}

static ROWS: Mutex<Vec<Row>> = Mutex::new(Vec::new());

/// Akses baca (untuk refresh ListView).
pub fn with_rows<R>(f: impl FnOnce(&[Row]) -> R) -> R {
    let guard = ROWS.lock().unwrap();
    f(&guard)
}

pub fn len() -> usize {
    ROWS.lock().unwrap().len()
}

pub fn id_at(index: usize) -> Option<u64> {
    ROWS.lock().unwrap().get(index).map(|r| r.id)
}

pub fn active_count() -> usize {
    ROWS.lock()
        .unwrap()
        .iter()
        .filter(|r| r.status == Status::Downloading)
        .count()
}

/// Tambah/perbarui baris saat unduhan dimulai (keyed by id → resume tak duplikat).
pub fn on_started(id: u64, url: String, output: PathBuf) {
    let mut rows = ROWS.lock().unwrap();
    let name = output
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let category = Category::from_filename(&name);
    if let Some(r) = rows.iter_mut().find(|r| r.id == id) {
        r.url = url;
        r.output = output;
        r.name = name;
        r.category = category;
        r.status = Status::Downloading;
    } else {
        rows.push(Row {
            id,
            url,
            output,
            name,
            size: None,
            downloaded: 0,
            speed_bps: 0,
            status: Status::Downloading,
            segments: Vec::new(),
            complete_announced: false,
            category,
        });
    }
}

/// Tambahkan baris berstatus Queued (Download Later).
pub fn on_queued(id: u64, url: String, output: PathBuf) {
    let mut rows = ROWS.lock().unwrap();
    if rows.iter().any(|r| r.id == id) {
        return;
    }
    let name = output
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let category = Category::from_filename(&name);
    rows.push(Row {
        id,
        url,
        output,
        name,
        size: None,
        downloaded: 0,
        speed_bps: 0,
        status: Status::Queued,
        segments: Vec::new(),
        complete_announced: false,
        category,
    });
}

/// Baris yang baru selesai & belum ditampilkan dialog "Download complete";
/// menandainya sudah diumumkan dan mengembalikan salinannya.
pub fn take_newly_completed() -> Vec<Row> {
    let mut out = Vec::new();
    for r in ROWS.lock().unwrap().iter_mut() {
        if r.status == Status::Complete && !r.complete_announced {
            r.complete_announced = true;
            out.push(r.clone());
        }
    }
    out
}

pub fn on_progress(
    id: u64,
    downloaded: u64,
    total: Option<u64>,
    speed_bps: u64,
    segments: Vec<(u64, u64, u64)>,
) {
    if let Some(r) = ROWS.lock().unwrap().iter_mut().find(|r| r.id == id) {
        r.downloaded = downloaded;
        if total.is_some() {
            r.size = total;
        }
        r.speed_bps = speed_bps;
        if !segments.is_empty() {
            r.segments = segments;
        }
        r.status = Status::Downloading;
    }
}

pub fn set_status(id: u64, status: Status) {
    if let Some(r) = ROWS.lock().unwrap().iter_mut().find(|r| r.id == id) {
        r.status = status;
        r.speed_bps = 0;
    }
}

/// Hapus baris; kembalikan baris yang dihapus (untuk hapus file bila perlu).
pub fn remove(id: u64) -> Option<Row> {
    let mut rows = ROWS.lock().unwrap();
    rows.iter().position(|r| r.id == id).map(|pos| rows.remove(pos))
}

/// Hapus semua baris berstatus Complete; kembalikan jumlah yang dihapus.
pub fn remove_completed() -> usize {
    let mut rows = ROWS.lock().unwrap();
    let before = rows.len();
    rows.retain(|r| r.status != Status::Complete);
    before - rows.len()
}

pub fn get(id: u64) -> Option<Row> {
    ROWS.lock().unwrap().iter().find(|r| r.id == id).cloned()
}

/// Perbarui path output + nama + kategori (mis. setelah koreksi nama).
pub fn set_output(id: u64, output: PathBuf) {
    if let Some(r) = ROWS.lock().unwrap().iter_mut().find(|r| r.id == id) {
        let name = output
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| r.name.clone());
        r.category = Category::from_filename(&name);
        r.name = name;
        r.output = output;
    }
}

/// Pindahkan baris ke kategori lain (output baru sudah dihitung pemanggil).
pub fn move_category(id: u64, output: PathBuf, category: Category) {
    if let Some(r) = ROWS.lock().unwrap().iter_mut().find(|r| r.id == id) {
        r.name = output
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| r.name.clone());
        r.output = output;
        r.category = category;
    }
}
