//! State bersama UI <-> engine + pesan kustom untuk marshalling ke UI thread.

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// Klik/aksi tray (uCallbackMessage).
pub const WM_TRAY: u32 = WM_APP + 1;
/// Progres/daftar unduhan berubah → UI refresh.
pub const WM_PROGRESS: u32 = WM_APP + 2;
/// Instance kedua minta jendela dimunculkan.
pub const WM_ACTIVATE_APP: u32 = WM_APP + 3;

pub static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);
pub static TOOLBAR_HWND: AtomicIsize = AtomicIsize::new(0);
pub static TREE_HWND: AtomicIsize = AtomicIsize::new(0);
pub static LIST_HWND: AtomicIsize = AtomicIsize::new(0);
pub static STATUS_HWND: AtomicIsize = AtomicIsize::new(0);

/// Sidebar kategori terlihat (View ▸ Hide categories).
pub static SIDEBAR_VISIBLE: AtomicBool = AtomicBool::new(true);

fn to_hwnd(v: isize) -> Option<HWND> {
    if v == 0 {
        None
    } else {
        Some(HWND(v as *mut core::ffi::c_void))
    }
}

pub fn store_hwnd(slot: &AtomicIsize, hwnd: HWND) {
    slot.store(hwnd.0 as isize, Ordering::SeqCst);
}

pub fn load_hwnd(slot: &AtomicIsize) -> Option<HWND> {
    to_hwnd(slot.load(Ordering::SeqCst))
}

/// Kirim pesan kustom ke UI thread (aman dari thread mana pun).
pub fn post_to_ui(msg: u32) {
    if let Some(hwnd) = load_hwnd(&MAIN_HWND) {
        unsafe {
            let _ = PostMessageW(Some(hwnd), msg, WPARAM(0), LPARAM(0));
        }
    }
}
