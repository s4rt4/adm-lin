//! Clipboard monitor (gaya IDM): thread latar memantau clipboard sistem; saat
//! URL berkas unduhan disalin, URL ditaruh di `pending` dan UI di-repaint untuk
//! menawarkan dialog Add. Toggle via `enabled` (setelan Options). Polling ~1 dtk
//! memakai satu instance `arboard::Clipboard` yang dipakai ulang.

use eframe::egui;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub struct ClipMonitor {
    enabled: Arc<AtomicBool>,
    pending: Arc<Mutex<Option<String>>>,
}

impl ClipMonitor {
    /// Mulai thread pemantau. `ctx` dipakai untuk `request_repaint` saat ada
    /// kandidat baru; `enabled` menyalakan/mematikan tanpa mematikan thread.
    pub fn start(ctx: egui::Context, enabled: bool) -> Self {
        let enabled = Arc::new(AtomicBool::new(enabled));
        let pending = Arc::new(Mutex::new(None));
        let (en, pen) = (enabled.clone(), pending.clone());
        std::thread::spawn(move || run(ctx, en, pen));
        Self { enabled, pending }
    }

    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Relaxed);
    }

    /// Ambil URL kandidat yang tertunda (sekali pakai).
    pub fn take(&self) -> Option<String> {
        self.pending.lock().unwrap().take()
    }
}

fn run(ctx: egui::Context, enabled: Arc<AtomicBool>, pending: Arc<Mutex<Option<String>>>) {
    let mut cb = arboard::Clipboard::new().ok();
    // Abaikan apa pun yang sudah ada di clipboard saat start (jangan memicu
    // dialog untuk teks lama begitu monitor dinyalakan).
    let mut last = cb.as_mut().and_then(|c| c.get_text().ok()).unwrap_or_default();
    loop {
        std::thread::sleep(Duration::from_millis(1000));
        if cb.is_none() {
            cb = arboard::Clipboard::new().ok();
        }
        let Some(c) = cb.as_mut() else { continue };
        let Ok(text) = c.get_text() else { continue };
        if text == last {
            continue;
        }
        // Selalu perbarui `last` (juga saat nonaktif) agar menyalakan monitor tak
        // memicu konten lama; emit hanya bila aktif & berupa URL unduhan.
        last = text.clone();
        if !enabled.load(Ordering::Relaxed) {
            continue;
        }
        if let Some(url) = crate::tasks::clipboard_download_url(&text) {
            *pending.lock().unwrap() = Some(url);
            ctx.request_repaint();
        }
    }
}
