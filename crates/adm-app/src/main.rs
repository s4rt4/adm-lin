//! adm-app — proses resident ADM (plan §4).
//!
//! WM0: jendela Win32 kosong + message loop + pipe server yang menjawab
//! `daemon.ping`. Engine in-process (WM1), tray + single-instance (WM2),
//! dan GUI lengkap (WM3+) menyusul.
//
// Catatan: subsystem masih "console" agar log pipe server terlihat saat WM0.
// Akan diganti `#![windows_subsystem = "windows"]` di WM2/WM7.

mod gui;
mod ipc_server;

fn main() {
    // Pipe server jalan di runtime tokio pada thread terpisah; UI thread
    // memegang message loop Win32 (plan §4 "Threading GUI").
    std::thread::Builder::new()
        .name("adm-ipc".into())
        .spawn(|| {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("gagal membangun runtime tokio");
            if let Err(e) = rt.block_on(ipc_server::serve()) {
                eprintln!("[ipc] pipe server berhenti: {e}");
            }
        })
        .expect("gagal spawn thread ipc");

    if let Err(e) = gui::run() {
        eprintln!("[gui] error: {e}");
        std::process::exit(1);
    }
}
