//! Scheduler: jalankan/hentikan antrian otomatis pada jam & hari tertentu
//! (port dari `adm-app/src/scheduler.rs`). State global persist ke
//! `$XDG_DATA_HOME/adm/scheduler.json` + thread pemeriksa waktu lokal tiap 20
//! detik (edge-trigger start/stop queue). Waktu lokal via `chrono`.

use crate::engine::EngineHandle;
use chrono::{Datelike, Local, Timelike};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Schedule {
    pub enabled: bool,
    /// Jam mulai (jam, menit).
    pub start: (u8, u8),
    /// Jam berhenti (jam, menit).
    pub stop: (u8, u8),
    /// index 0=Minggu .. 6=Sabtu (cocok dgn chrono Weekday::num_days_from_sunday).
    pub days: [bool; 7],
}

impl Default for Schedule {
    fn default() -> Self {
        Self {
            enabled: false,
            start: (9, 0),
            stop: (18, 0),
            days: [true; 7],
        }
    }
}

static SCHEDULE: Mutex<Option<Schedule>> = Mutex::new(None);

fn schedule_file() -> PathBuf {
    crate::store::data_dir().join("scheduler.json")
}

/// Snapshot setelan scheduler terkini (memuat dari disk sekali bila perlu).
pub fn get() -> Schedule {
    let mut guard = SCHEDULE.lock().unwrap();
    if guard.is_none() {
        let loaded = std::fs::read(schedule_file())
            .ok()
            .and_then(|b| serde_json::from_slice::<Schedule>(&b).ok())
            .unwrap_or_default();
        *guard = Some(loaded);
    }
    guard.clone().unwrap()
}

/// Simpan setelan scheduler (memori + disk, tulis atomik).
pub fn set(s: Schedule) {
    *SCHEDULE.lock().unwrap() = Some(s.clone());
    let file = schedule_file();
    if let Some(parent) = file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_vec_pretty(&s) {
        let tmp = file.with_extension("json.tmp");
        if std::fs::write(&tmp, &json).is_ok() {
            let _ = std::fs::rename(&tmp, &file);
        }
    }
}

/// True bila waktu `(dow 0=Minggu, menit-sejak-tengah-malam)` berada dalam
/// jendela aktif jadwal `s` (menangani rentang yang melewati tengah malam).
fn is_active_at(s: &Schedule, dow: usize, now_min: u32) -> bool {
    if !s.days.get(dow).copied().unwrap_or(false) {
        return false;
    }
    let start = s.start.0 as u32 * 60 + s.start.1 as u32;
    let stop = s.stop.0 as u32 * 60 + s.stop.1 as u32;
    if start <= stop {
        now_min >= start && now_min < stop
    } else {
        now_min >= start || now_min < stop // melewati tengah malam
    }
}

/// Thread pemicu: cek tiap 20 detik, edge-trigger start/stop queue.
pub fn start(engine: EngineHandle) {
    std::thread::Builder::new()
        .name("adm-scheduler".into())
        .spawn(move || {
            let mut was_active = false;
            loop {
                std::thread::sleep(Duration::from_secs(20));
                let s = get();
                if !s.enabled {
                    was_active = false;
                    continue;
                }
                let now = Local::now();
                let dow = now.weekday().num_days_from_sunday() as usize;
                let now_min = now.hour() * 60 + now.minute();
                let active = is_active_at(&s, dow, now_min);
                if active && !was_active {
                    engine.start_queue();
                } else if !active && was_active {
                    engine.stop_queue();
                }
                was_active = active;
            }
        })
        .expect("spawn scheduler");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sched(start: (u8, u8), stop: (u8, u8)) -> Schedule {
        Schedule { enabled: true, start, stop, days: [true; 7] }
    }

    #[test]
    fn window_same_day() {
        let s = sched((9, 0), (18, 0));
        assert!(!is_active_at(&s, 1, 8 * 60));
        assert!(is_active_at(&s, 1, 9 * 60));
        assert!(is_active_at(&s, 1, 17 * 60 + 59));
        assert!(!is_active_at(&s, 1, 18 * 60));
    }

    #[test]
    fn window_overnight() {
        let s = sched((22, 0), (6, 0));
        assert!(is_active_at(&s, 3, 23 * 60));
        assert!(is_active_at(&s, 3, 1 * 60));
        assert!(!is_active_at(&s, 3, 12 * 60));
    }

    #[test]
    fn day_disabled() {
        let mut s = sched((0, 0), (23, 59));
        s.days[2] = false;
        assert!(!is_active_at(&s, 2, 12 * 60));
        assert!(is_active_at(&s, 3, 12 * 60));
    }
}
