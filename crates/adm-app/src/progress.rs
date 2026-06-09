//! Dialog progres unduhan (plan §9.11–9.13) + dialog "Download complete"
//! (§9.14). Modeless, refresh via timer dari `store`; SegmentBar custom-draw
//! menggambar progres per koneksi.

use crate::store::{self, Row, Status};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use windows::core::{w, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const PROGRESS_CLASS: PCWSTR = w!("AdmProgressDialog");
const SEGBAR_CLASS: PCWSTR = w!("AdmSegmentBar");
static REG: AtomicBool = AtomicBool::new(false);

const IDC_PAUSE: usize = 1;
const IDC_CANCEL: usize = 2;
const IDC_SL_CHECK: usize = 10;
const IDC_SL_EDIT: usize = 11;
const TIMER_ID: usize = 1;

// Warna segmen (palet beragam) untuk porsi terunduh.
const COLORS: [(u8, u8, u8); 6] = [
    (59, 91, 67),    // hijau logo
    (217, 180, 4),   // emas logo
    (70, 130, 180),
    (176, 96, 64),
    (120, 90, 160),
    (80, 150, 120),
];

fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF((r as u32) | ((g as u32) << 8) | ((b as u32) << 16))
}

struct DlgData {
    id: u64,
    lbl_status: HWND,
    lbl_size: HWND,
    lbl_downloaded: HWND,
    lbl_rate: HWND,
    lbl_timeleft: HWND,
    lbl_resume: HWND,
    progress: HWND,
    segbar: HWND,
    conn: HWND,
    btn_pause: HWND,
    tab: HWND,
    tabs: [Vec<HWND>; 3],
    chk_limit: HWND,
    edit_limit: HWND,
}

unsafe fn gui_font() -> HGDIOBJ {
    GetStockObject(DEFAULT_GUI_FONT)
}

#[allow(clippy::too_many_arguments)]
unsafe fn mk(
    parent: HWND,
    class: PCWSTR,
    text: PCWSTR,
    style: WINDOW_STYLE,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    id: usize,
) -> HWND {
    let instance: HINSTANCE = GetModuleHandleW(None).unwrap_or_default().into();
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
    SendMessageW(hwnd, WM_SETFONT, Some(WPARAM(gui_font().0 as usize)), Some(LPARAM(1)));
    hwnd
}

unsafe fn label(parent: HWND, text: &str, x: i32, y: i32, w: i32) -> HWND {
    let h = HSTRING::from(text);
    mk(parent, w!("STATIC"), PCWSTR(h.as_ptr()), WINDOW_STYLE(0), x, y, w, 18, 0)
}

fn set_text(hwnd: HWND, text: &str) {
    let h = HSTRING::from(text);
    unsafe {
        let _ = SetWindowTextW(hwnd, PCWSTR(h.as_ptr()));
    }
}

unsafe fn register_classes() {
    if REG.swap(true, Ordering::SeqCst) {
        return;
    }
    let instance: HINSTANCE = GetModuleHandleW(None).unwrap_or_default().into();
    let dlg = WNDCLASSW {
        lpfnWndProc: Some(dlg_proc),
        hInstance: instance,
        hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
        hbrBackground: HBRUSH((COLOR_BTNFACE.0 + 1) as *mut core::ffi::c_void),
        lpszClassName: PROGRESS_CLASS,
        ..Default::default()
    };
    RegisterClassW(&dlg);

    let seg = WNDCLASSW {
        lpfnWndProc: Some(segbar_proc),
        hInstance: instance,
        hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
        hbrBackground: HBRUSH((COLOR_WINDOW.0 + 1) as *mut core::ffi::c_void),
        lpszClassName: SEGBAR_CLASS,
        ..Default::default()
    };
    RegisterClassW(&seg);
}

/// Buka dialog progres modeless untuk unduhan `id`.
pub fn open(parent: HWND, id: u64) {
    unsafe {
        register_classes();
        let instance: HINSTANCE = GetModuleHandleW(None).unwrap_or_default().into();

        let row = store::get(id);
        let title = row
            .as_ref()
            .map(|r| r.filename())
            .unwrap_or_else(|| "Download".into());
        let htitle = HSTRING::from(title);

        let dlg = CreateWindowExW(
            WS_EX_DLGMODALFRAME,
            PROGRESS_CLASS,
            PCWSTR(htitle.as_ptr()),
            WS_POPUP | WS_CAPTION | WS_SYSMENU,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            560,
            470,
            Some(parent),
            None,
            Some(instance),
            None,
        );
        let Ok(dlg) = dlg else { return };

        // Tab control.
        let tab = mk(dlg, w!("SysTabControl32"), PCWSTR::null(), WINDOW_STYLE(0), 8, 8, 536, 400, 0);
        for (i, t) in ["Download status", "Speed Limiter", "Options on completion"].iter().enumerate() {
            let h = HSTRING::from(*t);
            let mut item = TCITEMW {
                mask: TCIF_TEXT,
                pszText: PWSTR(h.as_ptr() as *mut u16),
                ..Default::default()
            };
            SendMessageW(tab, TCM_INSERTITEMW, Some(WPARAM(i)), Some(LPARAM(&mut item as *mut _ as isize)));
        }

        // ---- Tab 1: Download status ----
        let mut t1 = Vec::new();
        t1.push(label(dlg, "URL:", 20, 44, 60));
        let lbl_url = label(dlg, "", 90, 44, 440);
        t1.push(lbl_url);
        t1.push(label(dlg, "Status:", 20, 70, 60));
        let lbl_status = label(dlg, "", 90, 70, 440);
        t1.push(lbl_status);
        t1.push(label(dlg, "File size:", 20, 96, 70));
        let lbl_size = label(dlg, "", 90, 96, 200);
        t1.push(lbl_size);
        t1.push(label(dlg, "Downloaded:", 20, 122, 80));
        let lbl_downloaded = label(dlg, "", 110, 122, 200);
        t1.push(lbl_downloaded);
        t1.push(label(dlg, "Transfer rate:", 20, 148, 90));
        let lbl_rate = label(dlg, "", 120, 148, 200);
        t1.push(lbl_rate);
        t1.push(label(dlg, "Time left:", 20, 174, 70));
        let lbl_timeleft = label(dlg, "", 110, 174, 200);
        t1.push(lbl_timeleft);
        t1.push(label(dlg, "Resume capability:", 300, 96, 110));
        let lbl_resume = label(dlg, "", 410, 96, 120);
        t1.push(lbl_resume);

        let progress = mk(dlg, w!("msctls_progress32"), PCWSTR::null(), WINDOW_STYLE(0), 20, 202, 510, 18, 0);
        SendMessageW(progress, PBM_SETRANGE32, Some(WPARAM(0)), Some(LPARAM(1000)));
        t1.push(progress);

        t1.push(label(dlg, "Start positions and download progress by connections:", 20, 228, 400));
        let segbar = CreateWindowExW(
            WS_EX_STATICEDGE,
            SEGBAR_CLASS,
            PCWSTR::null(),
            WS_CHILD | WS_VISIBLE,
            20, 248, 510, 34,
            Some(dlg),
            None,
            Some(instance),
            None,
        )
        .unwrap_or_default();
        SetWindowLongPtrW(segbar, GWLP_USERDATA, id as isize);
        t1.push(segbar);

        let conn = mk(
            dlg, w!("SysListView32"), PCWSTR::null(),
            WINDOW_STYLE(LVS_REPORT | LVS_NOSORTHEADER),
            20, 292, 510, 96, 0,
        );
        SendMessageW(conn, LVM_SETEXTENDEDLISTVIEWSTYLE, Some(WPARAM(0)), Some(LPARAM(LVS_EX_FULLROWSELECT as isize)));
        for (i, (t, wdt)) in [("N.", 40), ("Downloaded", 120), ("Info", 320)].iter().enumerate() {
            let mut wide: Vec<u16> = t.encode_utf16().chain(std::iter::once(0)).collect();
            let mut col = LVCOLUMNW {
                mask: LVCF_TEXT | LVCF_WIDTH | LVCF_SUBITEM,
                cx: *wdt,
                pszText: PWSTR(wide.as_mut_ptr()),
                iSubItem: i as i32,
                ..Default::default()
            };
            SendMessageW(conn, LVM_INSERTCOLUMNW, Some(WPARAM(i)), Some(LPARAM(&mut col as *mut _ as isize)));
        }
        t1.push(conn);

        // ---- Tab 2: Speed Limiter (kontrol kosmetik untuk WM4) ----
        let mut t2 = Vec::new();
        t2.push(label(dlg, "Use the speed limiter to reduce bandwidth usage.", 20, 50, 480));
        let chk_limit = mk(dlg, w!("BUTTON"), w!("Use Speed Limiter"), WINDOW_STYLE(BS_AUTOCHECKBOX as u32), 20, 80, 200, 20, IDC_SL_CHECK);
        t2.push(chk_limit);
        t2.push(label(dlg, "Maximum download speed:", 20, 112, 160));
        let edit_limit = mk(dlg, w!("EDIT"), w!("0"), WINDOW_STYLE(WS_BORDER.0 | ES_NUMBER as u32), 190, 110, 80, 22, IDC_SL_EDIT);
        t2.push(edit_limit);
        t2.push(label(dlg, "KBytes/sec", 280, 112, 80));
        t2.push(mk(dlg, w!("BUTTON"), w!("Remember setting on stop/resume"), WINDOW_STYLE(BS_AUTOCHECKBOX as u32), 20, 144, 300, 20, 0));
        for h in &t2 {
            let _ = ShowWindow(*h, SW_HIDE);
        }

        // ---- Tab 3: Options on completion (kosmetik untuk WM4) ----
        let mut t3 = Vec::new();
        t3.push(mk(dlg, w!("BUTTON"), w!("Show download complete dialog"), WINDOW_STYLE(BS_AUTOCHECKBOX as u32), 20, 50, 280, 20, 0));
        t3.push(mk(dlg, w!("BUTTON"), w!("Exit ADM when done"), WINDOW_STYLE(BS_AUTOCHECKBOX as u32), 20, 78, 280, 20, 0));
        t3.push(mk(dlg, w!("BUTTON"), w!("Turn off computer when done"), WINDOW_STYLE(BS_AUTOCHECKBOX as u32), 20, 106, 280, 20, 0));
        t3.push(label(dlg, "When done:", 20, 138, 80));
        let combo = mk(
            dlg, w!("COMBOBOX"), PCWSTR::null(),
            WINDOW_STYLE(CBS_DROPDOWNLIST as u32 | WS_VSCROLL.0),
            110, 136, 160, 200, 0,
        );
        for o in ["Shut down", "Hibernate", "Sleep", "Exit"] {
            let h = HSTRING::from(o);
            SendMessageW(combo, CB_ADDSTRING, Some(WPARAM(0)), Some(LPARAM(h.as_ptr() as isize)));
        }
        SendMessageW(combo, CB_SETCURSEL, Some(WPARAM(0)), Some(LPARAM(0)));
        t3.push(combo);
        for h in &t3 {
            let _ = ShowWindow(*h, SW_HIDE);
        }

        // Tombol bawah.
        let btn_pause = mk(dlg, w!("BUTTON"), w!("Pause"), WINDOW_STYLE(BS_PUSHBUTTON as u32), 330, 420, 100, 28, IDC_PAUSE);
        let _ = mk(dlg, w!("BUTTON"), w!("Cancel"), WINDOW_STYLE(BS_PUSHBUTTON as u32), 440, 420, 100, 28, IDC_CANCEL);

        let data = Box::new(DlgData {
            id,
            lbl_status,
            lbl_size,
            lbl_downloaded,
            lbl_rate,
            lbl_timeleft,
            lbl_resume,
            progress,
            segbar,
            conn,
            btn_pause,
            tab,
            tabs: [t1, t2, t3],
            chk_limit,
            edit_limit,
        });
        SetWindowLongPtrW(dlg, GWLP_USERDATA, Box::into_raw(data) as isize);

        if let Some(r) = &row {
            set_text(lbl_url, &r.url);
        }
        refresh(dlg);
        SetTimer(Some(dlg), TIMER_ID, 400, None);
        let _ = ShowWindow(dlg, SW_SHOW);
    }
}

unsafe fn data_of(hwnd: HWND) -> Option<&'static mut DlgData> {
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut DlgData;
    if ptr.is_null() {
        None
    } else {
        Some(&mut *ptr)
    }
}

unsafe fn show_tab(d: &DlgData, sel: usize) {
    for (i, controls) in d.tabs.iter().enumerate() {
        let cmd = if i == sel { SW_SHOW } else { SW_HIDE };
        for h in controls {
            let _ = ShowWindow(*h, cmd);
        }
    }
}

unsafe fn refresh(dlg: HWND) {
    let Some(d) = data_of(dlg) else { return };
    let Some(r) = store::get(d.id) else {
        // Baris dihapus → tutup dialog.
        let _ = DestroyWindow(dlg);
        return;
    };

    set_text(d.lbl_status, &status_line(&r));
    set_text(d.lbl_size, &fmt_size(r.size));
    let pct = r.size.and_then(|t| (r.downloaded * 100).checked_div(t)).unwrap_or(0);
    set_text(d.lbl_downloaded, &format!("{} ({pct}%)", fmt_size(Some(r.downloaded))));
    set_text(d.lbl_rate, &fmt_speed(r.speed_bps));
    set_text(d.lbl_timeleft, &fmt_eta(r.eta_secs()));
    set_text(d.lbl_resume, if r.segments.is_empty() { "No" } else { "Yes" });

    let permille = r.size.and_then(|t| (r.downloaded * 1000).checked_div(t)).unwrap_or(0);
    SendMessageW(d.progress, PBM_SETPOS, Some(WPARAM(permille as usize)), Some(LPARAM(0)));

    set_text(d.btn_pause, if r.status == Status::Downloading { "Pause" } else { "Resume" });

    // Tabel koneksi.
    let count = SendMessageW(d.conn, LVM_GETITEMCOUNT, Some(WPARAM(0)), Some(LPARAM(0))).0 as usize;
    if count != r.segments.len() {
        SendMessageW(d.conn, LVM_DELETEALLITEMS, Some(WPARAM(0)), Some(LPARAM(0)));
        for i in 0..r.segments.len() {
            let mut wide: Vec<u16> = format!("{}", i + 1).encode_utf16().chain(std::iter::once(0)).collect();
            let mut lvi = LVITEMW {
                mask: LVIF_TEXT,
                iItem: i as i32,
                pszText: PWSTR(wide.as_mut_ptr()),
                ..Default::default()
            };
            SendMessageW(d.conn, LVM_INSERTITEMW, Some(WPARAM(0)), Some(LPARAM(&mut lvi as *mut _ as isize)));
        }
    }
    for (i, (start, end, dl)) in r.segments.iter().enumerate() {
        let len = end - start + 1;
        let seg_pct = (dl * 100).checked_div(len).unwrap_or(0);
        conn_set(d.conn, i as i32, 1, &fmt_size(Some(*dl)));
        conn_set(d.conn, i as i32, 2, &format!("{seg_pct}%"));
    }

    let _ = InvalidateRect(Some(d.segbar), None, true);
}

unsafe fn conn_set(lv: HWND, item: i32, sub: i32, text: &str) {
    let mut wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let mut lvi = LVITEMW {
        iSubItem: sub,
        pszText: PWSTR(wide.as_mut_ptr()),
        ..Default::default()
    };
    SendMessageW(lv, LVM_SETITEMTEXTW, Some(WPARAM(item as usize)), Some(LPARAM(&mut lvi as *mut _ as isize)));
}

extern "system" fn dlg_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_TIMER => {
                refresh(hwnd);
                LRESULT(0)
            }
            WM_NOTIFY => {
                let hdr = &*(lparam.0 as *const NMHDR);
                if let Some(d) = data_of(hwnd) {
                    if hdr.hwndFrom == d.tab && hdr.code == TCN_SELCHANGE {
                        let sel = SendMessageW(d.tab, TCM_GETCURSEL, Some(WPARAM(0)), Some(LPARAM(0))).0 as usize;
                        show_tab(d, sel);
                    }
                }
                LRESULT(0)
            }
            WM_CTLCOLORSTATIC => {
                SetBkMode(HDC(wparam.0 as *mut _), TRANSPARENT);
                LRESULT(GetSysColorBrush(COLOR_BTNFACE).0 as isize)
            }
            WM_COMMAND => {
                let id = wparam.0 & 0xFFFF;
                let code = (wparam.0 >> 16) & 0xFFFF;
                if let Some(d) = data_of(hwnd) {
                    let did = d.id;
                    match id {
                        IDC_SL_CHECK => apply_limit(hwnd),
                        IDC_SL_EDIT if code == EN_KILLFOCUS as usize => apply_limit(hwnd),
                        IDC_PAUSE => {
                            if let Some(e) = crate::gui::engine() {
                                if let Some(r) = store::get(did) {
                                    if r.status == Status::Downloading {
                                        e.cancel(did);
                                    } else {
                                        let f = r.filename();
                                        e.resume(did, r.url, f);
                                    }
                                }
                            }
                        }
                        IDC_CANCEL => {
                            if let Some(e) = crate::gui::engine() {
                                e.cancel(did);
                            }
                            let _ = DestroyWindow(hwnd);
                        }
                        _ => {}
                    }
                }
                LRESULT(0)
            }
            WM_CLOSE => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_NCDESTROY => {
                let _ = KillTimer(Some(hwnd), TIMER_ID);
                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut DlgData;
                if !ptr.is_null() {
                    drop(Box::from_raw(ptr));
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                }
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

/// Terapkan batas kecepatan per-unduhan dari tab Speed Limiter.
unsafe fn apply_limit(hwnd: HWND) {
    let Some(d) = data_of(hwnd) else { return };
    let checked = SendMessageW(d.chk_limit, BM_GETCHECK, Some(WPARAM(0)), Some(LPARAM(0))).0 == 1;
    let kb = read_uint(d.edit_limit);
    let bps = if checked { kb * 1024 } else { 0 };
    if let Some(e) = crate::gui::engine() {
        e.set_limit(d.id, bps);
    }
}

unsafe fn read_uint(hwnd: HWND) -> u64 {
    let len = GetWindowTextLengthW(hwnd);
    if len <= 0 {
        return 0;
    }
    let mut buf = vec![0u16; len as usize + 1];
    let n = GetWindowTextW(hwnd, &mut buf);
    String::from_utf16_lossy(&buf[..n as usize]).trim().parse().unwrap_or(0)
}

// ============================ SegmentBar ============================

extern "system" fn segbar_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_PAINT => {
                let id = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as u64;
                paint_segbar(hwnd, id);
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

unsafe fn paint_segbar(hwnd: HWND, id: u64) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let w = (rc.right - rc.left).max(1);
    let h = rc.bottom - rc.top;

    // Latar.
    let bg = CreateSolidBrush(rgb(245, 245, 245));
    FillRect(hdc, &rc, bg);
    let _ = DeleteObject(bg.into());

    let pending = CreateSolidBrush(rgb(214, 214, 214));

    if let Some(r) = store::get(id) {
        if let Some(total) = r.size.filter(|t| *t > 0) {
            for (i, (start, end, dl)) in r.segments.iter().enumerate() {
                let len = (end - start + 1) as i64;
                let x0 = (*start as i64 * w as i64 / total as i64) as i32;
                let x1 = ((*end as i64 + 1) * w as i64 / total as i64) as i32;
                let filled = if len > 0 {
                    (*dl as i64 * (x1 - x0) as i64 / len) as i32
                } else {
                    0
                };

                // Porsi pending.
                let mut rp = RECT { left: x0, top: 0, right: x1, bottom: h };
                FillRect(hdc, &rp, pending);
                // Porsi terunduh.
                let (cr, cg, cb) = COLORS[i % COLORS.len()];
                let fill = CreateSolidBrush(rgb(cr, cg, cb));
                rp.right = x0 + filled;
                FillRect(hdc, &rp, fill);
                let _ = DeleteObject(fill.into());

                // Pemisah segmen.
                let sep = CreateSolidBrush(rgb(140, 140, 140));
                let line = RECT { left: x1 - 1, top: 0, right: x1, bottom: h };
                FillRect(hdc, &line, sep);
                let _ = DeleteObject(sep.into());
            }
        }
    }
    let _ = DeleteObject(pending.into());
    let _ = EndPaint(hwnd, &ps);
}

// ============================ Download complete ============================

// Dialog "Download complete" (§9.14) — kustom, sesuai desain IDM.
const CMPL_CLASS: PCWSTR = w!("AdmCompleteDialog");
static CMPL_REG: AtomicBool = AtomicBool::new(false);
static CMPL_DONE: AtomicBool = AtomicBool::new(false);
static CMPL_PATH: Mutex<String> = Mutex::new(String::new());
static CMPL_DONTSHOW: Mutex<isize> = Mutex::new(0); // HWND checkbox

const IDB_OPEN: usize = 1;
const IDB_OPENWITH: usize = 2;
const IDB_OPENFOLDER: usize = 3;
const IDB_CLOSE: usize = 4;
const IDC_DONTSHOW: usize = 5;

/// Dialog modal "Download complete" (§9.14): Open / Open with… / Open folder /
/// Close + "Don't show this dialog again". Close hanya menutup (tak membuka file).
pub fn show_complete(parent: HWND, row: &Row) {
    unsafe {
        let instance: HINSTANCE = match GetModuleHandleW(None) {
            Ok(h) => h.into(),
            Err(_) => return,
        };
        if !CMPL_REG.swap(true, Ordering::SeqCst) {
            let wc = WNDCLASSW {
                lpfnWndProc: Some(cmpl_proc),
                hInstance: instance,
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                hbrBackground: HBRUSH((COLOR_BTNFACE.0 + 1) as *mut core::ffi::c_void),
                lpszClassName: CMPL_CLASS,
                ..Default::default()
            };
            RegisterClassW(&wc);
        }
        CMPL_DONE.store(false, Ordering::SeqCst);
        *CMPL_PATH.lock().unwrap() = row.output.to_string_lossy().into_owned();

        // Ukuran CLIENT yang diinginkan; window dibesarkan agar client = ini.
        let (cw, ch) = (520, 250);
        let style = WS_POPUP | WS_CAPTION | WS_SYSMENU;
        let mut rc = RECT { left: 0, top: 0, right: cw, bottom: ch };
        let _ = AdjustWindowRectEx(&mut rc, style, false, WS_EX_DLGMODALFRAME);
        let (dw, dh) = (rc.right - rc.left, rc.bottom - rc.top);

        let mut pr = RECT::default();
        let _ = GetWindowRect(parent, &mut pr);
        let x = (pr.left + ((pr.right - pr.left) - dw) / 2).max(0);
        let y = (pr.top + ((pr.bottom - pr.top) - dh) / 2).max(0);

        let dlg = CreateWindowExW(
            WS_EX_DLGMODALFRAME,
            CMPL_CLASS,
            w!("Download complete"),
            style,
            x, y, dw, dh,
            Some(parent),
            None,
            Some(instance),
            None,
        );
        let Ok(dlg) = dlg else { return };

        // Ikon info.
        let ico = mk(dlg, w!("STATIC"), PCWSTR::null(), WINDOW_STYLE(0x0000_0003), 16, 16, 32, 32, 0); // SS_ICON
        if let Ok(hic) = LoadIconW(None, IDI_INFORMATION) {
            SendMessageW(ico, STM_SETICON, Some(WPARAM(hic.0 as usize)), Some(LPARAM(0)));
        }

        let _ = mk(dlg, w!("STATIC"), w!("Download complete"), WINDOW_STYLE(0), 60, 16, 380, 18, 0);
        let sz = fmt_size(row.size);
        let bytes = row.size.unwrap_or(row.downloaded);
        let head = HSTRING::from(format!("Downloaded {sz} ({bytes} Bytes)"));
        let _ = mk(dlg, w!("STATIC"), PCWSTR(head.as_ptr()), WINDOW_STYLE(0), 60, 36, 440, 18, 0);

        let _ = mk(dlg, w!("STATIC"), w!("Address"), WINDOW_STYLE(0), 16, 62, 200, 16, 0);
        let url_edit = mk(
            dlg, w!("EDIT"), PCWSTR::null(),
            WINDOW_STYLE(WS_BORDER.0 | ES_AUTOHSCROLL as u32 | ES_READONLY as u32),
            16, 80, 488, 22, 0,
        );
        let hu = HSTRING::from(row.url.clone());
        let _ = SetWindowTextW(url_edit, PCWSTR(hu.as_ptr()));

        let _ = mk(dlg, w!("STATIC"), w!("The file saved as"), WINDOW_STYLE(0), 16, 110, 200, 16, 0);
        let path_edit = mk(
            dlg, w!("EDIT"), PCWSTR::null(),
            WINDOW_STYLE(WS_BORDER.0 | ES_AUTOHSCROLL as u32 | ES_READONLY as u32),
            16, 128, 488, 22, 0,
        );
        let hpth = HSTRING::from(row.output.to_string_lossy().into_owned());
        let _ = SetWindowTextW(path_edit, PCWSTR(hpth.as_ptr()));

        let _ = mk(dlg, w!("BUTTON"), w!("Open"), WINDOW_STYLE(WS_TABSTOP.0 | BS_PUSHBUTTON as u32), 16, 166, 100, 28, IDB_OPEN);
        let _ = mk(dlg, w!("BUTTON"), w!("Open with..."), WINDOW_STYLE(WS_TABSTOP.0 | BS_PUSHBUTTON as u32), 124, 166, 110, 28, IDB_OPENWITH);
        let _ = mk(dlg, w!("BUTTON"), w!("Open folder"), WINDOW_STYLE(WS_TABSTOP.0 | BS_PUSHBUTTON as u32), 242, 166, 110, 28, IDB_OPENFOLDER);
        let _ = mk(dlg, w!("BUTTON"), w!("Close"), WINDOW_STYLE(WS_TABSTOP.0 | BS_DEFPUSHBUTTON as u32), 404, 166, 100, 28, IDB_CLOSE);

        let chk = mk(dlg, w!("BUTTON"), w!("Don't show this dialog again"), WINDOW_STYLE(BS_AUTOCHECKBOX as u32), 16, 206, 260, 20, IDC_DONTSHOW);
        *CMPL_DONTSHOW.lock().unwrap() = chk.0 as isize;

        let _ = EnableWindow(parent, false);
        let _ = ShowWindow(dlg, SW_SHOW);
        let _ = SetForegroundWindow(dlg);

        let mut msg = MSG::default();
        while !CMPL_DONE.load(Ordering::SeqCst) && GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if !IsDialogMessageW(dlg, &msg).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }

        // "Don't show again" → simpan preferensi.
        let dont = {
            let h = *CMPL_DONTSHOW.lock().unwrap();
            h != 0 && SendMessageW(HWND(h as *mut core::ffi::c_void), BM_GETCHECK, Some(WPARAM(0)), Some(LPARAM(0))).0 == 1
        };
        if dont {
            crate::settings::update(|s| s.show_complete_dialog = false);
        }

        let _ = EnableWindow(parent, true);
        let _ = SetForegroundWindow(parent);
        if IsWindow(Some(dlg)).as_bool() {
            let _ = DestroyWindow(dlg);
        }
    }
}

extern "system" fn cmpl_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_COMMAND => {
                let id = wparam.0 & 0xFFFF;
                let path = CMPL_PATH.lock().unwrap().clone();
                let hp = HSTRING::from(path.clone());
                match id {
                    IDB_OPEN => {
                        ShellExecuteW(None, w!("open"), PCWSTR(hp.as_ptr()), None, None, SW_SHOWNORMAL);
                        CMPL_DONE.store(true, Ordering::SeqCst);
                        let _ = DestroyWindow(hwnd);
                    }
                    IDB_OPENWITH => {
                        // Verb "openas" → dialog "Open With" Windows.
                        ShellExecuteW(None, w!("openas"), PCWSTR(hp.as_ptr()), None, None, SW_SHOWNORMAL);
                        CMPL_DONE.store(true, Ordering::SeqCst);
                        let _ = DestroyWindow(hwnd);
                    }
                    IDB_OPENFOLDER => {
                        crate::gui::open_folder(std::path::Path::new(&path));
                        CMPL_DONE.store(true, Ordering::SeqCst);
                        let _ = DestroyWindow(hwnd);
                    }
                    IDB_CLOSE => {
                        CMPL_DONE.store(true, Ordering::SeqCst);
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
                CMPL_DONE.store(true, Ordering::SeqCst);
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

// ============================ Format ============================

fn status_line(r: &Row) -> String {
    match r.status {
        Status::Queued => "Queued".into(),
        Status::Downloading => "Receiving data...".into(),
        Status::Complete => "Complete".into(),
        Status::Paused => "Stopped".into(),
        Status::Error => "Error".into(),
    }
}

fn fmt_size(bytes: Option<u64>) -> String {
    match bytes {
        None => "?".into(),
        Some(b) => {
            let b = b as f64;
            if b >= 1024.0 * 1024.0 * 1024.0 {
                format!("{:.2} GB", b / (1024.0 * 1024.0 * 1024.0))
            } else if b >= 1024.0 * 1024.0 {
                format!("{:.2} MB", b / (1024.0 * 1024.0))
            } else if b >= 1024.0 {
                format!("{:.2} KB", b / 1024.0)
            } else {
                format!("{b:.0} B")
            }
        }
    }
}

fn fmt_speed(bps: u64) -> String {
    if bps == 0 {
        return "-".into();
    }
    let b = bps as f64;
    if b >= 1024.0 * 1024.0 {
        format!("{:.2} MB/s", b / (1024.0 * 1024.0))
    } else {
        format!("{:.1} KB/s", b / 1024.0)
    }
}

fn fmt_eta(secs: Option<u64>) -> String {
    match secs {
        None => "-".into(),
        Some(s) if s >= 3600 => format!("{} hr", s / 3600),
        Some(s) if s >= 60 => format!("{} min", s / 60),
        Some(s) => format!("{s} sec"),
    }
}
