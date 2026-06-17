//! Autostart Linux via XDG Autostart (plan §3/§12). Pengganti `HKCU\...\Run`
//! di versi Windows: tulis/hapus `$XDG_CONFIG_HOME/autostart/adm-egui.desktop`
//! (default `~/.config/autostart/...`). Desktop environment menjalankan entri
//! ini saat login.

use std::path::PathBuf;

fn autostart_dir() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            PathBuf::from(home).join(".config")
        });
    base.join("autostart")
}

fn file() -> PathBuf {
    autostart_dir().join("adm-egui.desktop")
}

/// Apakah autostart aktif (entri .desktop ada).
pub fn is_enabled() -> bool {
    file().exists()
}

/// Aktif/nonaktifkan autostart. `true` bila operasi sukses.
pub fn set(enabled: bool) -> bool {
    if enabled {
        let exe = std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "adm-egui".into());
        let content = format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=Alpha Download Manager\n\
             Comment=Start ADM at login\n\
             Exec={exe}\n\
             Icon=adm\n\
             Terminal=false\n\
             Categories=Network;FileTransfer;\n\
             X-GNOME-Autostart-enabled=true\n"
        );
        let dir = autostart_dir();
        std::fs::create_dir_all(&dir).is_ok() && std::fs::write(file(), content).is_ok()
    } else {
        match std::fs::remove_file(file()) {
            Ok(()) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
            Err(_) => false,
        }
    }
}
