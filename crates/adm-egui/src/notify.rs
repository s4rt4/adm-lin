//! Notifikasi desktop saat unduhan selesai (toast gaya IDM). Memakai
//! `notify-rust` (D-Bus `org.freedesktop.Notifications`). Aksi Open / Open
//! folder ditangani di thread terpisah agar UI tak terblokir: klik default
//! membuka berkas, aksi "folder" membuka direktorinya — keduanya via xdg-open.
//!
//! Catatan reaktor: notify-rust default memakai zbus async-io (sama dgn tray
//! ksni), jadi aman dipanggil dari thread mana pun tanpa runtime tokio.

use std::path::{Path, PathBuf};

/// Tampilkan notifikasi "Download complete" untuk `path`. Non-blok: seluruh
/// interaksi D-Bus berjalan di thread terlepas (termasuk menunggu klik aksi).
pub fn completed(filename: &str, path: &Path) {
    let filename = filename.to_string();
    let path = path.to_path_buf();
    std::thread::spawn(move || show_completed(&filename, &path));
}

fn show_completed(filename: &str, path: &PathBuf) {
    let handle = notify_rust::Notification::new()
        .summary("Download complete")
        .body(filename)
        .icon("emblem-downloads")
        .appname("ADM")
        .action("default", "Open")
        .action("folder", "Open folder")
        .show();
    let Ok(handle) = handle else { return };
    // Blok di thread ini sampai notifikasi diklik/ditutup oleh daemon.
    handle.wait_for_action(|action| match action {
        "default" => open(path),
        "folder" => {
            if let Some(p) = path.parent() {
                open(p);
            }
        }
        _ => {}
    });
}

fn open(path: &Path) {
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
}
