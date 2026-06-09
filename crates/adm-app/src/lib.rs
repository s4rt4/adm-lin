//! adm-app — proses resident ADM (plan §4). Pustaka + binary.
//!
//! Modul `engine` & `ipc_server` bebas-Win32 (bisa ditest tanpa GUI);
//! `gui`/`state`/`tray`/`autostart`/`single_instance` adalah lapisan Windows.

pub mod autostart;
pub mod category;
pub mod dialogs;
pub mod engine;
pub mod gui;
pub mod ipc_server;
pub mod options;
pub mod progress;
pub mod scheduler;
pub mod settings;
pub mod single_instance;
pub mod state;
pub mod store;
pub mod tasks;
pub mod theme;

use std::path::PathBuf;

/// Folder unduhan default (plan §10; pemetaan kategori penuh = WM6).
fn default_download_dir() -> PathBuf {
    let base = std::env::var("USERPROFILE").unwrap_or_else(|_| ".".into());
    let dir = PathBuf::from(base).join("Downloads");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Entry point aplikasi.
pub fn run() {
    std::panic::set_hook(Box::new(|info| {
        eprintln!("[PANIC] {info}");
    }));
    let args: Vec<String> = std::env::args().skip(1).collect();

    // Mode CLI autostart (untuk Options/test; toggle utama via tray).
    if args.first().map(String::as_str) == Some("--autostart") {
        match args.get(1).map(String::as_str).unwrap_or("status") {
            "on" => {
                autostart::set(true);
                println!("autostart: on");
            }
            "off" => {
                autostart::set(false);
                println!("autostart: off");
            }
            _ => println!("autostart: {}", if autostart::is_enabled() { "on" } else { "off" }),
        }
        return;
    }

    let start_hidden = args.iter().any(|a| a == "--tray");

    // Single instance: instance kedua aktifkan jendela lalu keluar.
    let _mutex_guard = match single_instance::acquire() {
        single_instance::Acquire::First(h) => h,
        single_instance::Acquire::Already => {
            single_instance::activate_existing();
            return;
        }
    };

    // Runtime tokio + engine in-process; pipe server di thread yang menggerakkan
    // runtime, UI thread memegang message loop (plan §4).
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("gagal membangun runtime tokio");
    // Muat pengaturan persist & terapkan.
    let cfg = settings::load();
    let dl_dir = cfg
        .download_dir
        .clone()
        .map(PathBuf::from)
        .unwrap_or_else(default_download_dir);
    let _ = std::fs::create_dir_all(&dl_dir);
    let engine = engine::EngineHandle::new(rt.handle().clone(), dl_dir, gui::make_sink());
    engine.set_queue_max(cfg.queue_max.max(1));
    engine.set_global_limit(cfg.global_limit_kbps.saturating_mul(1024));
    if cfg.autostart != autostart::is_enabled() {
        autostart::set(cfg.autostart);
    }
    // Pulihkan daftar unduhan yang persist; setel id engine agar tak bentrok.
    let max_id = store::load();
    engine.reserve_ids(max_id + 1);
    gui::set_engine(engine.clone());
    scheduler::start(engine.clone()); // timer pemicu start/stop queue (§9.15)

    std::thread::Builder::new()
        .name("adm-ipc".into())
        .spawn(move || {
            rt.block_on(async move {
                if let Err(e) = ipc_server::serve(engine).await {
                    eprintln!("[ipc] pipe server berhenti: {e}");
                }
            });
        })
        .expect("gagal spawn thread ipc");

    if let Err(e) = gui::run(start_hidden) {
        eprintln!("[gui] error: {e}");
        std::process::exit(1);
    }
}
