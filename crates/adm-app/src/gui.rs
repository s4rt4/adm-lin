//! GUI Win32 + tray (plan §9, §12). WM2: jendela, tray Shell_NotifyIcon,
//! minimize-to-tray, menu tray (Show/Hide, Pause All, Stop All, autostart,
//! Exit), marshalling progres engine ke UI thread.

use crate::engine::{EngineEvent, EngineHandle, EventSink};
use crate::{autostart, state};
use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

static ENGINE: OnceLock<EngineHandle> = OnceLock::new();

const TRAY_UID: u32 = 1;
const IDM_SHOW: usize = 101;
const IDM_PAUSE_ALL: usize = 102;
const IDM_STOP_ALL: usize = 103;
const IDM_AUTOSTART: usize = 104;
const IDM_EXIT: usize = 105;

/// Daftarkan engine agar bisa diakses WndProc (Pause/Stop All).
pub fn set_engine(engine: EngineHandle) {
    let _ = ENGINE.set(engine);
}

/// EventSink untuk GUI: simpan progres ke atomics + post ke UI thread.
pub fn make_sink() -> EventSink {
    Arc::new(|ev: EngineEvent| {
        match &ev {
            EngineEvent::Started { id, output, .. } => {
                state::ACTIVE.fetch_add(1, Ordering::SeqCst);
                eprintln!("[engine] #{id} mulai -> {}", output.display());
            }
            EngineEvent::Progress { downloaded, total, speed_bps, .. } => {
                state::DOWNLOADED.store(*downloaded, Ordering::SeqCst);
                state::TOTAL.store(total.unwrap_or(0), Ordering::SeqCst);
                state::SPEED.store(*speed_bps, Ordering::SeqCst);
            }
            EngineEvent::Completed { id, bytes } => {
                state::ACTIVE.fetch_sub(1, Ordering::SeqCst);
                eprintln!("[engine] #{id} selesai ({bytes} byte)");
            }
            EngineEvent::Paused { id, downloaded } => {
                state::ACTIVE.fetch_sub(1, Ordering::SeqCst);
                eprintln!("[engine] #{id} paused ({downloaded} byte)");
            }
            EngineEvent::Failed { id, error } => {
                state::ACTIVE.fetch_sub(1, Ordering::SeqCst);
                eprintln!("[engine] #{id} GAGAL: {error}");
            }
        }
        state::post_to_ui(state::WM_PROGRESS);
    })
}

pub fn run(start_hidden: bool) -> windows::core::Result<()> {
    unsafe {
        let instance: HINSTANCE = GetModuleHandleW(None)?.into();

        let icc = INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_STANDARD_CLASSES
                | ICC_BAR_CLASSES
                | ICC_LISTVIEW_CLASSES
                | ICC_TREEVIEW_CLASSES
                | ICC_TAB_CLASSES
                | ICC_PROGRESS_CLASS,
        };
        let _ = InitCommonControlsEx(&icc);

        let class_name = w!("AdmMainWindow");
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: instance,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hbrBackground: HBRUSH(GetStockObject(WHITE_BRUSH).0),
            lpszClassName: class_name,
            ..Default::default()
        };
        let atom = RegisterClassW(&wc);
        debug_assert!(atom != 0, "RegisterClassW gagal");

        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class_name,
            w!("Alpha Download Manager"),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            980,
            600,
            None,
            None,
            Some(instance),
            None,
        )?;
        state::set_main_hwnd(hwnd);
        add_tray_icon(hwnd);

        if !start_hidden {
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = UpdateWindow(hwnd);
        }

        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        Ok(())
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            // Minimize-to-tray: tutup jendela = sembunyikan, proses tetap hidup.
            WM_CLOSE => {
                let _ = ShowWindow(hwnd, SW_HIDE);
                LRESULT(0)
            }
            WM_DESTROY => {
                remove_tray_icon(hwnd);
                PostQuitMessage(0);
                LRESULT(0)
            }
            state::WM_PROGRESS => {
                update_title(hwnd);
                LRESULT(0)
            }
            state::WM_ACTIVATE_APP => {
                show_window(hwnd);
                LRESULT(0)
            }
            state::WM_TRAY => {
                let event = (lparam.0 as u32) & 0xFFFF;
                match event {
                    e if e == WM_RBUTTONUP || e == WM_CONTEXTMENU => show_tray_menu(hwnd),
                    e if e == WM_LBUTTONDBLCLK => toggle_window(hwnd),
                    _ => {}
                }
                LRESULT(0)
            }
            WM_COMMAND => {
                let id = wparam.0 & 0xFFFF;
                handle_command(hwnd, id);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

fn handle_command(hwnd: HWND, id: usize) {
    match id {
        IDM_SHOW => toggle_window(hwnd),
        IDM_PAUSE_ALL | IDM_STOP_ALL => {
            if let Some(e) = ENGINE.get() {
                e.cancel_all();
            }
        }
        IDM_AUTOSTART => {
            let _ = autostart::toggle();
        }
        IDM_EXIT => request_exit(hwnd),
        _ => {}
    }
}

fn request_exit(hwnd: HWND) {
    let active = ENGINE.get().map(|e| e.active_count()).unwrap_or(0);
    unsafe {
        if active > 0 {
            let text = HSTRING::from(format!(
                "Ada {active} unduhan aktif. Keluar dari ADM?"
            ));
            let r = MessageBoxW(
                Some(hwnd),
                PCWSTR(text.as_ptr()),
                w!("Alpha Download Manager"),
                MB_YESNO | MB_ICONQUESTION,
            );
            if r != IDYES {
                return;
            }
        }
        let _ = DestroyWindow(hwnd);
    }
}

fn show_window(hwnd: HWND) {
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
}

fn toggle_window(hwnd: HWND) {
    unsafe {
        if IsWindowVisible(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_HIDE);
        } else {
            show_window(hwnd);
        }
    }
}

fn update_title(hwnd: HWND) {
    let downloaded = state::DOWNLOADED.load(Ordering::SeqCst);
    let total = state::TOTAL.load(Ordering::SeqCst);
    let speed = state::SPEED.load(Ordering::SeqCst);
    let active = state::ACTIVE.load(Ordering::SeqCst);

    let title = if active == 0 {
        "Alpha Download Manager".to_string()
    } else {
        let pct = match (downloaded * 100).checked_div(total) {
            Some(p) => format!("{p}%"),
            None => "…".to_string(),
        };
        format!(
            "Alpha Download Manager — {pct} ({:.1} MB/s) [{active} aktif]",
            speed as f64 / (1024.0 * 1024.0)
        )
    };
    let h = HSTRING::from(title);
    unsafe {
        let _ = SetWindowTextW(hwnd, PCWSTR(h.as_ptr()));
    }
}

fn show_tray_menu(hwnd: HWND) {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else {
            return;
        };
        let _ = AppendMenuW(menu, MF_STRING, IDM_SHOW, w!("Show / Hide"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, IDM_PAUSE_ALL, w!("Pause All"));
        let _ = AppendMenuW(menu, MF_STRING, IDM_STOP_ALL, w!("Stop All"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let autostart_flags = if autostart::is_enabled() {
            MF_STRING | MF_CHECKED
        } else {
            MF_STRING
        };
        let _ = AppendMenuW(menu, autostart_flags, IDM_AUTOSTART, w!("Start with Windows"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, IDM_EXIT, w!("Exit"));

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        // Diperlukan agar menu hilang saat klik di luar.
        let _ = SetForegroundWindow(hwnd);
        let _ = TrackPopupMenu(menu, TPM_RIGHTBUTTON, pt.x, pt.y, Some(0), hwnd, None);
        let _ = DestroyMenu(menu);
    }
}

fn tray_data(hwnd: HWND) -> NOTIFYICONDATAW {
    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_UID,
        ..Default::default()
    };
    nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    nid.uCallbackMessage = state::WM_TRAY;
    nid.hIcon = unsafe { LoadIconW(None, IDI_APPLICATION).unwrap_or_default() };

    let tip: Vec<u16> = "Alpha Download Manager".encode_utf16().collect();
    for (i, c) in tip.iter().enumerate().take(nid.szTip.len() - 1) {
        nid.szTip[i] = *c;
    }
    nid
}

fn add_tray_icon(hwnd: HWND) {
    let nid = tray_data(hwnd);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_ADD, &nid);
    }
}

fn remove_tray_icon(hwnd: HWND) {
    let nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_UID,
        ..Default::default()
    };
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
    }
}
