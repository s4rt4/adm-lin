//! Fungsi pendukung menu Tasks: batch download (dialog multi-URL + ekspansi
//! wildcard `[a-b]`), ambil URL dari clipboard, serta Export/Import daftar URL.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use windows::core::{w, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::UI::Controls::Dialogs::*;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::*;

// ============================ Clipboard ============================

/// Baca teks (CF_UNICODETEXT) dari clipboard, bila ada.
pub fn read_clipboard_text() -> Option<String> {
    unsafe {
        if OpenClipboard(None).is_err() {
            return None;
        }
        let mut out = None;
        if let Ok(handle) = GetClipboardData(CF_UNICODETEXT.0 as u32) {
            if !handle.0.is_null() {
                let hglobal = HGLOBAL(handle.0);
                let ptr = GlobalLock(hglobal) as *const u16;
                if !ptr.is_null() {
                    let mut len = 0usize;
                    while *ptr.add(len) != 0 {
                        len += 1;
                    }
                    let slice = std::slice::from_raw_parts(ptr, len);
                    out = Some(String::from_utf16_lossy(slice));
                    let _ = GlobalUnlock(hglobal);
                }
            }
        }
        let _ = CloseClipboard();
        out
    }
}

// ============================ Parsing ============================

fn is_url(s: &str) -> bool {
    (s.starts_with("http://") || s.starts_with("https://")) && s.len() > 10
}

/// Ambil semua URL http(s) dari teks bebas (per token), urut & tanpa duplikat.
pub fn extract_urls(text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for tok in text.split(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '(' | ')')) {
        let t = tok.trim().trim_end_matches(['.', ',', ';']);
        if is_url(t) && seen.insert(t.to_string()) {
            out.push(t.to_string());
        }
    }
    out
}

/// Ekspansi pola `[start-end]` numerik (mendukung zero-pad: `[01-12]`).
/// Beberapa pola dalam satu baris diekspansi kartesian. Dibatasi agar aman.
pub fn expand_pattern(line: &str) -> Vec<String> {
    if let Some(open) = line.find('[') {
        if let Some(close_rel) = line[open..].find(']') {
            let close = open + close_rel;
            let inner = &line[open + 1..close];
            if let Some(dash) = inner.find('-') {
                let a = &inner[..dash];
                let b = &inner[dash + 1..];
                if let (Ok(start), Ok(end)) = (a.parse::<u64>(), b.parse::<u64>()) {
                    if start <= end && end - start < 100_000 {
                        let width = a.len();
                        let pad = a.starts_with('0') && width > 1;
                        let (pre, post) = (&line[..open], &line[close + 1..]);
                        let mut out = Vec::new();
                        for n in start..=end {
                            let num = if pad {
                                format!("{n:0width$}")
                            } else {
                                n.to_string()
                            };
                            out.extend(expand_pattern(&format!("{pre}{num}{post}")));
                        }
                        return out;
                    }
                }
            }
        }
    }
    vec![line.to_string()]
}

/// Pecah teks batch (per baris) → daftar URL final (wildcard diekspansi).
pub fn parse_batch(text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for line in text.lines() {
        let l = line.trim();
        if l.is_empty() {
            continue;
        }
        for u in expand_pattern(l) {
            let u = u.trim().to_string();
            if is_url(&u) && seen.insert(u.clone()) {
                out.push(u);
            }
        }
    }
    out
}

// ============================ Dialog batch ============================

const IDOK_BTN: usize = 1;
const IDCANCEL_BTN: usize = 2;
const ID_EDIT: usize = 100;
const CLASS: PCWSTR = w!("AdmBatchDialog");

static REGISTERED: AtomicBool = AtomicBool::new(false);
static DONE: AtomicBool = AtomicBool::new(false);
static RESULT: Mutex<Option<String>> = Mutex::new(None);
static EDIT_HWND: Mutex<isize> = Mutex::new(0);

/// Dialog batch: textarea multi-baris (1 URL atau pola per baris). `initial`
/// dipakai untuk prefill (mis. hasil clipboard). Mengembalikan teks bila OK.
pub fn batch_dialog(parent: HWND, initial: &str) -> Option<String> {
    unsafe {
        let module = GetModuleHandleW(None).ok()?;
        let instance: HINSTANCE = module.into();
        if !REGISTERED.swap(true, Ordering::SeqCst) {
            let wc = WNDCLASSW {
                lpfnWndProc: Some(batch_proc),
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

        let style = WS_POPUP | WS_CAPTION | WS_SYSMENU;
        let mut rc = RECT { left: 0, top: 0, right: 540, bottom: 360 };
        let _ = AdjustWindowRectEx(&mut rc, style, false, WS_EX_DLGMODALFRAME);
        let (dw, dh) = (rc.right - rc.left, rc.bottom - rc.top);
        let mut pr = RECT::default();
        let _ = GetWindowRect(parent, &mut pr);
        let x = pr.left + ((pr.right - pr.left) - dw) / 2;
        let y = pr.top + ((pr.bottom - pr.top) - dh) / 2;

        let dlg = CreateWindowExW(
            WS_EX_DLGMODALFRAME,
            CLASS,
            w!("Add batch download"),
            style,
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

        let font = GetStockObject(DEFAULT_GUI_FONT);
        let mk = |class: PCWSTR, text: PCWSTR, st: WINDOW_STYLE, x, y, w, h, id| {
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class,
                text,
                st | WS_CHILD | WS_VISIBLE,
                x,
                y,
                w,
                h,
                Some(dlg),
                Some(HMENU(id as *mut core::ffi::c_void)),
                Some(instance),
                None,
            )
            .unwrap_or_default();
            SendMessageW(hwnd, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
            hwnd
        };

        mk(
            w!("STATIC"),
            w!("Masukkan satu URL per baris. Pola angka didukung, mis. file[1-10].zip"),
            WINDOW_STYLE(0),
            16,
            12,
            500,
            18,
            0,
        );
        let edit = mk(
            w!("EDIT"),
            PCWSTR::null(),
            WINDOW_STYLE(
                WS_BORDER.0
                    | WS_TABSTOP.0
                    | WS_VSCROLL.0
                    | ES_MULTILINE as u32
                    | ES_AUTOVSCROLL as u32
                    | ES_WANTRETURN as u32,
            ),
            16,
            36,
            508,
            250,
            ID_EDIT,
        );
        *EDIT_HWND.lock().unwrap() = edit.0 as isize;
        if !initial.is_empty() {
            let h = HSTRING::from(initial);
            let _ = SetWindowTextW(edit, PCWSTR(h.as_ptr()));
        }

        mk(
            w!("BUTTON"),
            w!("OK"),
            WINDOW_STYLE(WS_TABSTOP.0 | BS_DEFPUSHBUTTON as u32),
            316,
            300,
            100,
            30,
            IDOK_BTN,
        );
        mk(
            w!("BUTTON"),
            w!("Cancel"),
            WINDOW_STYLE(WS_TABSTOP.0 | BS_PUSHBUTTON as u32),
            424,
            300,
            100,
            30,
            IDCANCEL_BTN,
        );

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
        RESULT.lock().unwrap().take()
    }
}

extern "system" fn batch_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_COMMAND => {
                let id = wparam.0 & 0xFFFF;
                match id {
                    IDOK_BTN => {
                        let h = HWND(*EDIT_HWND.lock().unwrap() as *mut core::ffi::c_void);
                        let len = GetWindowTextLengthW(h);
                        let text = if len > 0 {
                            let mut buf = vec![0u16; len as usize + 1];
                            let n = GetWindowTextW(h, &mut buf);
                            String::from_utf16_lossy(&buf[..n as usize])
                        } else {
                            String::new()
                        };
                        *RESULT.lock().unwrap() = Some(text);
                        DONE.store(true, Ordering::SeqCst);
                        let _ = DestroyWindow(hwnd);
                        LRESULT(0)
                    }
                    IDCANCEL_BTN => {
                        DONE.store(true, Ordering::SeqCst);
                        let _ = DestroyWindow(hwnd);
                        LRESULT(0)
                    }
                    _ => DefWindowProcW(hwnd, msg, wparam, lparam),
                }
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

// ============================ Prompt satu baris ============================

const P_OK: usize = 1;
const P_CANCEL: usize = 2;
const P_EDIT: usize = 100;
const P_CLASS: PCWSTR = w!("AdmPromptDialog");
static P_REG: AtomicBool = AtomicBool::new(false);
static P_DONE: AtomicBool = AtomicBool::new(false);
static P_RESULT: Mutex<Option<String>> = Mutex::new(None);
static P_EDIT_HWND: Mutex<isize> = Mutex::new(0);

/// Dialog input satu baris (mis. untuk Find). Mengembalikan teks bila OK.
pub fn prompt_dialog(parent: HWND, title: &str, label: &str, initial: &str) -> Option<String> {
    unsafe {
        let module = GetModuleHandleW(None).ok()?;
        let instance: HINSTANCE = module.into();
        if !P_REG.swap(true, Ordering::SeqCst) {
            let wc = WNDCLASSW {
                lpfnWndProc: Some(prompt_proc),
                hInstance: instance,
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                hbrBackground: HBRUSH((COLOR_BTNFACE.0 + 1) as *mut core::ffi::c_void),
                lpszClassName: P_CLASS,
                ..Default::default()
            };
            RegisterClassW(&wc);
        }
        P_DONE.store(false, Ordering::SeqCst);
        *P_RESULT.lock().unwrap() = None;

        let style = WS_POPUP | WS_CAPTION | WS_SYSMENU;
        let mut rc = RECT { left: 0, top: 0, right: 380, bottom: 132 };
        let _ = AdjustWindowRectEx(&mut rc, style, false, WS_EX_DLGMODALFRAME);
        let (dw, dh) = (rc.right - rc.left, rc.bottom - rc.top);
        let mut pr = RECT::default();
        let _ = GetWindowRect(parent, &mut pr);
        let x = pr.left + ((pr.right - pr.left) - dw) / 2;
        let y = pr.top + ((pr.bottom - pr.top) - dh) / 2;

        let th = HSTRING::from(title);
        let Ok(dlg) = CreateWindowExW(
            WS_EX_DLGMODALFRAME,
            P_CLASS,
            PCWSTR(th.as_ptr()),
            style,
            x.max(0),
            y.max(0),
            dw,
            dh,
            Some(parent),
            None,
            Some(instance),
            None,
        ) else {
            return None;
        };

        let font = GetStockObject(DEFAULT_GUI_FONT);
        let mk = |class: PCWSTR, text: PCWSTR, st: WINDOW_STYLE, x, y, w, h, id| {
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class,
                text,
                st | WS_CHILD | WS_VISIBLE,
                x,
                y,
                w,
                h,
                Some(dlg),
                Some(HMENU(id as *mut core::ffi::c_void)),
                Some(instance),
                None,
            )
            .unwrap_or_default();
            SendMessageW(hwnd, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
            hwnd
        };

        let lh = HSTRING::from(label);
        mk(w!("STATIC"), PCWSTR(lh.as_ptr()), WINDOW_STYLE(0), 16, 14, 348, 18, 0);
        let edit = mk(
            w!("EDIT"),
            PCWSTR::null(),
            WINDOW_STYLE(WS_BORDER.0 | WS_TABSTOP.0 | ES_AUTOHSCROLL as u32),
            16,
            36,
            348,
            24,
            P_EDIT,
        );
        *P_EDIT_HWND.lock().unwrap() = edit.0 as isize;
        if !initial.is_empty() {
            let h = HSTRING::from(initial);
            let _ = SetWindowTextW(edit, PCWSTR(h.as_ptr()));
            SendMessageW(edit, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
        }
        mk(
            w!("BUTTON"),
            w!("OK"),
            WINDOW_STYLE(WS_TABSTOP.0 | BS_DEFPUSHBUTTON as u32),
            176,
            74,
            90,
            28,
            P_OK,
        );
        mk(
            w!("BUTTON"),
            w!("Cancel"),
            WINDOW_STYLE(WS_TABSTOP.0 | BS_PUSHBUTTON as u32),
            274,
            74,
            90,
            28,
            P_CANCEL,
        );

        let _ = EnableWindow(parent, false);
        let _ = ShowWindow(dlg, SW_SHOW);
        let _ = SetForegroundWindow(dlg);
        let _ = SetFocus(Some(edit));

        let mut msg = MSG::default();
        while !P_DONE.load(Ordering::SeqCst) && GetMessageW(&mut msg, None, 0, 0).as_bool() {
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
        P_RESULT.lock().unwrap().take()
    }
}

extern "system" fn prompt_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_COMMAND => {
                let id = wparam.0 & 0xFFFF;
                match id {
                    P_OK => {
                        let h = HWND(*P_EDIT_HWND.lock().unwrap() as *mut core::ffi::c_void);
                        let len = GetWindowTextLengthW(h);
                        let text = if len > 0 {
                            let mut buf = vec![0u16; len as usize + 1];
                            let n = GetWindowTextW(h, &mut buf);
                            String::from_utf16_lossy(&buf[..n as usize])
                        } else {
                            String::new()
                        };
                        *P_RESULT.lock().unwrap() = Some(text);
                        P_DONE.store(true, Ordering::SeqCst);
                        let _ = DestroyWindow(hwnd);
                        LRESULT(0)
                    }
                    P_CANCEL => {
                        P_DONE.store(true, Ordering::SeqCst);
                        let _ = DestroyWindow(hwnd);
                        LRESULT(0)
                    }
                    _ => DefWindowProcW(hwnd, msg, wparam, lparam),
                }
            }
            WM_CLOSE => {
                P_DONE.store(true, Ordering::SeqCst);
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

// ============================ Customize columns ============================

const C_OK: usize = 1;
const C_CANCEL: usize = 2;
const C_BASE: usize = 100;
const C_CLASS: PCWSTR = w!("AdmColumnsDialog");
static C_REG: AtomicBool = AtomicBool::new(false);
static C_DONE: AtomicBool = AtomicBool::new(false);
static C_RESULT: Mutex<Option<Vec<bool>>> = Mutex::new(None);
static C_CHECKS: Mutex<Vec<isize>> = Mutex::new(Vec::new());

/// Dialog pilih kolom (checkbox per kolom). Kolom 0 dikunci (selalu tampil).
pub fn columns_dialog(parent: HWND, names: &[&str], current: &[bool]) -> Option<Vec<bool>> {
    unsafe {
        let module = GetModuleHandleW(None).ok()?;
        let instance: HINSTANCE = module.into();
        if !C_REG.swap(true, Ordering::SeqCst) {
            let wc = WNDCLASSW {
                lpfnWndProc: Some(columns_proc),
                hInstance: instance,
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                hbrBackground: HBRUSH((COLOR_BTNFACE.0 + 1) as *mut core::ffi::c_void),
                lpszClassName: C_CLASS,
                ..Default::default()
            };
            RegisterClassW(&wc);
        }
        C_DONE.store(false, Ordering::SeqCst);
        *C_RESULT.lock().unwrap() = None;
        C_CHECKS.lock().unwrap().clear();

        let rows = names.len() as i32;
        let style = WS_POPUP | WS_CAPTION | WS_SYSMENU;
        let mut rc = RECT { left: 0, top: 0, right: 300, bottom: 56 + rows * 26 + 56 };
        let _ = AdjustWindowRectEx(&mut rc, style, false, WS_EX_DLGMODALFRAME);
        let (dw, dh) = (rc.right - rc.left, rc.bottom - rc.top);
        let mut pr = RECT::default();
        let _ = GetWindowRect(parent, &mut pr);
        let x = pr.left + ((pr.right - pr.left) - dw) / 2;
        let y = pr.top + ((pr.bottom - pr.top) - dh) / 2;

        let Ok(dlg) = CreateWindowExW(
            WS_EX_DLGMODALFRAME,
            C_CLASS,
            w!("Customize URL List"),
            style,
            x.max(0),
            y.max(0),
            dw,
            dh,
            Some(parent),
            None,
            Some(instance),
            None,
        ) else {
            return None;
        };

        let font = GetStockObject(DEFAULT_GUI_FONT);
        let mk = |class: PCWSTR, text: PCWSTR, st: WINDOW_STYLE, x, y, w, h, id| {
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class,
                text,
                st | WS_CHILD | WS_VISIBLE,
                x,
                y,
                w,
                h,
                Some(dlg),
                Some(HMENU(id as *mut core::ffi::c_void)),
                Some(instance),
                None,
            )
            .unwrap_or_default();
            SendMessageW(hwnd, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
            hwnd
        };

        mk(w!("STATIC"), w!("Kolom yang ditampilkan:"), WINDOW_STYLE(0), 16, 12, 260, 18, 0);
        let mut checks = Vec::new();
        for (i, name) in names.iter().enumerate() {
            let nh = HSTRING::from(*name);
            let cb = mk(
                w!("BUTTON"),
                PCWSTR(nh.as_ptr()),
                WINDOW_STYLE(WS_TABSTOP.0 | BS_AUTOCHECKBOX as u32),
                24,
                36 + i as i32 * 26,
                250,
                22,
                C_BASE + i,
            );
            let checked = current.get(i).copied().unwrap_or(true);
            SendMessageW(cb, BM_SETCHECK, Some(WPARAM(if checked { 1 } else { 0 })), Some(LPARAM(0)));
            if i == 0 {
                let _ = EnableWindow(cb, false); // "File Name" dikunci
            }
            checks.push(cb.0 as isize);
        }
        *C_CHECKS.lock().unwrap() = checks;

        let by = 44 + rows * 26;
        mk(
            w!("BUTTON"),
            w!("OK"),
            WINDOW_STYLE(WS_TABSTOP.0 | BS_DEFPUSHBUTTON as u32),
            96,
            by,
            90,
            28,
            C_OK,
        );
        mk(
            w!("BUTTON"),
            w!("Cancel"),
            WINDOW_STYLE(WS_TABSTOP.0 | BS_PUSHBUTTON as u32),
            194,
            by,
            90,
            28,
            C_CANCEL,
        );

        let _ = EnableWindow(parent, false);
        let _ = ShowWindow(dlg, SW_SHOW);
        let _ = SetForegroundWindow(dlg);

        let mut msg = MSG::default();
        while !C_DONE.load(Ordering::SeqCst) && GetMessageW(&mut msg, None, 0, 0).as_bool() {
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
        C_RESULT.lock().unwrap().take()
    }
}

extern "system" fn columns_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_COMMAND => {
                let id = wparam.0 & 0xFFFF;
                match id {
                    C_OK => {
                        let checks = C_CHECKS.lock().unwrap().clone();
                        let vis: Vec<bool> = checks
                            .iter()
                            .map(|&h| {
                                let cb = HWND(h as *mut core::ffi::c_void);
                                SendMessageW(cb, BM_GETCHECK, Some(WPARAM(0)), Some(LPARAM(0))).0 == 1
                            })
                            .collect();
                        *C_RESULT.lock().unwrap() = Some(vis);
                        C_DONE.store(true, Ordering::SeqCst);
                        let _ = DestroyWindow(hwnd);
                        LRESULT(0)
                    }
                    C_CANCEL => {
                        C_DONE.store(true, Ordering::SeqCst);
                        let _ = DestroyWindow(hwnd);
                        LRESULT(0)
                    }
                    _ => DefWindowProcW(hwnd, msg, wparam, lparam),
                }
            }
            WM_CLOSE => {
                C_DONE.store(true, Ordering::SeqCst);
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

// ============================ Export / Import ============================

/// Filter dialog file: UTF-16 dengan null pemisah ganda di akhir.
fn filter_txt() -> Vec<u16> {
    let mut v: Vec<u16> = Vec::new();
    for part in ["Text files (*.txt)", "*.txt", "All files (*.*)", "*.*"] {
        v.extend(part.encode_utf16());
        v.push(0);
    }
    v.push(0);
    v
}

/// Pilih lokasi simpan (.txt) untuk daftar URL. Mengembalikan path bila dipilih.
pub fn save_dialog(parent: HWND) -> Option<PathBuf> {
    unsafe {
        let mut buf = [0u16; 1024];
        let def = "adm-downloads.txt".encode_utf16().collect::<Vec<u16>>();
        buf[..def.len()].copy_from_slice(&def);
        let filter = filter_txt();
        let ext: Vec<u16> = "txt\0".encode_utf16().collect();
        let mut ofn = OPENFILENAMEW {
            lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
            hwndOwner: parent,
            lpstrFilter: PCWSTR(filter.as_ptr()),
            lpstrFile: PWSTR(buf.as_mut_ptr()),
            nMaxFile: buf.len() as u32,
            lpstrDefExt: PCWSTR(ext.as_ptr()),
            Flags: OFN_OVERWRITEPROMPT | OFN_PATHMUSTEXIST | OFN_HIDEREADONLY,
            ..Default::default()
        };
        if GetSaveFileNameW(&mut ofn).as_bool() {
            Some(PathBuf::from(pwstr_to_string(&buf)))
        } else {
            None
        }
    }
}

/// Pilih file daftar URL untuk diimpor.
pub fn open_dialog(parent: HWND) -> Option<PathBuf> {
    unsafe {
        let mut buf = [0u16; 1024];
        let filter = filter_txt();
        let mut ofn = OPENFILENAMEW {
            lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
            hwndOwner: parent,
            lpstrFilter: PCWSTR(filter.as_ptr()),
            lpstrFile: PWSTR(buf.as_mut_ptr()),
            nMaxFile: buf.len() as u32,
            Flags: OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_HIDEREADONLY,
            ..Default::default()
        };
        if GetOpenFileNameW(&mut ofn).as_bool() {
            Some(PathBuf::from(pwstr_to_string(&buf)))
        } else {
            None
        }
    }
}

fn pwstr_to_string(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

// ============================ Site grabber ============================

const G_FETCH: usize = 10;
const G_OK: usize = 11;
const G_CANCEL: usize = 12;
const G_URL: usize = 13;
const G_LIST: usize = 14;
const G_CLASS: PCWSTR = w!("AdmGrabberDialog");
/// Hasil grab dikirim balik dari runtime: lParam = *mut Vec<String>.
const WM_GRAB_RESULT: u32 = WM_APP + 20;

static G_REG: AtomicBool = AtomicBool::new(false);
static G_DONE: AtomicBool = AtomicBool::new(false);
static G_RESULT: Mutex<Vec<String>> = Mutex::new(Vec::new());
static G_LINKS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static G_URL_HWND: Mutex<isize> = Mutex::new(0);
static G_LIST_HWND: Mutex<isize> = Mutex::new(0);
static G_DLG: Mutex<isize> = Mutex::new(0);

/// Dialog Site Grabber: input URL halaman → Fetch → daftar tautan (checkbox) →
/// Download selected. Mengembalikan URL terpilih.
pub fn grabber_dialog(parent: HWND) -> Vec<String> {
    unsafe {
        let Ok(module) = GetModuleHandleW(None) else { return Vec::new() };
        let instance: HINSTANCE = module.into();
        if !G_REG.swap(true, Ordering::SeqCst) {
            let wc = WNDCLASSW {
                lpfnWndProc: Some(grab_proc),
                hInstance: instance,
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                hbrBackground: HBRUSH((COLOR_BTNFACE.0 + 1) as *mut core::ffi::c_void),
                lpszClassName: G_CLASS,
                ..Default::default()
            };
            RegisterClassW(&wc);
        }
        G_DONE.store(false, Ordering::SeqCst);
        *G_RESULT.lock().unwrap() = Vec::new();
        *G_LINKS.lock().unwrap() = Vec::new();

        let style = WS_POPUP | WS_CAPTION | WS_SYSMENU;
        let mut rc = RECT { left: 0, top: 0, right: 600, bottom: 420 };
        let _ = AdjustWindowRectEx(&mut rc, style, false, WS_EX_DLGMODALFRAME);
        let (dw, dh) = (rc.right - rc.left, rc.bottom - rc.top);
        let mut pr = RECT::default();
        let _ = GetWindowRect(parent, &mut pr);
        let x = pr.left + ((pr.right - pr.left) - dw) / 2;
        let y = pr.top + ((pr.bottom - pr.top) - dh) / 2;

        let Ok(dlg) = CreateWindowExW(
            WS_EX_DLGMODALFRAME,
            G_CLASS,
            w!("Run site grabber"),
            style,
            x.max(0),
            y.max(0),
            dw,
            dh,
            Some(parent),
            None,
            Some(instance),
            None,
        ) else {
            return Vec::new();
        };
        *G_DLG.lock().unwrap() = dlg.0 as isize;

        let font = GetStockObject(DEFAULT_GUI_FONT);
        let mk = |class: PCWSTR, text: PCWSTR, st: WINDOW_STYLE, ex: WINDOW_EX_STYLE, x, y, w, h, id| {
            let hwnd = CreateWindowExW(
                ex,
                class,
                text,
                st | WS_CHILD | WS_VISIBLE,
                x,
                y,
                w,
                h,
                Some(dlg),
                Some(HMENU(id as *mut core::ffi::c_void)),
                Some(instance),
                None,
            )
            .unwrap_or_default();
            SendMessageW(hwnd, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
            hwnd
        };

        mk(w!("STATIC"), w!("Page URL:"), WINDOW_STYLE(0), WINDOW_EX_STYLE::default(), 16, 18, 70, 18, 0);
        let url = mk(
            w!("EDIT"),
            PCWSTR::null(),
            WINDOW_STYLE(WS_BORDER.0 | WS_TABSTOP.0 | ES_AUTOHSCROLL as u32),
            WINDOW_EX_STYLE::default(),
            90,
            16,
            390,
            24,
            G_URL,
        );
        *G_URL_HWND.lock().unwrap() = url.0 as isize;
        mk(
            w!("BUTTON"),
            w!("Fetch"),
            WINDOW_STYLE(WS_TABSTOP.0 | BS_PUSHBUTTON as u32),
            WINDOW_EX_STYLE::default(),
            488,
            15,
            92,
            26,
            G_FETCH,
        );

        let list = mk(
            w!("SysListView32"),
            PCWSTR::null(),
            WINDOW_STYLE(WS_BORDER.0 | WS_TABSTOP.0 | LVS_REPORT),
            WINDOW_EX_STYLE::default(),
            16,
            52,
            564,
            300,
            G_LIST,
        );
        *G_LIST_HWND.lock().unwrap() = list.0 as isize;
        SendMessageW(
            list,
            LVM_SETEXTENDEDLISTVIEWSTYLE,
            Some(WPARAM((LVS_EX_CHECKBOXES | LVS_EX_FULLROWSELECT) as usize)),
            Some(LPARAM((LVS_EX_CHECKBOXES | LVS_EX_FULLROWSELECT) as isize)),
        );
        let col = LVCOLUMNW {
            mask: LVCF_TEXT | LVCF_WIDTH,
            cx: 540,
            pszText: PWSTR(w!("URL").as_ptr() as *mut u16),
            ..Default::default()
        };
        SendMessageW(list, LVM_INSERTCOLUMNW, Some(WPARAM(0)), Some(LPARAM(&col as *const _ as isize)));

        mk(
            w!("BUTTON"),
            w!("Download selected"),
            WINDOW_STYLE(WS_TABSTOP.0 | BS_DEFPUSHBUTTON as u32),
            WINDOW_EX_STYLE::default(),
            360,
            364,
            150,
            30,
            G_OK,
        );
        mk(
            w!("BUTTON"),
            w!("Cancel"),
            WINDOW_STYLE(WS_TABSTOP.0 | BS_PUSHBUTTON as u32),
            WINDOW_EX_STYLE::default(),
            516,
            364,
            64,
            30,
            G_CANCEL,
        );

        let _ = EnableWindow(parent, false);
        let _ = ShowWindow(dlg, SW_SHOW);
        let _ = SetForegroundWindow(dlg);
        let _ = SetFocus(Some(url));

        let mut msg = MSG::default();
        while !G_DONE.load(Ordering::SeqCst) && GetMessageW(&mut msg, None, 0, 0).as_bool() {
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
        std::mem::take(&mut *G_RESULT.lock().unwrap())
    }
}

unsafe fn grab_read_url() -> String {
    let h = HWND(*G_URL_HWND.lock().unwrap() as *mut core::ffi::c_void);
    let len = GetWindowTextLengthW(h);
    if len <= 0 {
        return String::new();
    }
    let mut buf = vec![0u16; len as usize + 1];
    let n = GetWindowTextW(h, &mut buf);
    String::from_utf16_lossy(&buf[..n as usize]).trim().to_string()
}

unsafe fn grab_set_title(text: &str) {
    let dlg = HWND(*G_DLG.lock().unwrap() as *mut core::ffi::c_void);
    let h = HSTRING::from(text);
    let _ = SetWindowTextW(dlg, PCWSTR(h.as_ptr()));
}

extern "system" fn grab_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_COMMAND => {
                let id = wparam.0 & 0xFFFF;
                match id {
                    G_FETCH => {
                        let url = grab_read_url();
                        if url.starts_with("http://") || url.starts_with("https://") {
                            grab_set_title("Run site grabber — mengambil…");
                            if let Some(eng) = crate::gui::engine() {
                                let dlg_isize = hwnd.0 as isize;
                                eng.runtime().spawn(async move {
                                    let links = adm_core::grab_links(&url).await.unwrap_or_default();
                                    let boxed = Box::into_raw(Box::new(links));
                                    let _ = PostMessageW(
                                        Some(HWND(dlg_isize as *mut core::ffi::c_void)),
                                        WM_GRAB_RESULT,
                                        WPARAM(0),
                                        LPARAM(boxed as isize),
                                    );
                                });
                            }
                        }
                        LRESULT(0)
                    }
                    G_OK => {
                        *G_RESULT.lock().unwrap() = grab_checked();
                        G_DONE.store(true, Ordering::SeqCst);
                        let _ = DestroyWindow(hwnd);
                        LRESULT(0)
                    }
                    G_CANCEL => {
                        G_DONE.store(true, Ordering::SeqCst);
                        let _ = DestroyWindow(hwnd);
                        LRESULT(0)
                    }
                    _ => DefWindowProcW(hwnd, msg, wparam, lparam),
                }
            }
            m if m == WM_GRAB_RESULT => {
                let ptr = lparam.0 as *mut Vec<String>;
                if !ptr.is_null() {
                    let links = *Box::from_raw(ptr);
                    grab_populate(&links);
                    let n = links.len();
                    *G_LINKS.lock().unwrap() = links;
                    grab_set_title(&format!("Run site grabber — {n} tautan ditemukan"));
                }
                LRESULT(0)
            }
            WM_CLOSE => {
                G_DONE.store(true, Ordering::SeqCst);
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

/// Isi listview dengan tautan; semua dicentang secara default.
unsafe fn grab_populate(links: &[String]) {
    let lv = HWND(*G_LIST_HWND.lock().unwrap() as *mut core::ffi::c_void);
    SendMessageW(lv, LVM_DELETEALLITEMS, Some(WPARAM(0)), Some(LPARAM(0)));
    for (i, u) in links.iter().enumerate() {
        let h = HSTRING::from(u.as_str());
        let item = LVITEMW {
            mask: LVIF_TEXT,
            iItem: i as i32,
            pszText: PWSTR(h.as_ptr() as *mut u16),
            ..Default::default()
        };
        SendMessageW(lv, LVM_INSERTITEMW, Some(WPARAM(0)), Some(LPARAM(&item as *const _ as isize)));
        // Centang default (state image index 2 → bit 12..16 = 2).
        let st = LVITEMW {
            mask: LVIF_STATE,
            iItem: i as i32,
            stateMask: LVIS_STATEIMAGEMASK,
            state: LIST_VIEW_ITEM_STATE_FLAGS(0x2000),
            ..Default::default()
        };
        SendMessageW(lv, LVM_SETITEMSTATE, Some(WPARAM(i)), Some(LPARAM(&st as *const _ as isize)));
    }
}

/// Kumpulkan URL pada baris yang tercentang.
unsafe fn grab_checked() -> Vec<String> {
    let lv = HWND(*G_LIST_HWND.lock().unwrap() as *mut core::ffi::c_void);
    let links = G_LINKS.lock().unwrap();
    let count = SendMessageW(lv, LVM_GETITEMCOUNT, Some(WPARAM(0)), Some(LPARAM(0))).0;
    let mut out = Vec::new();
    for i in 0..count {
        let st = SendMessageW(
            lv,
            LVM_GETITEMSTATE,
            Some(WPARAM(i as usize)),
            Some(LPARAM(LVIS_STATEIMAGEMASK.0 as isize)),
        );
        let checked = ((st.0 as u32) >> 12) == 2;
        if checked {
            if let Some(u) = links.get(i as usize) {
                out.push(u.clone());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_urls_from_text() {
        let t = "lihat http://a.com/x.zip dan \"https://b.com/y.rar\", juga ftp://no.com";
        assert_eq!(
            extract_urls(t),
            vec!["http://a.com/x.zip".to_string(), "https://b.com/y.rar".to_string()]
        );
    }

    #[test]
    fn extract_urls_dedup() {
        let t = "http://a.com/x http://a.com/x";
        assert_eq!(extract_urls(t).len(), 1);
    }

    #[test]
    fn expand_simple_range() {
        let v = expand_pattern("http://s/f[1-3].zip");
        assert_eq!(v, vec!["http://s/f1.zip", "http://s/f2.zip", "http://s/f3.zip"]);
    }

    #[test]
    fn expand_zero_padded() {
        let v = expand_pattern("http://s/f[08-10].bin");
        assert_eq!(v, vec!["http://s/f08.bin", "http://s/f09.bin", "http://s/f10.bin"]);
    }

    #[test]
    fn expand_two_ranges_cartesian() {
        let v = expand_pattern("http://s/[1-2]/p[1-2].dat");
        assert_eq!(
            v,
            vec![
                "http://s/1/p1.dat",
                "http://s/1/p2.dat",
                "http://s/2/p1.dat",
                "http://s/2/p2.dat",
            ]
        );
    }

    #[test]
    fn expand_no_range_passthrough() {
        assert_eq!(expand_pattern("http://s/file.zip"), vec!["http://s/file.zip"]);
    }

    #[test]
    fn parse_batch_lines_and_patterns() {
        let t = "http://s/a.zip\n  \nhttp://s/f[1-2].bin\nbukan-url\n";
        assert_eq!(
            parse_batch(t),
            vec!["http://s/a.zip", "http://s/f1.bin", "http://s/f2.bin"]
        );
    }
}
