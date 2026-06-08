//! Dialog Options (plan §9.16). Subset fungsional: folder unduhan, batas
//! antrian, batas kecepatan global, autostart, bahasa. Persist via `settings`
//! dan diterapkan live ke engine.

use crate::{autostart, settings};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::WindowsAndMessaging::*;

const CLASS: PCWSTR = w!("AdmOptionsDialog");
static REGISTERED: AtomicBool = AtomicBool::new(false);
static DONE: AtomicBool = AtomicBool::new(false);
static SAVED: AtomicBool = AtomicBool::new(false);

const ID_DIR: usize = 1;
const ID_QUEUE: usize = 2;
const ID_LIMIT: usize = 3;
const ID_AUTOSTART: usize = 4;
const ID_LANG: usize = 5;
const ID_OK: usize = 20;
const ID_CANCEL: usize = 21;

// 0 dir, 1 queue, 2 limit, 3 autostart, 4 lang
static CTRL: Mutex<[isize; 5]> = Mutex::new([0; 5]);

fn set_ctrl(i: usize, h: HWND) {
    CTRL.lock().unwrap()[i] = h.0 as isize;
}
fn ctrl(i: usize) -> HWND {
    HWND(CTRL.lock().unwrap()[i] as *mut core::ffi::c_void)
}

unsafe fn gui_font() -> HGDIOBJ {
    GetStockObject(DEFAULT_GUI_FONT)
}

#[allow(clippy::too_many_arguments)]
unsafe fn mk(parent: HWND, class: PCWSTR, text: PCWSTR, style: WINDOW_STYLE, x: i32, y: i32, w: i32, h: i32, id: usize) -> HWND {
    let instance: HINSTANCE = GetModuleHandleW(None).unwrap_or_default().into();
    let hwnd = CreateWindowExW(
        WINDOW_EX_STYLE::default(),
        class,
        text,
        style | WS_CHILD | WS_VISIBLE,
        x, y, w, h,
        Some(parent),
        Some(HMENU(id as *mut core::ffi::c_void)),
        Some(instance),
        None,
    )
    .unwrap_or_default();
    SendMessageW(hwnd, WM_SETFONT, Some(WPARAM(gui_font().0 as usize)), Some(LPARAM(1)));
    hwnd
}

fn set_text(h: HWND, s: &str) {
    let hs = HSTRING::from(s);
    unsafe {
        let _ = SetWindowTextW(h, PCWSTR(hs.as_ptr()));
    }
}
unsafe fn get_text(h: HWND) -> String {
    let len = GetWindowTextLengthW(h);
    if len <= 0 {
        return String::new();
    }
    let mut buf = vec![0u16; len as usize + 1];
    let n = GetWindowTextW(h, &mut buf);
    String::from_utf16_lossy(&buf[..n as usize])
}

pub fn show(parent: HWND) {
    unsafe {
        let instance: HINSTANCE = match GetModuleHandleW(None) {
            Ok(h) => h.into(),
            Err(_) => return,
        };
        if !REGISTERED.swap(true, Ordering::SeqCst) {
            let wc = WNDCLASSW {
                lpfnWndProc: Some(proc_),
                hInstance: instance,
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                hbrBackground: HBRUSH((COLOR_BTNFACE.0 + 1) as *mut core::ffi::c_void),
                lpszClassName: CLASS,
                ..Default::default()
            };
            RegisterClassW(&wc);
        }
        DONE.store(false, Ordering::SeqCst);
        SAVED.store(false, Ordering::SeqCst);

        let cfg = settings::get();
        let dir = cfg
            .download_dir
            .clone()
            .or_else(|| crate::gui::engine().map(|e| e.download_dir().to_string_lossy().into_owned()))
            .unwrap_or_default();

        let mut pr = RECT::default();
        let _ = GetWindowRect(parent, &mut pr);
        let (dw, dh) = (480, 280);
        let x = (pr.left + ((pr.right - pr.left) - dw) / 2).max(0);
        let y = (pr.top + ((pr.bottom - pr.top) - dh) / 2).max(0);

        let dlg = CreateWindowExW(
            WS_EX_DLGMODALFRAME,
            CLASS,
            w!("Options"),
            WS_POPUP | WS_CAPTION | WS_SYSMENU,
            x, y, dw, dh,
            Some(parent),
            None,
            Some(instance),
            None,
        );
        let Ok(dlg) = dlg else { return };

        let _ = mk(dlg, w!("STATIC"), w!("Download folder:"), WINDOW_STYLE(0), 16, 18, 120, 18, 0);
        let d = mk(dlg, w!("EDIT"), PCWSTR::null(), WINDOW_STYLE(WS_BORDER.0 | ES_AUTOHSCROLL as u32), 16, 38, 440, 22, ID_DIR);
        set_text(d, &dir);
        set_ctrl(0, d);

        let _ = mk(dlg, w!("STATIC"), w!("Max simultaneous (queue):"), WINDOW_STYLE(0), 16, 74, 180, 18, 0);
        let q = mk(dlg, w!("EDIT"), PCWSTR::null(), WINDOW_STYLE(WS_BORDER.0 | ES_NUMBER as u32), 200, 72, 60, 22, ID_QUEUE);
        set_text(q, &cfg.queue_max.to_string());
        set_ctrl(1, q);

        let _ = mk(dlg, w!("STATIC"), w!("Global speed limit (KB/s, 0=unlimited):"), WINDOW_STYLE(0), 16, 108, 250, 18, 0);
        let l = mk(dlg, w!("EDIT"), PCWSTR::null(), WINDOW_STYLE(WS_BORDER.0 | ES_NUMBER as u32), 270, 106, 80, 22, ID_LIMIT);
        set_text(l, &cfg.global_limit_kbps.to_string());
        set_ctrl(2, l);

        let a = mk(dlg, w!("BUTTON"), w!("Start with Windows"), WINDOW_STYLE(BS_AUTOCHECKBOX as u32), 16, 142, 220, 20, ID_AUTOSTART);
        SendMessageW(a, BM_SETCHECK, Some(WPARAM(if cfg.autostart { 1 } else { 0 })), Some(LPARAM(0)));
        set_ctrl(3, a);

        let _ = mk(dlg, w!("STATIC"), w!("Language:"), WINDOW_STYLE(0), 16, 176, 80, 18, 0);
        let lang = mk(dlg, w!("COMBOBOX"), PCWSTR::null(), WINDOW_STYLE(CBS_DROPDOWNLIST as u32 | WS_VSCROLL.0), 100, 174, 160, 200, ID_LANG);
        for s in ["English (en)", "Indonesia (id)"] {
            let hs = HSTRING::from(s);
            SendMessageW(lang, CB_ADDSTRING, Some(WPARAM(0)), Some(LPARAM(hs.as_ptr() as isize)));
        }
        SendMessageW(lang, CB_SETCURSEL, Some(WPARAM(if cfg.language == "id" { 1 } else { 0 })), Some(LPARAM(0)));
        set_ctrl(4, lang);

        let _ = mk(dlg, w!("BUTTON"), w!("OK"), WINDOW_STYLE(WS_TABSTOP.0 | BS_DEFPUSHBUTTON as u32), 280, 220, 80, 28, ID_OK);
        let _ = mk(dlg, w!("BUTTON"), w!("Cancel"), WINDOW_STYLE(WS_TABSTOP.0 | BS_PUSHBUTTON as u32), 372, 220, 80, 28, ID_CANCEL);

        let _ = EnableWindow(parent, false);
        let _ = ShowWindow(dlg, SW_SHOW);
        let _ = SetForegroundWindow(dlg);

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

        if SAVED.load(Ordering::SeqCst) {
            let dir = get_text(ctrl(0)).trim().to_string();
            let queue_max = get_text(ctrl(1)).trim().parse::<usize>().unwrap_or(1).max(1);
            let limit = get_text(ctrl(2)).trim().parse::<u64>().unwrap_or(0);
            let auto = SendMessageW(ctrl(3), BM_GETCHECK, Some(WPARAM(0)), Some(LPARAM(0))).0 == 1;
            let lang = if SendMessageW(ctrl(4), CB_GETCURSEL, Some(WPARAM(0)), Some(LPARAM(0))).0 == 1 {
                "id"
            } else {
                "en"
            };

            settings::update(|s| {
                s.download_dir = if dir.is_empty() { None } else { Some(dir.clone()) };
                s.queue_max = queue_max;
                s.global_limit_kbps = limit;
                s.autostart = auto;
                s.language = lang.to_string();
            });

            if let Some(e) = crate::gui::engine() {
                if !dir.is_empty() {
                    e.set_download_dir(PathBuf::from(&dir));
                }
                e.set_queue_max(queue_max);
                e.set_global_limit(limit.saturating_mul(1024));
            }
            autostart::set(auto);
        }
    }
}

extern "system" fn proc_(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_COMMAND => {
                match wparam.0 & 0xFFFF {
                    ID_OK => {
                        SAVED.store(true, Ordering::SeqCst);
                        DONE.store(true, Ordering::SeqCst);
                        let _ = DestroyWindow(hwnd);
                    }
                    ID_CANCEL => {
                        DONE.store(true, Ordering::SeqCst);
                        let _ = DestroyWindow(hwnd);
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_CTLCOLORSTATIC => {
                SetBkMode(HDC(wparam.0 as *mut _), TRANSPARENT);
                LRESULT(GetSysColorBrush(COLOR_BTNFACE).0 as isize)
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
