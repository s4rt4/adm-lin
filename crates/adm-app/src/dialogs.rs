//! Dialog "Add new download / Download File Info" (plan §9.10). WM3: field
//! inti (URL, Category, Save As) + Start/Cancel, modal. Refinement (remember
//! path, preview, ikon tipe) menyusul.

use adm_ipc::DownloadAddParams;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::WindowsAndMessaging::*;

const IDOK: usize = 1;
const IDCANCEL: usize = 2;
const IDLATER: usize = 3;

static REGISTERED: AtomicBool = AtomicBool::new(false);
static DONE: AtomicBool = AtomicBool::new(false);
static START_NOW: AtomicBool = AtomicBool::new(true);
static RESULT: Mutex<Option<DownloadAddParams>> = Mutex::new(None);
/// Hasil probe ukuran dikirim ke dialog (wparam = total byte).
const WM_SIZE_RESULT: u32 = WM_APP + 10;
static SIZE_LABEL: Mutex<isize> = Mutex::new(0);
// HWND edit (sebagai isize) — dialog modal tunggal, jadi global aman.
static URL_EDIT: Mutex<isize> = Mutex::new(0);
static SAVE_EDIT: Mutex<isize> = Mutex::new(0);

const CLASS: PCWSTR = w!("AdmAddDialog");

fn gui_font() -> HGDIOBJ {
    unsafe { GetStockObject(DEFAULT_GUI_FONT) }
}

unsafe fn set_font(hwnd: HWND) {
    SendMessageW(
        hwnd,
        WM_SETFONT,
        Some(WPARAM(gui_font().0 as usize)),
        Some(LPARAM(1)),
    );
}

#[allow(clippy::too_many_arguments)]
unsafe fn make_child(
    parent: HWND,
    class: PCWSTR,
    text: PCWSTR,
    style: WINDOW_STYLE,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    id: usize,
    instance: HINSTANCE,
) -> HWND {
    let hwnd = CreateWindowExW(
        WINDOW_EX_STYLE::default(),
        class,
        text,
        style | WS_CHILD | WS_VISIBLE,
        x,
        y,
        w,
        h,
        Some(parent),
        Some(HMENU(id as *mut core::ffi::c_void)),
        Some(instance),
        None,
    )
    .unwrap_or_default();
    set_font(hwnd);
    hwnd
}

fn read_text(slot: &Mutex<isize>) -> String {
    let h = *slot.lock().unwrap();
    if h == 0 {
        return String::new();
    }
    let hwnd = HWND(h as *mut core::ffi::c_void);
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; len as usize + 1];
        let n = GetWindowTextW(hwnd, &mut buf);
        String::from_utf16_lossy(&buf[..n as usize])
    }
}

/// Tampilkan dialog Add modal. Mengembalikan params bila user menekan Start.
pub fn add_dialog(
    parent: HWND,
    default_url: &str,
    default_filename: Option<&str>,
    download_dir: &Path,
) -> Option<(DownloadAddParams, bool)> {
    unsafe {
        let instance: HINSTANCE = GetModuleHandleW(None).ok()?.into();

        if !REGISTERED.swap(true, Ordering::SeqCst) {
            let wc = WNDCLASSW {
                lpfnWndProc: Some(dlg_proc),
                hInstance: instance,
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                hbrBackground: HBRUSH((COLOR_BTNFACE.0 + 1) as *mut core::ffi::c_void),
                lpszClassName: CLASS,
                ..Default::default()
            };
            RegisterClassW(&wc);
        }

        DONE.store(false, Ordering::SeqCst);
        *RESULT.lock().unwrap() = None;

        // Posisikan di tengah parent.
        let mut pr = RECT::default();
        let _ = GetWindowRect(parent, &mut pr);
        let (dw, dh) = (560, 250);
        let x = pr.left + ((pr.right - pr.left) - dw) / 2;
        let y = pr.top + ((pr.bottom - pr.top) - dh) / 2;

        let dlg = CreateWindowExW(
            WS_EX_DLGMODALFRAME,
            CLASS,
            w!("Download File Info"),
            WS_POPUP | WS_CAPTION | WS_SYSMENU,
            x.max(0),
            y.max(0),
            dw,
            dh,
            Some(parent),
            None,
            Some(instance),
            None,
        )
        .ok()?;

        // Kontrol anak.
        let _ = make_child(dlg, w!("STATIC"), w!("URL:"), WINDOW_STYLE(0), 16, 18, 60, 18, 0, instance);
        let url = make_child(
            dlg, w!("EDIT"), PCWSTR::null(),
            WINDOW_STYLE((WS_BORDER.0 | WS_TABSTOP.0) | ES_AUTOHSCROLL as u32),
            84, 16, 452, 22, 100, instance,
        );
        *URL_EDIT.lock().unwrap() = url.0 as isize;
        if !default_url.is_empty() {
            let h = HSTRING::from(default_url);
            let _ = SetWindowTextW(url, PCWSTR(h.as_ptr()));
        }

        let _ = make_child(dlg, w!("STATIC"), w!("Category:"), WINDOW_STYLE(0), 16, 56, 60, 18, 0, instance);
        let combo = make_child(
            dlg, w!("COMBOBOX"), PCWSTR::null(),
            WINDOW_STYLE(WS_TABSTOP.0 | CBS_DROPDOWNLIST as u32 | WS_VSCROLL.0),
            84, 54, 200, 200, 101, instance,
        );
        for c in ["General", "Compressed", "Documents", "Music", "Programs", "Video"] {
            let h = HSTRING::from(c);
            SendMessageW(combo, CB_ADDSTRING, Some(WPARAM(0)), Some(LPARAM(h.as_ptr() as isize)));
        }
        SendMessageW(combo, CB_SETCURSEL, Some(WPARAM(0)), Some(LPARAM(0)));

        let _ = make_child(dlg, w!("STATIC"), w!("Save As:"), WINDOW_STYLE(0), 16, 94, 60, 18, 0, instance);
        let save = make_child(
            dlg, w!("EDIT"), PCWSTR::null(),
            WINDOW_STYLE((WS_BORDER.0 | WS_TABSTOP.0) | ES_AUTOHSCROLL as u32),
            84, 92, 452, 22, 102, instance,
        );
        *SAVE_EDIT.lock().unwrap() = save.0 as isize;
        // Prefill Save As: <download_dir>\<nama dari browser / tebakan url>
        let base = default_filename
            .map(|s| s.to_string())
            .unwrap_or_else(|| guess_filename(default_url));
        let initial = download_dir.join(&base);
        let h = HSTRING::from(initial.to_string_lossy().into_owned());
        let _ = SetWindowTextW(save, PCWSTR(h.as_ptr()));

        // Readout ukuran (di-probe async; "terdeteksi setelah beberapa saat").
        let _ = make_child(dlg, w!("STATIC"), w!("Size:"), WINDOW_STYLE(0), 16, 126, 50, 18, 0, instance);
        let size_lbl = make_child(dlg, w!("STATIC"), w!("\u{2026}"), WINDOW_STYLE(0), 70, 126, 200, 18, 0, instance);
        *SIZE_LABEL.lock().unwrap() = size_lbl.0 as isize;
        if !default_url.is_empty() {
            if let Some(eng) = crate::gui::engine() {
                let url = default_url.to_string();
                let dlg_isize = dlg.0 as isize;
                eng.runtime().spawn(async move {
                    let total = adm_core::probe_url(&url).await.ok().and_then(|p| p.total).unwrap_or(0);
                    let _ = PostMessageW(
                        Some(HWND(dlg_isize as *mut core::ffi::c_void)),
                        WM_SIZE_RESULT,
                        WPARAM(total as usize),
                        LPARAM(0),
                    );
                });
            }
        }

        let _ = make_child(
            dlg, w!("BUTTON"), w!("Download Later"),
            WINDOW_STYLE(WS_TABSTOP.0 | BS_PUSHBUTTON as u32),
            120, 160, 120, 30, IDLATER, instance,
        );
        let _ = make_child(
            dlg, w!("BUTTON"), w!("Start Download"),
            WINDOW_STYLE(WS_TABSTOP.0 | BS_DEFPUSHBUTTON as u32),
            250, 160, 140, 30, IDOK, instance,
        );
        let _ = make_child(
            dlg, w!("BUTTON"), w!("Cancel"),
            WINDOW_STYLE(WS_TABSTOP.0 | BS_PUSHBUTTON as u32),
            400, 160, 100, 30, IDCANCEL, instance,
        );

        let _ = EnableWindow(parent, false);
        let _ = ShowWindow(dlg, SW_SHOW);
        let _ = SetForegroundWindow(dlg);

        // Loop modal.
        let mut msg = MSG::default();
        while !DONE.load(Ordering::SeqCst) && GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if !IsDialogMessageW(dlg, &msg).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }

        let _ = EnableWindow(parent, true);
        let _ = SetForegroundWindow(parent);
        if IsWindow(Some(dlg)).as_bool() {
            let _ = DestroyWindow(dlg);
        }

        RESULT
            .lock()
            .unwrap()
            .take()
            .map(|p| (p, START_NOW.load(Ordering::SeqCst)))
    }
}

extern "system" fn dlg_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_COMMAND => {
                let id = wparam.0 & 0xFFFF;
                match id {
                    IDOK | IDLATER => {
                        let url = read_text(&URL_EDIT);
                        if !url.trim().is_empty() {
                            let save = read_text(&SAVE_EDIT);
                            let filename = save
                                .rsplit(['\\', '/'])
                                .next()
                                .filter(|s| !s.is_empty())
                                .map(|s| s.to_string());
                            *RESULT.lock().unwrap() = Some(DownloadAddParams {
                                url: url.trim().to_string(),
                                filename,
                                ..Default::default()
                            });
                            START_NOW.store(id == IDOK, Ordering::SeqCst);
                        }
                        DONE.store(true, Ordering::SeqCst);
                        let _ = DestroyWindow(hwnd);
                        LRESULT(0)
                    }
                    IDCANCEL => {
                        DONE.store(true, Ordering::SeqCst);
                        let _ = DestroyWindow(hwnd);
                        LRESULT(0)
                    }
                    _ => DefWindowProcW(hwnd, msg, wparam, lparam),
                }
            }
            WM_SIZE_RESULT => {
                let total = wparam.0 as u64;
                let h = *SIZE_LABEL.lock().unwrap();
                if h != 0 {
                    let txt = if total > 0 { fmt_size(total) } else { "Unknown".to_string() };
                    let hs = HSTRING::from(txt);
                    let _ = SetWindowTextW(HWND(h as *mut core::ffi::c_void), PCWSTR(hs.as_ptr()));
                }
                LRESULT(0)
            }
            WM_CLOSE => {
                DONE.store(true, Ordering::SeqCst);
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

fn fmt_size(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1024.0 * 1024.0 * 1024.0 {
        format!("{:.2} GB", b / (1024.0 * 1024.0 * 1024.0))
    } else if b >= 1024.0 * 1024.0 {
        format!("{:.2} MB", b / (1024.0 * 1024.0))
    } else if b >= 1024.0 {
        format!("{:.2} KB", b / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

fn guess_filename(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or("");
    path.rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("download.bin")
        .to_string()
}
