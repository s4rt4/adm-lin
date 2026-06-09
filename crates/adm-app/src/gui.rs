//! GUI Win32 jendela utama (plan §9): menu, toolbar (+split ▾), TreeView
//! kategori, ListView unduhan, status bar, tray, context menu. Live dari engine.

use crate::engine::{EngineEvent, EngineHandle, EventSink};
use crate::{autostart, dialogs, state, store};
use adm_ipc::DownloadAddParams;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use windows::core::{w, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

static ENGINE: OnceLock<EngineHandle> = OnceLock::new();
/// Filter aktif dari sidebar kategori (kode = lParam node tree).
static FILTER: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
/// HMENU submenu Theme (untuk radio-check).
static THEME_MENU: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
/// Tema gelap aktif (dibaca WndProc untuk custom-draw).
static DARK: AtomicBool = AtomicBool::new(false);
static STATUS_TEXT: Mutex<String> = Mutex::new(String::new());
/// HMENU 5 popup menu utama (Tasks/File/Downloads/View/About).
static MENUS: Mutex<[isize; 5]> = Mutex::new([0; 5]);

// Dialog About (custom owner-draw).
static ABOUT_REG: AtomicBool = AtomicBool::new(false);
static ABOUT_DONE: AtomicBool = AtomicBool::new(false);
static ABOUT_ICON: Mutex<isize> = Mutex::new(0);
static ABOUT_LINK: Mutex<isize> = Mutex::new(0);
const ABOUT_VERSION: &str = "0.1.0";
const ABOUT_CLASS: PCWSTR = w!("AdmAboutDialog");
const IDC_ABOUT_OK: usize = 1;
const IDC_ABOUT_LINK: usize = 2;

// Palet tema gelap dari logo (plan §12).
const DARK_BG: (u8, u8, u8) = (26, 38, 32); // #1A2620
const DARK_SURFACE: (u8, u8, u8) = (36, 52, 48); // #243430
const DARK_TEXT: (u8, u8, u8) = (230, 236, 232); // #E6ECE8

// Tombol menu-strip (pengganti menu bar agar bisa gelap).
const ID_MENU_BASE: usize = 0x1A0; // ID_MENU_BASE+0..5

// Kode filter (lParam node tree).
const F_ALL: u8 = 0;
const F_COMPRESSED: u8 = 1;
const F_DOCUMENTS: u8 = 2;
const F_MUSIC: u8 = 3;
const F_PROGRAMS: u8 = 4;
const F_VIDEO: u8 = 5;
const F_UNFINISHED: u8 = 6;
const F_FINISHED: u8 = 7;
const F_GRABBER: u8 = 8;
const F_QUEUES: u8 = 9;

fn filter_match(filter: u8, r: &store::Row) -> bool {
    use crate::category::Category as C;
    use store::Status;
    match filter {
        F_ALL => true,
        F_COMPRESSED => r.category == C::Compressed,
        F_DOCUMENTS => r.category == C::Documents,
        F_MUSIC => r.category == C::Music,
        F_PROGRAMS => r.category == C::Programs,
        F_VIDEO => r.category == C::Video,
        F_UNFINISHED => r.status != Status::Complete,
        F_FINISHED => r.status == Status::Complete,
        F_GRABBER | F_QUEUES => false, // belum ada isi (WM6 lanjutan)
        _ => true,
    }
}

const TRAY_UID: u32 = 1;
const SIDEBAR_W: i32 = 190;

// ---- Command IDs ----
const ID_ADD: usize = 0x100;
const ID_ADD_BATCH: usize = 0x101;
const ID_ADD_BATCH_CLIP: usize = 0x102;
const ID_SITE_GRABBER: usize = 0x103;
const ID_EXPORT: usize = 0x105;
const ID_IMPORT: usize = 0x106;
const ID_EXIT: usize = 0x107;

const ID_STOP: usize = 0x110;
const ID_REMOVE: usize = 0x111;
const ID_DOWNLOAD_NOW: usize = 0x112;
const ID_REDOWNLOAD: usize = 0x113;

const ID_PAUSE_ALL: usize = 0x120;
const ID_STOP_ALL: usize = 0x121;
const ID_DELETE_COMPLETED: usize = 0x122;
const ID_FIND: usize = 0x123;
const ID_FIND_NEXT: usize = 0x124;
const ID_SCHEDULER: usize = 0x125;
const ID_START_QUEUE: usize = 0x126;
const ID_STOP_QUEUE: usize = 0x127;
const ID_OPTIONS: usize = 0x129;

// Speed Limiter global (preset).
const ID_SL_UNLIM: usize = 0x160;
const ID_SL_50: usize = 0x161;
const ID_SL_100: usize = 0x162;
const ID_SL_500: usize = 0x163;
const ID_SL_1M: usize = 0x164;
const ID_SL_5M: usize = 0x165;

const ID_HIDE_CATEGORIES: usize = 0x130;
const ID_ARRANGE: usize = 0x131;
const ID_TOOLBAR: usize = 0x132;
const ID_TRAY_ICON: usize = 0x133;
const ID_CUSTOMIZE: usize = 0x134;
const ID_THEME_DARK: usize = 0x135;
const ID_THEME_LIGHT: usize = 0x136;
const ID_THEME_SYSTEM: usize = 0x137;
const ID_FONT: usize = 0x138;
const ID_LANGUAGE: usize = 0x139;

const ID_ABOUT: usize = 0x142;

const ID_RESUME: usize = 0x150;
const ID_DELETE: usize = 0x151;
const ID_UPDATES: usize = 0x152;
const ID_OPEN: usize = 0x153;
const ID_OPEN_FOLDER: usize = 0x154;

// Move to category (6 item).
const ID_MOVE_BASE: usize = 0x180;

// Tray menu.
const ID_TRAY_SHOW: usize = 0x200;
const ID_TRAY_AUTOSTART: usize = 0x201;
const ID_TRAY_EXIT: usize = 0x202;

/// Ikon aplikasi (di-embed; dibuat dari logo.svg via tools/icongen).
const APP_ICO: &[u8] = include_bytes!("../assets/adm.ico");

/// Muat HICON dari .ico embedded pada ukuran terdekat. Entry 256px disimpan
/// PNG (tak didukung CreateIconFromResourceEx) → coba ukuran lalu fallback 48/32.
unsafe fn load_app_icon(cx: i32, _cy: i32) -> HICON {
    for &sz in &[cx, 48, 32] {
        let off = LookupIconIdFromDirectoryEx(APP_ICO.as_ptr(), true, sz, sz, LR_DEFAULTCOLOR);
        if off <= 0 {
            continue;
        }
        let data = &APP_ICO[off as usize..];
        if let Ok(h) = CreateIconFromResourceEx(data, true, 0x0003_0000, sz, sz, LR_DEFAULTCOLOR) {
            if !h.is_invalid() {
                return h;
            }
        }
    }
    LoadIconW(None, IDI_APPLICATION).unwrap_or_default()
}

pub fn set_engine(engine: EngineHandle) {
    let _ = ENGINE.set(engine);
}

/// Akses engine untuk modul lain (mis. dialog progres).
pub fn engine() -> Option<EngineHandle> {
    ENGINE.get().cloned()
}

/// Anti-duplikat: URL terakhir + waktunya (mencegah unduhan dobel saat browser
/// mengirim `download.add` 2x untuk satu klik).
static LAST_ADD: Mutex<Option<(String, std::time::Instant)>> = Mutex::new(None);

/// Dipanggil dari thread IPC (browser/bridge): titip unduhan ke UI thread untuk
/// dialog "Download File Info" (tidak langsung mulai). Menunggu window siap.
pub fn request_add(params: DownloadAddParams) {
    // Tolak duplikat URL yang sama dalam 3 detik.
    {
        let now = std::time::Instant::now();
        let mut last = LAST_ADD.lock().unwrap();
        if let Some((u, t)) = last.as_ref() {
            if *u == params.url && now.duration_since(*t) < std::time::Duration::from_secs(3) {
                return;
            }
        }
        *last = Some((params.url.clone(), now));
    }
    for _ in 0..60 {
        if state::load_hwnd(&state::MAIN_HWND).is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    if let Some(hwnd) = state::load_hwnd(&state::MAIN_HWND) {
        let ptr = Box::into_raw(Box::new(params));
        unsafe {
            if PostMessageW(Some(hwnd), state::WM_ADD_DOWNLOAD, WPARAM(0), LPARAM(ptr as isize)).is_err() {
                drop(Box::from_raw(ptr)); // post gagal → jangan bocor
            }
        }
    }
}

/// EventSink GUI: perbarui store + post ke UI thread.
pub fn make_sink() -> EventSink {
    Arc::new(|ev: EngineEvent| {
        match ev {
            EngineEvent::Queued { id, url, output } => {
                store::on_queued(id, url, output);
            }
            EngineEvent::Started { id, url, output } => {
                eprintln!("[engine] #{id} mulai -> {}", output.display());
                store::on_started(id, url, output);
            }
            EngineEvent::Renamed { id, output } => {
                store::set_output(id, output);
            }
            EngineEvent::Progress { id, downloaded, total, speed_bps, segments } => {
                store::on_progress(id, downloaded, total, speed_bps, segments);
            }
            EngineEvent::Completed { id, bytes } => {
                eprintln!("[engine] #{id} selesai ({bytes} byte)");
                store::set_status(id, store::Status::Complete);
            }
            EngineEvent::Paused { id, downloaded } => {
                eprintln!("[engine] #{id} stopped ({downloaded} byte)");
                store::set_status(id, store::Status::Paused);
            }
            EngineEvent::Failed { id, error } => {
                eprintln!("[engine] #{id} GAGAL: {error}");
                store::set_status(id, store::Status::Error);
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
            hIcon: load_app_icon(64, 64),
            hbrBackground: HBRUSH((COLOR_BTNFACE.0 + 1) as *mut core::ffi::c_void),
            lpszClassName: class_name,
            ..Default::default()
        };
        let _ = RegisterClassW(&wc);

        build_menus();
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
        state::store_hwnd(&state::MAIN_HWND, hwnd);

        // Ikon kecil & besar untuk taskbar/titlebar.
        let small = load_app_icon(GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON));
        // ICON_BIG dipakai taskbar/Alt-Tab. 64px = BMP (256 disimpan PNG → gagal).
        let big = load_app_icon(64, 64);
        SendMessageW(hwnd, WM_SETICON, Some(WPARAM(ICON_SMALL as usize)), Some(LPARAM(small.0 as isize)));
        SendMessageW(hwnd, WM_SETICON, Some(WPARAM(ICON_BIG as usize)), Some(LPARAM(big.0 as isize)));

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

// ============================ WndProc ============================

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_CREATE => {
                let instance: HINSTANCE = GetModuleHandleW(None).unwrap_or_default().into();
                create_children(hwnd, instance);
                apply_theme(hwnd);
                update_theme_checks();
                update_toolbar_state(); // mulai: tanpa seleksi
                layout(hwnd);
                LRESULT(0)
            }
            WM_SETTINGCHANGE => {
                // Tema sistem berubah → re-apply bila mode = System.
                if crate::settings::get().theme == crate::settings::THEME_SYSTEM {
                    apply_theme(hwnd);
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            WM_SIZE => {
                layout(hwnd);
                LRESULT(0)
            }
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
                refresh_ui(hwnd);
                LRESULT(0)
            }
            state::WM_ACTIVATE_APP => {
                show_window(hwnd);
                LRESULT(0)
            }
            state::WM_ADD_DOWNLOAD => {
                let ptr = lparam.0 as *mut DownloadAddParams;
                if !ptr.is_null() {
                    let params = *Box::from_raw(ptr);
                    show_window(hwnd); // munculkan dari tray bila perlu
                    show_add(hwnd, Some(params));
                }
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
            WM_NOTIFY => {
                let hdr = &*(lparam.0 as *const NMHDR);
                let from = Some(hdr.hwndFrom);
                if DARK.load(Ordering::SeqCst)
                    && hdr.code == NM_CUSTOMDRAW
                    && (from == state::load_hwnd(&state::TOOLBAR_HWND)
                        || from == state::load_hwnd(&state::MENUSTRIP_HWND))
                {
                    return toolbar_customdraw(lparam);
                }
                handle_notify(hwnd, lparam);
                LRESULT(0)
            }
            WM_ERASEBKGND => {
                if DARK.load(Ordering::SeqCst) {
                    let hdc = HDC(wparam.0 as *mut core::ffi::c_void);
                    let mut rc = RECT::default();
                    let _ = GetClientRect(hwnd, &mut rc);
                    let br = CreateSolidBrush(rgb(DARK_BG.0, DARK_BG.1, DARK_BG.2));
                    FillRect(hdc, &rc, br);
                    let _ = DeleteObject(br.into());
                    LRESULT(1)
                } else {
                    DefWindowProcW(hwnd, msg, wparam, lparam)
                }
            }
            WM_DRAWITEM => {
                let dis = &*(lparam.0 as *const DRAWITEMSTRUCT);
                if Some(dis.hwndItem) == state::load_hwnd(&state::STATUS_HWND) {
                    let br = CreateSolidBrush(rgb(DARK_SURFACE.0, DARK_SURFACE.1, DARK_SURFACE.2));
                    FillRect(dis.hDC, &dis.rcItem, br);
                    let _ = DeleteObject(br.into());
                    SetBkMode(dis.hDC, TRANSPARENT);
                    SetTextColor(dis.hDC, rgb(DARK_TEXT.0, DARK_TEXT.1, DARK_TEXT.2));
                    let text = STATUS_TEXT.lock().unwrap().clone();
                    let mut wide: Vec<u16> = text.encode_utf16().collect();
                    let mut rc = dis.rcItem;
                    rc.left += 6;
                    DrawTextW(dis.hDC, &mut wide, &mut rc, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
                    LRESULT(1)
                } else {
                    DefWindowProcW(hwnd, msg, wparam, lparam)
                }
            }
            WM_COMMAND => {
                handle_command(hwnd, wparam.0 & 0xFFFF);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

// ============================ Layout ============================

unsafe fn create_children(hwnd: HWND, instance: HINSTANCE) {
    // Menu strip (pengganti menu bar — bisa di-dark-kan via custom-draw).
    let ms = CreateWindowExW(
        WINDOW_EX_STYLE::default(),
        w!("ToolbarWindow32"),
        PCWSTR::null(),
        WS_CHILD | WS_VISIBLE | WINDOW_STYLE(TBSTYLE_FLAT | TBSTYLE_LIST | (CCS_NODIVIDER | CCS_NORESIZE | CCS_NOPARENTALIGN) as u32),
        0, 0, 0, 0,
        Some(hwnd),
        None,
        Some(instance),
        None,
    )
    .unwrap_or_default();
    SendMessageW(ms, TB_BUTTONSTRUCTSIZE, Some(WPARAM(std::mem::size_of::<TBBUTTON>())), Some(LPARAM(0)));
    let mut mb: Vec<TBBUTTON> = Vec::new();
    for (i, label) in ["Tasks", "File", "Downloads", "View", "About"].iter().enumerate() {
        mkbtn(&mut mb, ID_MENU_BASE + i, label, -2, false);
    }
    SendMessageW(ms, TB_ADDBUTTONSW, Some(WPARAM(mb.len())), Some(LPARAM(mb.as_ptr() as isize)));
    SendMessageW(ms, TB_AUTOSIZE, Some(WPARAM(0)), Some(LPARAM(0)));
    state::store_hwnd(&state::MENUSTRIP_HWND, ms);

    // Toolbar.
    let tb = CreateWindowExW(
        WINDOW_EX_STYLE::default(),
        w!("ToolbarWindow32"),
        PCWSTR::null(),
        WS_CHILD | WS_VISIBLE | WINDOW_STYLE(TBSTYLE_FLAT | TBSTYLE_TOOLTIPS | (CCS_NODIVIDER | CCS_NORESIZE | CCS_NOPARENTALIGN) as u32),
        0, 0, 0, 0,
        Some(hwnd),
        None,
        Some(instance),
        None,
    )
    .unwrap_or_default();
    SendMessageW(tb, TB_BUTTONSTRUCTSIZE, Some(WPARAM(std::mem::size_of::<TBBUTTON>())), Some(LPARAM(0)));
    SendMessageW(tb, TB_SETEXTENDEDSTYLE, Some(WPARAM(0)), Some(LPARAM((TBSTYLE_EX_MIXEDBUTTONS | TBSTYLE_EX_DRAWDDARROWS) as isize)));
    let himl = build_toolbar_imagelist(false);
    SendMessageW(tb, TB_SETIMAGELIST, Some(WPARAM(0)), Some(LPARAM(himl.0)));
    let diml = build_disabled_imagelist();
    SendMessageW(tb, TB_SETDISABLEDIMAGELIST, Some(WPARAM(0)), Some(LPARAM(diml.0)));
    add_toolbar_buttons(tb);
    SendMessageW(tb, TB_AUTOSIZE, Some(WPARAM(0)), Some(LPARAM(0)));
    state::store_hwnd(&state::TOOLBAR_HWND, tb);

    // TreeView kategori.
    let tv = CreateWindowExW(
        WS_EX_CLIENTEDGE,
        w!("SysTreeView32"),
        PCWSTR::null(),
        WS_CHILD | WS_VISIBLE | WINDOW_STYLE(TVS_HASLINES | TVS_HASBUTTONS | TVS_LINESATROOT | TVS_SHOWSELALWAYS),
        0, 0, 0, 0,
        Some(hwnd),
        None,
        Some(instance),
        None,
    )
    .unwrap_or_default();
    set_font(tv);
    populate_categories(tv);
    state::store_hwnd(&state::TREE_HWND, tv);

    // ListView unduhan.
    let lv = CreateWindowExW(
        WS_EX_CLIENTEDGE,
        w!("SysListView32"),
        PCWSTR::null(),
        WS_CHILD | WS_VISIBLE | WINDOW_STYLE(LVS_REPORT | LVS_SHOWSELALWAYS),
        0, 0, 0, 0,
        Some(hwnd),
        None,
        Some(instance),
        None,
    )
    .unwrap_or_default();
    set_font(lv);
    SendMessageW(
        lv,
        LVM_SETEXTENDEDLISTVIEWSTYLE,
        Some(WPARAM(0)),
        Some(LPARAM((LVS_EX_FULLROWSELECT | LVS_EX_GRIDLINES) as isize)),
    );
    add_list_columns(lv);
    state::store_hwnd(&state::LIST_HWND, lv);

    // Status bar.
    let sb = CreateWindowExW(
        WINDOW_EX_STYLE::default(),
        w!("msctls_statusbar32"),
        PCWSTR::null(),
        WS_CHILD | WS_VISIBLE | WINDOW_STYLE(SBARS_SIZEGRIP),
        0, 0, 0, 0,
        Some(hwnd),
        None,
        Some(instance),
        None,
    )
    .unwrap_or_default();
    state::store_hwnd(&state::STATUS_HWND, sb);
}

unsafe fn tb_height(tb: HWND) -> i32 {
    let s = SendMessageW(tb, TB_GETBUTTONSIZE, Some(WPARAM(0)), Some(LPARAM(0))).0;
    (((s >> 16) & 0xFFFF) as i32) + 6
}

unsafe fn layout(hwnd: HWND) {
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);

    let ms = state::load_hwnd(&state::MENUSTRIP_HWND);
    let tb = state::load_hwnd(&state::TOOLBAR_HWND);
    let sb = state::load_hwnd(&state::STATUS_HWND);
    let tv = state::load_hwnd(&state::TREE_HWND);
    let lv = state::load_hwnd(&state::LIST_HWND);

    let mut top = 0;
    if let Some(ms) = ms {
        let h = tb_height(ms);
        let _ = MoveWindow(ms, 0, top, rc.right, h, true);
        top += h;
    }
    if let Some(tb) = tb {
        let h = tb_height(tb);
        let _ = MoveWindow(tb, 0, top, rc.right, h, true);
        top += h;
    }
    let mut bottom = rc.bottom;
    if let Some(sb) = sb {
        SendMessageW(sb, WM_SIZE, Some(WPARAM(0)), Some(LPARAM(0)));
        let mut sr = RECT::default();
        let _ = GetWindowRect(sb, &mut sr);
        bottom = rc.bottom - (sr.bottom - sr.top);
    }

    let sidebar = state::SIDEBAR_VISIBLE.load(Ordering::SeqCst);
    let split = if sidebar { SIDEBAR_W } else { 0 };

    if let Some(tv) = tv {
        let _ = ShowWindow(tv, if sidebar { SW_SHOW } else { SW_HIDE });
        if sidebar {
            let _ = MoveWindow(tv, 0, top, split, bottom - top, true);
        }
    }
    if let Some(lv) = lv {
        let _ = MoveWindow(lv, split, top, rc.right - split, bottom - top, true);
    }
}

// ============================ Menu ============================

unsafe fn build_menus() {
    let tasks = CreatePopupMenu().unwrap();
    append(tasks, ID_ADD, w!("Add new download...\tCtrl+N"));
    append(tasks, ID_ADD_BATCH, w!("Add batch download..."));
    append(tasks, ID_ADD_BATCH_CLIP, w!("Add batch download from clipboard"));
    append(tasks, ID_SITE_GRABBER, w!("Run site grabber..."));
    sep(tasks);
    append(tasks, ID_EXPORT, w!("Export..."));
    append(tasks, ID_IMPORT, w!("Import..."));
    sep(tasks);
    append(tasks, ID_EXIT, w!("Exit\tCtrl+Q"));

    let file = CreatePopupMenu().unwrap();
    append(file, ID_STOP, w!("Stop Download"));
    append(file, ID_REMOVE, w!("Remove"));
    append(file, ID_DOWNLOAD_NOW, w!("Download Now"));
    append(file, ID_REDOWNLOAD, w!("Redownload"));

    let dl = CreatePopupMenu().unwrap();
    append(dl, ID_PAUSE_ALL, w!("Pause All"));
    append(dl, ID_STOP_ALL, w!("Stop All"));
    append(dl, ID_DELETE_COMPLETED, w!("Delete All Completed"));
    sep(dl);
    append(dl, ID_FIND, w!("Find...\tCtrl+F"));
    append(dl, ID_FIND_NEXT, w!("Find Next\tF3"));
    sep(dl);
    append(dl, ID_SCHEDULER, w!("Scheduler..."));
    append(dl, ID_START_QUEUE, w!("Start queue"));
    append(dl, ID_STOP_QUEUE, w!("Stop queue"));
    let sl = CreatePopupMenu().unwrap();
    append(sl, ID_SL_UNLIM, w!("Unlimited"));
    append(sl, ID_SL_50, w!("50 KB/s"));
    append(sl, ID_SL_100, w!("100 KB/s"));
    append(sl, ID_SL_500, w!("500 KB/s"));
    append(sl, ID_SL_1M, w!("1 MB/s"));
    append(sl, ID_SL_5M, w!("5 MB/s"));
    popup(dl, sl, w!("Speed Limiter"));
    sep(dl);
    append(dl, ID_OPTIONS, w!("Options..."));

    let view = CreatePopupMenu().unwrap();
    append(view, ID_HIDE_CATEGORIES, w!("Hide categories"));
    append(view, ID_ARRANGE, w!("Arrange files"));
    append(view, ID_TOOLBAR, w!("Toolbar"));
    append(view, ID_TRAY_ICON, w!("ADM tray icon"));
    append(view, ID_CUSTOMIZE, w!("Customize URL List..."));

    let about = CreatePopupMenu().unwrap();
    append(about, ID_ABOUT, w!("About ADM"));

    *MENUS.lock().unwrap() = [
        tasks.0 as isize,
        file.0 as isize,
        dl.0 as isize,
        view.0 as isize,
        about.0 as isize,
    ];
}

unsafe fn append(menu: HMENU, id: usize, text: PCWSTR) {
    let _ = AppendMenuW(menu, MF_STRING, id, text);
}
unsafe fn sep(menu: HMENU) {
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
}
unsafe fn popup(bar: HMENU, sub: HMENU, text: PCWSTR) {
    let _ = AppendMenuW(bar, MF_POPUP, sub.0 as usize, text);
}

// ============================ Toolbar ============================

fn mkbtn(buttons: &mut Vec<TBBUTTON>, id: usize, label: &str, icon: i32, dropdown: bool) {
    let mut wide: Vec<u16> = label.encode_utf16().collect();
    wide.push(0);
    let ptr = Box::leak(wide.into_boxed_slice()).as_ptr() as isize;
    let mut style: u32 = BTNS_AUTOSIZE | BTNS_SHOWTEXT;
    if dropdown {
        style |= BTNS_DROPDOWN; // klik tombol = aksi; panah = TBN_DROPDOWN
    }
    buttons.push(TBBUTTON {
        iBitmap: icon,
        idCommand: id as i32,
        fsState: TBSTATE_ENABLED as u8,
        fsStyle: style as u8,
        bReserved: [0; 6],
        dwData: 0,
        iString: ptr,
    });
}

fn mksep(buttons: &mut Vec<TBBUTTON>) {
    buttons.push(TBBUTTON {
        iBitmap: 0,
        idCommand: 0,
        fsState: TBSTATE_ENABLED as u8,
        fsStyle: BTNS_SEP as u8,
        bReserved: [0; 6],
        dwData: 0,
        iString: 0,
    });
}

unsafe fn add_toolbar_buttons(tb: HWND) {
    let mut buttons: Vec<TBBUTTON> = Vec::new();
    mkbtn(&mut buttons, ID_ADD, "Add URL", 0, false);
    mkbtn(&mut buttons, ID_RESUME, "Resume", 1, false);
    mkbtn(&mut buttons, ID_STOP, "Stop", 2, false);
    mkbtn(&mut buttons, ID_STOP_ALL, "Stop All", 3, false);
    mkbtn(&mut buttons, ID_DELETE, "Delete", 4, false);
    mkbtn(&mut buttons, ID_DELETE_COMPLETED, "Delete Completed", 5, false);
    mksep(&mut buttons);
    mkbtn(&mut buttons, ID_OPTIONS, "Options", 6, false);
    mkbtn(&mut buttons, ID_SCHEDULER, "Scheduler", 7, false);
    mksep(&mut buttons);
    mkbtn(&mut buttons, ID_START_QUEUE, "Start Queue", 8, true);
    mkbtn(&mut buttons, ID_STOP_QUEUE, "Stop Queue", 9, true);
    mksep(&mut buttons);
    mkbtn(&mut buttons, ID_UPDATES, "Updates", 10, false);

    SendMessageW(
        tb,
        TB_ADDBUTTONSW,
        Some(WPARAM(buttons.len())),
        Some(LPARAM(buttons.as_ptr() as isize)),
    );
}

// ============================ Categories ============================

unsafe fn tv_insert(tv: HWND, parent: HTREEITEM, text: PCWSTR, code: u8) -> HTREEITEM {
    let mut item = TVINSERTSTRUCTW {
        hParent: parent,
        hInsertAfter: TVI_LAST,
        ..Default::default()
    };
    item.Anonymous.item = TVITEMW {
        mask: TVIF_TEXT | TVIF_PARAM,
        pszText: PWSTR(text.0 as *mut u16),
        lParam: LPARAM(code as isize),
        ..Default::default()
    };
    let r = SendMessageW(tv, TVM_INSERTITEMW, Some(WPARAM(0)), Some(LPARAM(&item as *const _ as isize)));
    HTREEITEM(r.0)
}

unsafe fn populate_categories(tv: HWND) {
    let all = tv_insert(tv, TVI_ROOT, w!("All Downloads"), F_ALL);
    tv_insert(tv, all, w!("Compressed"), F_COMPRESSED);
    tv_insert(tv, all, w!("Documents"), F_DOCUMENTS);
    tv_insert(tv, all, w!("Music"), F_MUSIC);
    tv_insert(tv, all, w!("Programs"), F_PROGRAMS);
    tv_insert(tv, all, w!("Video"), F_VIDEO);
    let _ = SendMessageW(tv, TVM_EXPAND, Some(WPARAM(TVE_EXPAND.0 as usize)), Some(LPARAM(all.0)));
    tv_insert(tv, TVI_ROOT, w!("Unfinished"), F_UNFINISHED);
    tv_insert(tv, TVI_ROOT, w!("Finished"), F_FINISHED);
    tv_insert(tv, TVI_ROOT, w!("Grabber projects"), F_GRABBER);
    tv_insert(tv, TVI_ROOT, w!("Queues"), F_QUEUES);
}

// ============================ ListView ============================

unsafe fn add_list_columns(lv: HWND) {
    let cols: [(&str, i32); 8] = [
        ("File Name", 230),
        ("Q", 28),
        ("Size", 90),
        ("Status", 100),
        ("Time left", 90),
        ("Transfer rate", 100),
        ("Last Try", 110),
        ("Description", 120),
    ];
    for (i, (title, width)) in cols.iter().enumerate() {
        let mut wide: Vec<u16> = title.encode_utf16().collect();
        wide.push(0);
        let mut col = LVCOLUMNW {
            mask: LVCF_TEXT | LVCF_WIDTH | LVCF_SUBITEM,
            cx: *width,
            pszText: PWSTR(wide.as_mut_ptr()),
            iSubItem: i as i32,
            ..Default::default()
        };
        SendMessageW(lv, LVM_INSERTCOLUMNW, Some(WPARAM(i)), Some(LPARAM(&mut col as *mut _ as isize)));
    }
}

unsafe fn set_subitem(lv: HWND, item: i32, subitem: i32, text: &str) {
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    wide.push(0);
    let mut lvi = LVITEMW {
        iSubItem: subitem,
        pszText: PWSTR(wide.as_mut_ptr()),
        ..Default::default()
    };
    SendMessageW(lv, LVM_SETITEMTEXTW, Some(WPARAM(item as usize)), Some(LPARAM(&mut lvi as *mut _ as isize)));
}

unsafe fn insert_row(lv: HWND, item: i32, text: &str, id: u64) {
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    wide.push(0);
    let mut lvi = LVITEMW {
        mask: LVIF_TEXT | LVIF_PARAM,
        iItem: item,
        iSubItem: 0,
        pszText: PWSTR(wide.as_mut_ptr()),
        lParam: LPARAM(id as isize),
        ..Default::default()
    };
    SendMessageW(lv, LVM_INSERTITEMW, Some(WPARAM(0)), Some(LPARAM(&mut lvi as *mut _ as isize)));
}

/// id unduhan dari lParam item ListView (independen dari urutan/ filter).
unsafe fn item_id(lv: HWND, index: i32) -> Option<u64> {
    let mut lvi = LVITEMW {
        mask: LVIF_PARAM,
        iItem: index,
        ..Default::default()
    };
    let r = SendMessageW(lv, LVM_GETITEMW, Some(WPARAM(0)), Some(LPARAM(&mut lvi as *mut _ as isize)));
    if r.0 != 0 {
        Some(lvi.lParam.0 as u64)
    } else {
        None
    }
}

unsafe fn refresh_list(lv: HWND) {
    let filter = FILTER.load(Ordering::Relaxed);
    let visible: Vec<store::Row> =
        store::with_rows(|rows| rows.iter().filter(|r| filter_match(filter, r)).cloned().collect());

    let count = SendMessageW(lv, LVM_GETITEMCOUNT, Some(WPARAM(0)), Some(LPARAM(0))).0 as usize;
    let rebuild = count != visible.len();
    if rebuild {
        SendMessageW(lv, LVM_DELETEALLITEMS, Some(WPARAM(0)), Some(LPARAM(0)));
    }
    for (i, r) in visible.iter().enumerate() {
        let idx = i as i32;
        if rebuild {
            insert_row(lv, idx, &r.filename(), r.id);
        } else {
            set_subitem(lv, idx, 0, &r.filename());
        }
        set_subitem(lv, idx, 2, &fmt_size(r.size));
        set_subitem(lv, idx, 3, &status_text(r));
        set_subitem(lv, idx, 4, &fmt_eta(r.eta_secs()));
        set_subitem(lv, idx, 5, &fmt_speed(r.speed_bps));
    }
}

fn status_text(r: &store::Row) -> String {
    if r.status == store::Status::Downloading {
        if let Some(pct) = r.size.and_then(|t| (r.downloaded * 100).checked_div(t)) {
            return format!("{pct}%");
        }
        return "Downloading".into();
    }
    r.status.label().to_string()
}

// ============================ Notifications ============================

unsafe fn handle_notify(hwnd: HWND, lparam: LPARAM) {
    let hdr = &*(lparam.0 as *const NMHDR);
    let lv = state::load_hwnd(&state::LIST_HWND);
    let tb = state::load_hwnd(&state::TOOLBAR_HWND);
    let tv = state::load_hwnd(&state::TREE_HWND);

    if Some(hdr.hwndFrom) == tv && hdr.code == TVN_SELCHANGEDW {
        let nm = &*(lparam.0 as *const NMTREEVIEWW);
        FILTER.store(nm.itemNew.lParam.0 as u8, Ordering::Relaxed);
        refresh_ui(hwnd);
        return;
    }

    if Some(hdr.hwndFrom) == lv {
        match hdr.code {
            NM_DBLCLK => {
                let ia = &*(lparam.0 as *const NMITEMACTIVATE);
                if ia.iItem >= 0 {
                    on_dblclick(hwnd, ia.iItem);
                }
            }
            NM_RCLICK => {
                let ia = &*(lparam.0 as *const NMITEMACTIVATE);
                if ia.iItem >= 0 {
                    select_item(lv.unwrap(), ia.iItem);
                }
                show_context_menu(hwnd);
            }
            LVN_ITEMCHANGED => update_toolbar_state(), // seleksi berubah
            _ => {}
        }
    } else if Some(hdr.hwndFrom) == tb && hdr.code == TBN_DROPDOWN {
        // Dropdown ▾ pada Start/Stop Queue — popup contoh.
        let menu = CreatePopupMenu().unwrap_or_default();
        append(menu, 0, w!("(belum ada antrian)"));
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(hwnd);
        let _ = TrackPopupMenu(menu, TPM_LEFTALIGN, pt.x, pt.y, Some(0), hwnd, None);
        let _ = DestroyMenu(menu);
    }
}

unsafe fn select_item(lv: HWND, item: i32) {
    let lvi = LVITEMW {
        mask: LVIF_STATE,
        stateMask: LIST_VIEW_ITEM_STATE_FLAGS(LVIS_SELECTED.0 | LVIS_FOCUSED.0),
        state: LIST_VIEW_ITEM_STATE_FLAGS(LVIS_SELECTED.0 | LVIS_FOCUSED.0),
        ..Default::default()
    };
    SendMessageW(lv, LVM_SETITEMSTATE, Some(WPARAM(item as usize)), Some(LPARAM(&lvi as *const _ as isize)));
}

fn selected_index(lv: HWND) -> Option<i32> {
    unsafe {
        let r = SendMessageW(
            lv,
            LVM_GETNEXTITEM,
            Some(WPARAM(usize::MAX)), // -1 = dari awal
            Some(LPARAM(LVNI_SELECTED as isize)),
        );
        if r.0 < 0 {
            None
        } else {
            Some(r.0 as i32)
        }
    }
}

fn selected_id() -> Option<u64> {
    let lv = state::load_hwnd(&state::LIST_HWND)?;
    let idx = selected_index(lv)?;
    unsafe { item_id(lv, idx) }
}

// ============================ Commands ============================

unsafe fn handle_command(hwnd: HWND, id: usize) {
    match id {
        ID_ADD => do_add(hwnd),
        ID_RESUME | ID_DOWNLOAD_NOW => {
            if let (Some(id), Some(e)) = (selected_id(), ENGINE.get()) {
                if let Some(r) = store::get(id) {
                    let fname = r.filename();
                    e.resume(id, r.url, fname);
                }
            }
        }
        ID_STOP => {
            if let (Some(id), Some(e)) = (selected_id(), ENGINE.get()) {
                e.cancel(id);
            }
        }
        ID_PAUSE_ALL | ID_STOP_ALL => {
            if let Some(e) = ENGINE.get() {
                e.cancel_all();
            }
        }
        ID_REMOVE => remove_selected(hwnd, false),
        ID_DELETE => remove_selected(hwnd, true),
        ID_DELETE_COMPLETED => {
            store::remove_completed();
            refresh_ui(hwnd);
        }
        ID_OPEN => {
            if let Some(lv) = state::load_hwnd(&state::LIST_HWND) {
                if let Some(idx) = selected_index(lv) {
                    open_selected(hwnd, idx);
                }
            }
        }
        ID_OPEN_FOLDER => open_folder_selected(),
        ID_HIDE_CATEGORIES => {
            let v = !state::SIDEBAR_VISIBLE.load(Ordering::SeqCst);
            state::SIDEBAR_VISIBLE.store(v, Ordering::SeqCst);
            layout(hwnd);
        }
        ID_THEME_DARK => set_theme(hwnd, crate::settings::THEME_DARK),
        ID_THEME_LIGHT => set_theme(hwnd, crate::settings::THEME_LIGHT),
        ID_THEME_SYSTEM => set_theme(hwnd, crate::settings::THEME_SYSTEM),
        ID_ABOUT => show_about(hwnd),
        ID_EXIT => request_exit(hwnd),
        // Fitur milestone lain.
        ID_SCHEDULER => crate::scheduler::show(hwnd),
        ID_OPTIONS => crate::options::show(hwnd),
        ID_SL_UNLIM => set_global_limit(0),
        ID_SL_50 => set_global_limit(50 * 1024),
        ID_SL_100 => set_global_limit(100 * 1024),
        ID_SL_500 => set_global_limit(500 * 1024),
        ID_SL_1M => set_global_limit(1024 * 1024),
        ID_SL_5M => set_global_limit(5 * 1024 * 1024),
        ID_START_QUEUE => {
            if let Some(e) = ENGINE.get() {
                e.start_queue();
            }
            refresh_ui(hwnd);
        }
        ID_STOP_QUEUE => {
            if let Some(e) = ENGINE.get() {
                e.stop_queue();
            }
            refresh_ui(hwnd);
        }
        ID_FIND | ID_FIND_NEXT => info(hwnd, "Find menyusul."),
        ID_ADD_BATCH => do_batch(hwnd, String::new()),
        ID_ADD_BATCH_CLIP => {
            let clip = crate::tasks::read_clipboard_text().unwrap_or_default();
            let urls = crate::tasks::extract_urls(&clip);
            if urls.is_empty() {
                info(hwnd, "Tidak ada URL http(s) di clipboard.");
            } else {
                do_batch(hwnd, urls.join("\r\n"));
            }
        }
        ID_EXPORT => do_export(hwnd),
        ID_IMPORT => do_import(hwnd),
        ID_SITE_GRABBER => {
            let urls = crate::tasks::grabber_dialog(hwnd);
            add_urls(hwnd, &urls);
        }
        ID_REDOWNLOAD => info(hwnd, "Redownload menyusul."),
        ID_UPDATES => {
            ShellExecuteW(None, w!("open"), w!("https://github.com/s4rt4/adm-win"), None, None, SW_SHOWNORMAL);
        }
        ID_ARRANGE | ID_TOOLBAR | ID_TRAY_ICON | ID_CUSTOMIZE | ID_FONT | ID_LANGUAGE => {
            info(hwnd, "Menyusul.")
        }
        // Tray.
        ID_TRAY_SHOW => toggle_window(hwnd),
        ID_TRAY_AUTOSTART => {
            let _ = autostart::toggle();
        }
        ID_TRAY_EXIT => request_exit(hwnd),
        m if (ID_MOVE_BASE..ID_MOVE_BASE + 6).contains(&m) => do_move(hwnd, m - ID_MOVE_BASE),
        m if (ID_MENU_BASE..ID_MENU_BASE + 5).contains(&m) => open_menu(hwnd, m - ID_MENU_BASE),
        _ => {}
    }
}

unsafe fn open_menu(hwnd: HWND, idx: usize) {
    let h = MENUS.lock().unwrap()[idx];
    if h == 0 {
        return;
    }
    let menu = HMENU(h as *mut core::ffi::c_void);
    if idx == 1 {
        update_file_menu(menu); // menu "File" — item kondisional ikut status
    }
    let Some(ms) = state::load_hwnd(&state::MENUSTRIP_HWND) else { return };
    let mut wr = RECT::default();
    let _ = GetWindowRect(ms, &mut wr);
    let mut br = RECT::default();
    SendMessageW(ms, TB_GETRECT, Some(WPARAM(ID_MENU_BASE + idx)), Some(LPARAM(&mut br as *mut _ as isize)));
    let _ = SetForegroundWindow(hwnd);
    let _ = TrackPopupMenu(menu, TPM_LEFTALIGN, wr.left + br.left, wr.top + br.bottom, Some(0), hwnd, None);
}

unsafe fn do_move(hwnd: HWND, idx: usize) {
    use crate::category::Category;
    let cats = [
        Category::General,
        Category::Compressed,
        Category::Documents,
        Category::Music,
        Category::Programs,
        Category::Video,
    ];
    let Some(cat) = cats.get(idx).copied() else { return };
    let Some(id) = selected_id() else { return };
    let Some(row) = store::get(id) else { return };
    if row.status == store::Status::Downloading {
        info(hwnd, "Hentikan unduhan dulu sebelum memindah kategori.");
        return;
    }
    let Some(engine) = ENGINE.get() else { return };
    let filename = row.filename();
    let mut newdir = engine.download_dir();
    if let Some(f) = cat.folder() {
        newdir.push(f);
    }
    let _ = std::fs::create_dir_all(&newdir);
    let newpath = newdir.join(&filename);
    if newpath != row.output {
        let _ = std::fs::rename(&row.output, &newpath);
        let mut old_sc = row.output.clone().into_os_string();
        old_sc.push(".adm");
        let mut new_sc = newpath.clone().into_os_string();
        new_sc.push(".adm");
        let _ = std::fs::rename(&old_sc, &new_sc);
    }
    store::move_category(id, newpath, cat);
    refresh_ui(hwnd);
}

unsafe fn do_add(hwnd: HWND) {
    show_add(hwnd, None);
}

/// Tampilkan dialog "Download File Info". `incoming` = titipan dari browser
/// (url+metadata). User memutuskan Start (→ mulai + dialog status) / Download
/// Later (→ antrian) / Cancel. Tidak ada yang mulai tanpa konfirmasi user.
unsafe fn show_add(hwnd: HWND, incoming: Option<DownloadAddParams>) {
    let Some(engine) = ENGINE.get() else { return };
    let dir = engine.download_dir();

    // Browser (incoming Some) → "Download File Info" dengan nama file otomatis.
    // Manual (None) → dialog "Add new download" sederhana tanpa nama file.
    let result = if let Some(inc) = &incoming {
        dialogs::download_info_dialog(hwnd, &inc.url, inc.filename.as_deref(), &dir)
    } else {
        dialogs::add_url_dialog(hwnd, "")
    };

    if let Some((mut params, start_now)) = result {
        // Bawa metadata browser (referrer/UA/cookies/nama) bila ada.
        if let Some(inc) = &incoming {
            params.referrer = params.referrer.or_else(|| inc.referrer.clone());
            params.user_agent = params.user_agent.or_else(|| inc.user_agent.clone());
            params.cookies = params.cookies.or_else(|| inc.cookies.clone());
            params.filename = params.filename.or_else(|| inc.filename.clone());
        }
        if start_now {
            let id = engine.add(params);
            refresh_ui(hwnd);
            crate::progress::open(hwnd, id); // dialog "Download status"
        } else {
            engine.enqueue(params);
            refresh_ui(hwnd);
        }
    }
}

/// Dialog batch (Tasks → Add batch / from clipboard). `initial` prefill.
unsafe fn do_batch(hwnd: HWND, initial: String) {
    if let Some(text) = crate::tasks::batch_dialog(hwnd, &initial) {
        let urls = crate::tasks::parse_batch(&text);
        add_urls(hwnd, &urls);
    }
}

/// Tambahkan banyak URL ke antrian (queued); user mulai via Start queue/Resume.
unsafe fn add_urls(hwnd: HWND, urls: &[String]) {
    if urls.is_empty() {
        info(hwnd, "Tidak ada URL valid.");
        return;
    }
    if let Some(e) = ENGINE.get() {
        for u in urls {
            e.enqueue(DownloadAddParams { url: u.clone(), ..Default::default() });
        }
    }
    refresh_ui(hwnd);
    info(hwnd, &format!("{} unduhan ditambahkan ke antrian.", urls.len()));
}

/// Export daftar URL semua unduhan ke berkas teks (satu URL per baris).
unsafe fn do_export(hwnd: HWND) {
    let urls = store::with_rows(|rows| rows.iter().map(|r| r.url.clone()).collect::<Vec<_>>());
    if urls.is_empty() {
        info(hwnd, "Daftar unduhan masih kosong.");
        return;
    }
    if let Some(path) = crate::tasks::save_dialog(hwnd) {
        match std::fs::write(&path, urls.join("\r\n")) {
            Ok(_) => info(hwnd, &format!("{} URL diekspor ke:\n{}", urls.len(), path.display())),
            Err(e) => info(hwnd, &format!("Gagal menulis berkas: {e}")),
        }
    }
}

/// Import daftar URL dari berkas teks → ditambahkan ke antrian.
unsafe fn do_import(hwnd: HWND) {
    if let Some(path) = crate::tasks::open_dialog(hwnd) {
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let urls = crate::tasks::parse_batch(&text);
                add_urls(hwnd, &urls);
            }
            Err(e) => info(hwnd, &format!("Gagal membaca berkas: {e}")),
        }
    }
}

unsafe fn remove_selected(hwnd: HWND, delete_file: bool) {
    let Some(id) = selected_id() else { return };
    if let Some(e) = ENGINE.get() {
        e.cancel(id);
    }
    if let Some(row) = store::remove(id) {
        if delete_file {
            let _ = std::fs::remove_file(&row.output);
            let mut sc = row.output.clone().into_os_string();
            sc.push(".adm");
            let _ = std::fs::remove_file(sc);
        }
    }
    refresh_ui(hwnd);
}

unsafe fn on_dblclick(hwnd: HWND, index: i32) {
    let Some(lv) = state::load_hwnd(&state::LIST_HWND) else { return };
    let Some(id) = item_id(lv, index) else { return };
    let Some(row) = store::get(id) else { return };
    if row.status == store::Status::Complete {
        let h = HSTRING::from(row.output.to_string_lossy().into_owned());
        ShellExecuteW(None, w!("open"), PCWSTR(h.as_ptr()), None, None, SW_SHOWNORMAL);
    } else {
        crate::progress::open(hwnd, id);
    }
}

unsafe fn open_selected(_hwnd: HWND, index: i32) {
    let Some(lv) = state::load_hwnd(&state::LIST_HWND) else { return };
    if let Some(id) = item_id(lv, index) {
        if let Some(row) = store::get(id) {
            if row.status == store::Status::Complete {
                let h = HSTRING::from(row.output.to_string_lossy().into_owned());
                ShellExecuteW(None, w!("open"), PCWSTR(h.as_ptr()), None, None, SW_SHOWNORMAL);
            }
        }
    }
}

fn open_folder_selected() {
    if let Some(id) = selected_id() {
        if let Some(row) = store::get(id) {
            open_folder(&row.output);
        }
    }
}

/// Buka Explorer & sorot berkas. `raw_arg` agar `/select,"<path berspasi>"`
/// tak diutip seluruhnya oleh Rust (penyebab explorer buka folder default).
pub fn open_folder(path: &std::path::Path) {
    use std::os::windows::process::CommandExt;
    let _ = std::process::Command::new("explorer.exe")
        .raw_arg(format!("/select,\"{}\"", path.display()))
        .spawn();
}

/// Enable/disable item menu "File" sesuai status download terpilih.
/// Stop Download → sedang unduh; Remove → ada yang dipilih;
/// Download Now → bisa dilanjut (paused/error/queued); Redownload → selesai/error.
unsafe fn update_file_menu(menu: HMENU) {
    use store::Status;
    let status = selected_id().and_then(store::get).map(|r| r.status);
    let can_stop = matches!(status, Some(Status::Downloading));
    let can_resume = matches!(status, Some(Status::Paused | Status::Error | Status::Queued));
    let can_redo = matches!(status, Some(Status::Complete | Status::Error));
    let en = |id: usize, on: bool| {
        let flag = if on { MF_ENABLED } else { MF_GRAYED };
        let _ = EnableMenuItem(menu, id as u32, MF_BYCOMMAND | flag);
    };
    en(ID_STOP, can_stop);
    en(ID_REMOVE, status.is_some());
    en(ID_DOWNLOAD_NOW, can_resume);
    en(ID_REDOWNLOAD, can_redo);
}

/// AppendMenu dengan kondisi enable (disabled = abu-abu).
unsafe fn append_en(menu: HMENU, id: usize, text: PCWSTR, enabled: bool) {
    let flags = if enabled { MF_STRING } else { MF_STRING | MF_GRAYED };
    let _ = AppendMenuW(menu, flags, id, text);
}

unsafe fn show_context_menu(hwnd: HWND) {
    use store::Status;
    let status = selected_id().and_then(store::get).map(|r| r.status);
    // Resume/Start relevan saat stopped/error/queued; Stop saat sedang unduh.
    let can_resume = matches!(status, Some(Status::Paused | Status::Error | Status::Queued));
    let can_stop = matches!(status, Some(Status::Downloading));
    let is_complete = matches!(status, Some(Status::Complete));

    let menu = CreatePopupMenu().unwrap_or_default();
    append_en(menu, ID_OPEN, w!("Open"), is_complete);
    append(menu, ID_OPEN_FOLDER, w!("Open folder"));
    sep(menu);
    append_en(menu, ID_RESUME, w!("Resume / Start"), can_resume);
    append_en(menu, ID_STOP, w!("Stop"), can_stop);
    sep(menu);
    append(menu, ID_REMOVE, w!("Remove from list"));
    append(menu, ID_DELETE, w!("Delete (file)"));
    sep(menu);
    let mv = CreatePopupMenu().unwrap_or_default();
    for (i, c) in ["General", "Compressed", "Documents", "Music", "Programs", "Video"]
        .iter()
        .enumerate()
    {
        let h = HSTRING::from(*c);
        let _ = AppendMenuW(mv, MF_STRING, ID_MOVE_BASE + i, PCWSTR(h.as_ptr()));
    }
    popup(menu, mv, w!("Move to category"));
    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    let _ = SetForegroundWindow(hwnd);
    let _ = TrackPopupMenu(menu, TPM_RIGHTBUTTON, pt.x, pt.y, Some(0), hwnd, None);
    let _ = DestroyMenu(menu);
}

// ============================ Misc UI ============================

unsafe fn refresh_ui(hwnd: HWND) {
    if let Some(lv) = state::load_hwnd(&state::LIST_HWND) {
        refresh_list(lv);
    }
    let total = store::len();
    let active = store::active_count();
    set_status_bar(&format!("{total} unduhan — {active} aktif"));
    // Judul.
    let title = if active == 0 {
        "Alpha Download Manager".to_string()
    } else {
        format!("Alpha Download Manager — {active} aktif")
    };
    let h = HSTRING::from(title);
    let _ = SetWindowTextW(hwnd, PCWSTR(h.as_ptr()));

    update_toolbar_state();

    // Unduhan baru selesai: tutup dialog status (tak diperlukan lagi), lalu
    // tampilkan dialog "Download complete" (§9.14).
    let show_complete = crate::settings::get().show_complete_dialog;
    for row in store::take_newly_completed() {
        crate::progress::close_for(row.id);
        if show_complete {
            crate::progress::show_complete(hwnd, &row);
        }
    }
}

/// Enable/disable tombol toolbar sesuai seleksi & status (mirip §9.3 File menu).
unsafe fn update_toolbar_state() {
    use store::Status;
    let Some(tb) = state::load_hwnd(&state::TOOLBAR_HWND) else { return };
    let sel = selected_id().and_then(store::get).map(|r| r.status);
    let complete = matches!(sel, Some(Status::Complete));
    let active = matches!(
        sel,
        Some(Status::Downloading | Status::Paused | Status::Queued | Status::Error)
    );
    let en = |id: usize, on: bool| {
        SendMessageW(tb, TB_ENABLEBUTTON, Some(WPARAM(id)), Some(LPARAM(on as isize)));
    };
    en(ID_RESUME, active);
    en(ID_STOP, active);
    en(ID_STOP_ALL, !complete); // aktif saat tanpa seleksi / sedang unduh
    en(ID_DELETE, sel.is_some());
    // Add URL & Delete Completed selalu aktif.
}

/// Set teks status bar; pada tema gelap pakai owner-draw agar teks terbaca.
fn set_status_bar(text: &str) {
    *STATUS_TEXT.lock().unwrap() = text.to_string();
    let Some(sb) = state::load_hwnd(&state::STATUS_HWND) else { return };
    let dark = DARK.load(Ordering::SeqCst);
    unsafe {
        if dark {
            let c = rgb(DARK_SURFACE.0, DARK_SURFACE.1, DARK_SURFACE.2);
            SendMessageW(sb, SB_SETBKCOLOR, Some(WPARAM(0)), Some(LPARAM(c.0 as isize)));
            // part 0 owner-draw (SBT_OWNERDRAW = 0x1000) → digambar di WM_DRAWITEM.
            SendMessageW(sb, SB_SETTEXTW, Some(WPARAM(0x1000)), Some(LPARAM(0)));
        } else {
            SendMessageW(sb, SB_SETBKCOLOR, Some(WPARAM(0)), Some(LPARAM(CLR_DEFAULT as isize)));
            let h = HSTRING::from(text);
            SendMessageW(sb, SB_SETTEXTW, Some(WPARAM(0)), Some(LPARAM(h.as_ptr() as isize)));
        }
        let _ = InvalidateRect(Some(sb), None, true);
    }
}

fn set_global_limit(bps: u64) {
    if let Some(e) = ENGINE.get() {
        e.set_global_limit(bps);
    }
}

unsafe fn set_theme(hwnd: HWND, theme: u8) {
    crate::settings::update(|s| s.theme = theme);
    apply_theme(hwnd);
    update_theme_checks();
}

unsafe fn update_theme_checks() {
    let h = THEME_MENU.load(Ordering::SeqCst);
    if h == 0 {
        return;
    }
    let menu = HMENU(h as *mut core::ffi::c_void);
    let sel = match crate::settings::get().theme {
        crate::settings::THEME_DARK => ID_THEME_DARK,
        crate::settings::THEME_LIGHT => ID_THEME_LIGHT,
        _ => ID_THEME_SYSTEM,
    };
    let _ = CheckMenuRadioItem(menu, ID_THEME_DARK as u32, ID_THEME_SYSTEM as u32, sel as u32, MF_BYCOMMAND.0);
}

fn info(hwnd: HWND, msg: &str) {
    let h = HSTRING::from(msg);
    unsafe {
        MessageBoxW(Some(hwnd), PCWSTR(h.as_ptr()), w!("ADM"), MB_OK | MB_ICONINFORMATION);
    }
}

// ============================ Dialog About ============================

/// Dialog "About" custom: logo + layout rapi + badge status rilis (UNSTABLE).
fn show_about(parent: HWND) {
    unsafe {
        let Ok(module) = GetModuleHandleW(None) else { return };
        let instance: HINSTANCE = module.into();

        if !ABOUT_REG.swap(true, Ordering::SeqCst) {
            let wc = WNDCLASSW {
                lpfnWndProc: Some(about_proc),
                hInstance: instance,
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                lpszClassName: ABOUT_CLASS,
                ..Default::default()
            };
            RegisterClassW(&wc);
        }

        ABOUT_DONE.store(false, Ordering::SeqCst);
        let icon = load_app_icon(72, 72);
        *ABOUT_ICON.lock().unwrap() = icon.0 as isize;

        let style = WS_POPUP | WS_CAPTION | WS_SYSMENU;
        let mut rc = RECT { left: 0, top: 0, right: 480, bottom: 300 };
        let _ = AdjustWindowRectEx(&mut rc, style, false, WS_EX_DLGMODALFRAME);
        let (dw, dh) = (rc.right - rc.left, rc.bottom - rc.top);
        let mut pr = RECT::default();
        let _ = GetWindowRect(parent, &mut pr);
        let x = pr.left + ((pr.right - pr.left) - dw) / 2;
        let y = pr.top + ((pr.bottom - pr.top) - dh) / 2;

        let Ok(dlg) = CreateWindowExW(
            WS_EX_DLGMODALFRAME,
            ABOUT_CLASS,
            w!("About ADM"),
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
            return;
        };

        let gui_font = GetStockObject(DEFAULT_GUI_FONT);

        // Tautan repo (klik → buka GitHub).
        let link = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("STATIC"),
            PCWSTR::null(),
            WS_CHILD | WS_VISIBLE | WINDOW_STYLE(0x0000_0100), // SS_NOTIFY
            28,
            166,
            280,
            20,
            Some(dlg),
            Some(HMENU(IDC_ABOUT_LINK as *mut core::ffi::c_void)),
            Some(instance),
            None,
        )
        .unwrap_or_default();
        let lh = HSTRING::from("github.com/s4rt4/adm-win");
        let _ = SetWindowTextW(link, PCWSTR(lh.as_ptr()));
        SendMessageW(link, WM_SETFONT, Some(WPARAM(gui_font.0 as usize)), Some(LPARAM(1)));
        *ABOUT_LINK.lock().unwrap() = link.0 as isize;

        // Tombol OK.
        let ok = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("BUTTON"),
            w!("OK"),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_DEFPUSHBUTTON as u32),
            480 - 96,
            252,
            80,
            28,
            Some(dlg),
            Some(HMENU(IDC_ABOUT_OK as *mut core::ffi::c_void)),
            Some(instance),
            None,
        )
        .unwrap_or_default();
        SendMessageW(ok, WM_SETFONT, Some(WPARAM(gui_font.0 as usize)), Some(LPARAM(1)));

        let _ = EnableWindow(parent, false);
        let _ = ShowWindow(dlg, SW_SHOW);
        let _ = SetForegroundWindow(dlg);
        let _ = SetFocus(Some(ok));

        let mut msg = MSG::default();
        while !ABOUT_DONE.load(Ordering::SeqCst) && GetMessageW(&mut msg, None, 0, 0).as_bool() {
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
        let ic = *ABOUT_ICON.lock().unwrap();
        if ic != 0 {
            let _ = DestroyIcon(HICON(ic as *mut core::ffi::c_void));
            *ABOUT_ICON.lock().unwrap() = 0;
        }
    }
}

extern "system" fn about_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_PAINT => {
                paint_about(hwnd);
                LRESULT(0)
            }
            WM_CTLCOLORSTATIC => {
                let hdc = HDC(wparam.0 as *mut core::ffi::c_void);
                SetBkMode(hdc, TRANSPARENT);
                if lparam.0 == *ABOUT_LINK.lock().unwrap() {
                    SetTextColor(hdc, COLORREF(0x00CC_6600)); // biru link (BGR)
                } else {
                    SetTextColor(hdc, COLORREF(0x0020_2020));
                }
                LRESULT(GetStockObject(WHITE_BRUSH).0 as isize)
            }
            WM_COMMAND => {
                let id = wparam.0 & 0xFFFF;
                match id {
                    IDC_ABOUT_OK => {
                        ABOUT_DONE.store(true, Ordering::SeqCst);
                        let _ = DestroyWindow(hwnd);
                    }
                    IDC_ABOUT_LINK => {
                        ShellExecuteW(
                            None,
                            w!("open"),
                            w!("https://github.com/s4rt4/adm-win"),
                            None,
                            None,
                            SW_SHOWNORMAL,
                        );
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_CLOSE => {
                ABOUT_DONE.store(true, Ordering::SeqCst);
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

unsafe fn ab_fill(hdc: HDC, l: i32, t: i32, r: i32, b: i32, bgr: u32) {
    let brush = CreateSolidBrush(COLORREF(bgr));
    let rc = RECT { left: l, top: t, right: r, bottom: b };
    FillRect(hdc, &rc, brush);
    let _ = DeleteObject(brush.into());
}

unsafe fn ab_font(height: i32, weight: i32) -> HFONT {
    CreateFontW(
        height,
        0,
        0,
        0,
        weight,
        0,
        0,
        0,
        DEFAULT_CHARSET,
        OUT_DEFAULT_PRECIS,
        CLIP_DEFAULT_PRECIS,
        CLEARTYPE_QUALITY,
        0,
        w!("Segoe UI"),
    )
}

unsafe fn ab_text(hdc: HDC, s: &str, x: i32, y: i32) {
    let mut wide: Vec<u16> = s.encode_utf16().collect();
    let mut rc = RECT { left: x, top: y, right: x + 2000, bottom: y + 48 };
    DrawTextW(hdc, &mut wide, &mut rc, DT_LEFT | DT_TOP | DT_SINGLELINE | DT_NOPREFIX);
}

unsafe fn ab_text_c(hdc: HDC, s: &str, x: i32, y: i32, w: i32, h: i32) {
    let mut wide: Vec<u16> = s.encode_utf16().collect();
    let mut rc = RECT { left: x, top: y, right: x + w, bottom: y + h };
    DrawTextW(hdc, &mut wide, &mut rc, DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
}

unsafe fn ab_text_width(hdc: HDC, s: &str) -> i32 {
    let wide: Vec<u16> = s.encode_utf16().collect();
    let mut sz = SIZE::default();
    let _ = GetTextExtentPoint32W(hdc, &wide, &mut sz);
    sz.cx
}

unsafe fn paint_about(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let (w, h) = (rc.right, rc.bottom);
    let footer_top = h - 56;

    // Panel atas putih, footer abu-abu + garis pemisah.
    ab_fill(hdc, 0, 0, w, footer_top, 0x00FF_FFFF);
    ab_fill(hdc, 0, footer_top, w, h, 0x00F3_F3F3);
    ab_fill(hdc, 0, footer_top, w, footer_top + 1, 0x00DE_DEDE);
    ab_fill(hdc, 28, 150, w - 28, 151, 0x00EC_ECEC);

    // Logo.
    let ic = *ABOUT_ICON.lock().unwrap();
    if ic != 0 {
        let _ = DrawIconEx(hdc, 28, 30, HICON(ic as *mut core::ffi::c_void), 72, 72, 0, None, DI_NORMAL);
    }

    SetBkMode(hdc, TRANSPARENT);
    let tx = 124;

    // Judul.
    let f = ab_font(-26, 700);
    let prev = SelectObject(hdc, f.into());
    SetTextColor(hdc, COLORREF(0x0020_2020));
    ab_text(hdc, "Alpha Download Manager", tx, 32);
    SelectObject(hdc, prev);
    let _ = DeleteObject(f.into());

    // Subjudul.
    let f = ab_font(-15, 400);
    let prev = SelectObject(hdc, f.into());
    SetTextColor(hdc, COLORREF(0x006E_6E6E));
    ab_text(hdc, "Native Windows download manager", tx + 2, 68);
    SelectObject(hdc, prev);
    let _ = DeleteObject(f.into());

    // Baris versi.
    let vtext = format!("Version {ABOUT_VERSION}");
    let f = ab_font(-16, 600);
    let prev = SelectObject(hdc, f.into());
    SetTextColor(hdc, COLORREF(0x0040_4040));
    ab_text(hdc, &vtext, tx + 2, 100);
    let vw = ab_text_width(hdc, &vtext);
    SelectObject(hdc, prev);
    let _ = DeleteObject(f.into());

    // Badge "UNSTABLE" (oranye, sudut membulat).
    let f = ab_font(-12, 700);
    let prev = SelectObject(hdc, f.into());
    let bw = ab_text_width(hdc, "UNSTABLE") + 16;
    let (bx, by, bh) = (tx + 2 + vw + 12, 99, 18);
    let brush = CreateSolidBrush(COLORREF(0x0022_7EE6)); // RGB(230,126,34)
    let pen = CreatePen(PS_SOLID, 1, COLORREF(0x0022_7EE6));
    let ob = SelectObject(hdc, brush.into());
    let op = SelectObject(hdc, pen.into());
    let _ = RoundRect(hdc, bx, by, bx + bw, by + bh, 9, 9);
    SelectObject(hdc, ob);
    SelectObject(hdc, op);
    let _ = DeleteObject(brush.into());
    let _ = DeleteObject(pen.into());
    SetTextColor(hdc, COLORREF(0x00FF_FFFF));
    ab_text_c(hdc, "UNSTABLE", bx, by, bw, bh);
    SelectObject(hdc, prev);
    let _ = DeleteObject(f.into());

    // Hak cipta + catatan status (di bawah pemisah; tautan repo = child control).
    let f = ab_font(-13, 400);
    let prev = SelectObject(hdc, f.into());
    SetTextColor(hdc, COLORREF(0x0080_8080));
    ab_text(hdc, "\u{00A9} 2026 s4rt4 \u{00B7} Released under the MIT License", 28, 192);
    ab_text(hdc, "Early pre-release \u{2014} expect bugs and breaking changes.", 28, 212);
    SelectObject(hdc, prev);
    let _ = DeleteObject(f.into());

    let _ = EndPaint(hwnd, &ps);
}

fn request_exit(hwnd: HWND) {
    let active = ENGINE.get().map(|e| e.active_count()).unwrap_or(0);
    unsafe {
        if active > 0 {
            let text = HSTRING::from(format!("Ada {active} unduhan aktif. Keluar dari ADM?"));
            if MessageBoxW(Some(hwnd), PCWSTR(text.as_ptr()), w!("Alpha Download Manager"), MB_YESNO | MB_ICONQUESTION) != IDYES {
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

unsafe fn set_font(hwnd: HWND) {
    let font = GetStockObject(DEFAULT_GUI_FONT);
    SendMessageW(hwnd, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
}

// ============================ Format helpers ============================

fn fmt_size(bytes: Option<u64>) -> String {
    match bytes {
        None => String::new(),
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
        return String::new();
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
        None => String::new(),
        Some(s) if s >= 3600 => format!("{} hr", s / 3600),
        Some(s) if s >= 60 => format!("{} min", s / 60),
        Some(s) => format!("{s} sec"),
    }
}

// ============================ Toolbar icons ============================

/// Blob ikon toolbar (11 × 24×24 premultiplied BGRA) dari tools/icongen.
const TB_ICONS: &[u8] = include_bytes!("../assets/icons/toolbar24.bin");

fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF((r as u32) | ((g as u32) << 8) | ((b as u32) << 16))
}

/// Custom-draw toolbar/menu-strip untuk tema gelap (latar surface + teks terang).
unsafe fn toolbar_customdraw(lparam: LPARAM) -> LRESULT {
    let cd = &mut *(lparam.0 as *mut NMTBCUSTOMDRAW);
    let stage = cd.nmcd.dwDrawStage;
    if stage == CDDS_PREPAINT {
        let br = CreateSolidBrush(rgb(DARK_SURFACE.0, DARK_SURFACE.1, DARK_SURFACE.2));
        FillRect(cd.nmcd.hdc, &cd.nmcd.rc, br);
        let _ = DeleteObject(br.into());
        LRESULT(CDRF_NOTIFYITEMDRAW as isize)
    } else if stage == CDDS_ITEMPREPAINT {
        cd.clrText = rgb(DARK_TEXT.0, DARK_TEXT.1, DARK_TEXT.2);
        LRESULT(TBCDRF_USECDCOLORS as isize)
    } else {
        LRESULT(CDRF_DODEFAULT as isize)
    }
}

/// Bangun ImageList 24x24 ARGB dari blob BGRA. Ikon Lucide monokrom hitam;
/// untuk tema gelap di-tint jadi abu terang (premultiplied) agar terlihat.
unsafe fn build_imagelist_with(transform: impl Fn(&mut [u8])) -> HIMAGELIST {
    const N: i32 = 11;
    const SZ: i32 = 24;
    let stride = (SZ * SZ * 4) as usize;
    let himl = ImageList_Create(SZ, SZ, ILC_COLOR32, N, 0);
    let screen = GetDC(None);

    let mut bmi = BITMAPINFO::default();
    bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
    bmi.bmiHeader.biWidth = SZ;
    bmi.bmiHeader.biHeight = -SZ; // top-down; biCompression default 0 = BI_RGB
    bmi.bmiHeader.biPlanes = 1;
    bmi.bmiHeader.biBitCount = 32;

    for i in 0..N as usize {
        let mut buf = TB_ICONS[i * stride..(i + 1) * stride].to_vec();
        for px in buf.chunks_exact_mut(4) {
            transform(px);
        }
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        if let Ok(hbmp) = CreateDIBSection(Some(screen), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
            if !bits.is_null() {
                std::ptr::copy_nonoverlapping(buf.as_ptr(), bits as *mut u8, stride);
            }
            ImageList_Add(himl, hbmp, None);
            let _ = DeleteObject(hbmp.into());
        }
    }
    ReleaseDC(None, screen);
    himl
}

/// Imagelist normal (ikon hitam; tema gelap → di-tint terang).
unsafe fn build_toolbar_imagelist(dark: bool) -> HIMAGELIST {
    build_imagelist_with(move |px| {
        if dark {
            let a = px[3] as u32;
            let v = (0xDC * a / 255) as u8;
            px[0] = v;
            px[1] = v;
            px[2] = v;
        }
    })
}

/// Imagelist untuk tombol disabled: abu-abu + dipudarkan (alpha 50%).
unsafe fn build_disabled_imagelist() -> HIMAGELIST {
    build_imagelist_with(|px| {
        let fa = px[3] as u32 / 2; // alpha 50%
        let v = (0xA0 * fa / 255) as u8; // abu-abu premultiplied
        px[0] = v;
        px[1] = v;
        px[2] = v;
        px[3] = fa as u8;
    })
}

/// Terapkan tema aktif (plan §12): title bar gelap, warna ListView/TreeView,
/// dan ikon toolbar sesuai tema. Chrome klasik (menu/toolbar/status) tetap
/// warna sistem (keterbatasan Win32 tanpa owner-draw penuh).
unsafe fn apply_theme(hwnd: HWND) {
    let dark = crate::theme::effective_dark(crate::settings::get().theme);
    DARK.store(dark, Ordering::SeqCst);

    // Popup menu gelap (best-effort via uxtheme; abaikan bila gagal).
    crate::theme::set_dark_menus(dark);

    // Title bar.
    let flag = windows::core::BOOL(dark as i32);
    let _ = DwmSetWindowAttribute(
        hwnd,
        DWMWA_USE_IMMERSIVE_DARK_MODE,
        &flag as *const _ as *const core::ffi::c_void,
        std::mem::size_of::<windows::core::BOOL>() as u32,
    );

    let (bg, txt) = if dark {
        (rgb(DARK_BG.0, DARK_BG.1, DARK_BG.2), rgb(DARK_TEXT.0, DARK_TEXT.1, DARK_TEXT.2))
    } else {
        (rgb(255, 255, 255), rgb(0, 0, 0))
    };
    let sub = if dark { w!("DarkMode_Explorer") } else { w!("Explorer") };

    if let Some(lv) = state::load_hwnd(&state::LIST_HWND) {
        let _ = SetWindowTheme(lv, sub, PCWSTR::null());
        SendMessageW(lv, LVM_SETBKCOLOR, Some(WPARAM(0)), Some(LPARAM(bg.0 as isize)));
        SendMessageW(lv, LVM_SETTEXTBKCOLOR, Some(WPARAM(0)), Some(LPARAM(bg.0 as isize)));
        SendMessageW(lv, LVM_SETTEXTCOLOR, Some(WPARAM(0)), Some(LPARAM(txt.0 as isize)));
        let _ = InvalidateRect(Some(lv), None, true);
    }
    if let Some(tv) = state::load_hwnd(&state::TREE_HWND) {
        let _ = SetWindowTheme(tv, sub, PCWSTR::null());
        SendMessageW(tv, TVM_SETBKCOLOR, Some(WPARAM(0)), Some(LPARAM(bg.0 as isize)));
        SendMessageW(tv, TVM_SETTEXTCOLOR, Some(WPARAM(0)), Some(LPARAM(txt.0 as isize)));
        let _ = InvalidateRect(Some(tv), None, true);
    }
    if let Some(tb) = state::load_hwnd(&state::TOOLBAR_HWND) {
        let himl = build_toolbar_imagelist(dark);
        SendMessageW(tb, TB_SETIMAGELIST, Some(WPARAM(0)), Some(LPARAM(himl.0)));
        let diml = build_disabled_imagelist();
        SendMessageW(tb, TB_SETDISABLEDIMAGELIST, Some(WPARAM(0)), Some(LPARAM(diml.0)));
        let _ = InvalidateRect(Some(tb), None, true);
    }
    if let Some(ms) = state::load_hwnd(&state::MENUSTRIP_HWND) {
        let _ = InvalidateRect(Some(ms), None, true);
    }
    // Status bar: warna + (gelap = owner-draw teks).
    let cur = STATUS_TEXT.lock().unwrap().clone();
    set_status_bar(&cur);
    let _ = InvalidateRect(Some(hwnd), None, true);
}

// ============================ Tray ============================

unsafe fn show_tray_menu(hwnd: HWND) {
    let Ok(menu) = CreatePopupMenu() else { return };
    append(menu, ID_TRAY_SHOW, w!("Show / Hide"));
    sep(menu);
    append(menu, ID_PAUSE_ALL, w!("Pause All"));
    append(menu, ID_STOP_ALL, w!("Stop All"));
    sep(menu);
    let flags = if autostart::is_enabled() { MF_STRING | MF_CHECKED } else { MF_STRING };
    let _ = AppendMenuW(menu, flags, ID_TRAY_AUTOSTART, w!("Start with Windows"));
    sep(menu);
    append(menu, ID_TRAY_EXIT, w!("Exit"));

    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    let _ = SetForegroundWindow(hwnd);
    let _ = TrackPopupMenu(menu, TPM_RIGHTBUTTON, pt.x, pt.y, Some(0), hwnd, None);
    let _ = DestroyMenu(menu);
}

fn tray_data(hwnd: HWND) -> NOTIFYICONDATAW {
    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_UID,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: state::WM_TRAY,
        hIcon: unsafe { load_app_icon(GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON)) },
        ..Default::default()
    };
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
