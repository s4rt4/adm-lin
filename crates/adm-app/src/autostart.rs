//! Autostart via `HKCU\...\Run` (plan §3, §12). Toggle dari tray.

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SAM_FLAGS, REG_SZ,
};

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const VALUE_NAME: PCWSTR = w!("ADM");

fn open(access: REG_SAM_FLAGS) -> Option<HKEY> {
    let mut hkey = HKEY::default();
    let rc = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY, Some(0), access, &mut hkey) };
    if rc == ERROR_SUCCESS {
        Some(hkey)
    } else {
        None
    }
}

/// Apakah autostart aktif.
pub fn is_enabled() -> bool {
    let Some(hkey) = open(KEY_QUERY_VALUE) else {
        return false;
    };
    let rc = unsafe { RegQueryValueExW(hkey, VALUE_NAME, None, None, None, None) };
    unsafe {
        let _ = RegCloseKey(hkey);
    }
    rc == ERROR_SUCCESS
}

/// Aktif/nonaktifkan autostart. Nilai = `"<exe>" --tray`.
pub fn set(enabled: bool) -> bool {
    let Some(hkey) = open(KEY_SET_VALUE) else {
        return false;
    };
    let ok = if enabled {
        let exe = std::env::current_exe()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let cmd = format!("\"{exe}\" --tray");
        let mut wide: Vec<u16> = cmd.encode_utf16().collect();
        wide.push(0); // NUL terminator
        let bytes = unsafe {
            std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2)
        };
        let rc = unsafe { RegSetValueExW(hkey, VALUE_NAME, Some(0), REG_SZ, Some(bytes)) };
        rc == ERROR_SUCCESS
    } else {
        let rc = unsafe { RegDeleteValueW(hkey, VALUE_NAME) };
        // Sudah tidak ada juga dianggap sukses.
        rc == ERROR_SUCCESS || !is_enabled_after_close(hkey)
    };
    unsafe {
        let _ = RegCloseKey(hkey);
    }
    ok
}

fn is_enabled_after_close(_hkey: HKEY) -> bool {
    is_enabled()
}

/// Toggle; kembalikan status baru.
pub fn toggle() -> bool {
    let new = !is_enabled();
    set(new);
    is_enabled()
}
