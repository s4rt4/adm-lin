//! adm-app — binary tipis; semua orkestrasi ada di pustaka (`adm_app::run`).
//
// Release: GUI tanpa console. Debug: tetap console agar log engine/pipe terlihat.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    adm_app::run();
}
