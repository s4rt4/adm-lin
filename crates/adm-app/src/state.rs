//! State bersama UI <-> engine + pesan kustom untuk marshalling ke UI thread.
//!
//! Engine berjalan di thread tokio; ia menulis atomics di sini lalu mem-post
//! pesan ke UI thread (plan §4 "Threading GUI"). WndProc membaca atomics ini.

use std::sync::atomic::{AtomicIsize, AtomicU64, AtomicUsize, Ordering};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// Klik/aksi tray (uCallbackMessage).
pub const WM_TRAY: u32 = WM_APP + 1;
/// Progres unduhan berubah → UI refresh judul.
pub const WM_PROGRESS: u32 = WM_APP + 2;
/// Instance kedua minta jendela dimunculkan.
pub const WM_ACTIVATE_APP: u32 = WM_APP + 3;

/// HWND jendela utama (0 = belum dibuat). Disimpan sbg isize agar atomik.
pub static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);

pub static DOWNLOADED: AtomicU64 = AtomicU64::new(0);
pub static TOTAL: AtomicU64 = AtomicU64::new(0);
pub static SPEED: AtomicU64 = AtomicU64::new(0);
pub static ACTIVE: AtomicUsize = AtomicUsize::new(0);

pub fn set_main_hwnd(hwnd: HWND) {
    MAIN_HWND.store(hwnd.0 as isize, Ordering::SeqCst);
}

fn main_hwnd() -> Option<HWND> {
    let h = MAIN_HWND.load(Ordering::SeqCst);
    if h == 0 {
        None
    } else {
        Some(HWND(h as *mut core::ffi::c_void))
    }
}

/// Kirim pesan kustom ke UI thread (aman dipanggil dari thread mana pun).
pub fn post_to_ui(msg: u32) {
    if let Some(hwnd) = main_hwnd() {
        unsafe {
            let _ = PostMessageW(Some(hwnd), msg, WPARAM(0), LPARAM(0));
        }
    }
}
