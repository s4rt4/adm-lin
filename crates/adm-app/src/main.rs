//! adm-app — binary tipis; semua orkestrasi ada di pustaka (`adm_app::run`).
//
// Subsystem masih "console" (lihat log engine/pipe saat WM2). Akan diganti
// `#![windows_subsystem = "windows"]` di WM7.

fn main() {
    adm_app::run();
}
