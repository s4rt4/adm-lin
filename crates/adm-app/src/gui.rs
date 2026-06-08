//! GUI Win32 jendela utama (plan §9): menu, toolbar (+split ▾), TreeView
//! kategori, ListView unduhan, status bar, tray, context menu. Live dari engine.

use crate::engine::{EngineEvent, EngineHandle, EventSink};
use crate::{autostart, dialogs, state, store};
use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};
use windows::core::{w, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

static ENGINE: OnceLock<EngineHandle> = OnceLock::new();

const TRAY_UID: u32 = 1;
const SIDEBAR_W: i32 = 190;

// ---- Command IDs ----
const ID_ADD: usize = 0x100;
const ID_ADD_BATCH: usize = 0x101;
const ID_ADD_BATCH_CLIP: usize = 0x102;
const ID_SITE_GRABBER: usize = 0x103;
const ID_DROP_TARGET: usize = 0x104;
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
const ID_SPEED_LIMITER: usize = 0x128;
const ID_OPTIONS: usize = 0x129;

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

const ID_HELP: usize = 0x140;
const ID_CHECK_UPDATES: usize = 0x141;
const ID_ABOUT: usize = 0x142;

const ID_RESUME: usize = 0x150;
const ID_DELETE: usize = 0x151;
const ID_TELL_FRIEND: usize = 0x152;
const ID_OPEN: usize = 0x153;
const ID_OPEN_FOLDER: usize = 0x154;

// Tray menu.
const ID_TRAY_SHOW: usize = 0x200;
const ID_TRAY_AUTOSTART: usize = 0x201;
const ID_TRAY_EXIT: usize = 0x202;

/// Ikon aplikasi (di-embed; dibuat dari logo.svg via tools/icongen).
const APP_ICO: &[u8] = include_bytes!("../assets/adm.ico");

/// Muat HICON dari .ico embedded pada ukuran terdekat.
unsafe fn load_app_icon(cx: i32, cy: i32) -> HICON {
    let off = LookupIconIdFromDirectoryEx(APP_ICO.as_ptr(), true, cx, cy, LR_DEFAULTCOLOR);
    if off <= 0 {
        return LoadIconW(None, IDI_APPLICATION).unwrap_or_default();
    }
    let data = &APP_ICO[off as usize..];
    CreateIconFromResourceEx(data, true, 0x0003_0000, cx, cy, LR_DEFAULTCOLOR).unwrap_or_default()
}

pub fn set_engine(engine: EngineHandle) {
    let _ = ENGINE.set(engine);
}

/// Akses engine untuk modul lain (mis. dialog progres).
pub fn engine() -> Option<EngineHandle> {
    ENGINE.get().cloned()
}

/// EventSink GUI: perbarui store + post ke UI thread.
pub fn make_sink() -> EventSink {
    Arc::new(|ev: EngineEvent| {
        match ev {
            EngineEvent::Started { id, url, output } => {
                eprintln!("[engine] #{id} mulai -> {}", output.display());
                store::on_started(id, url, output);
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
            hIcon: load_app_icon(GetSystemMetrics(SM_CXICON), GetSystemMetrics(SM_CYICON)),
            hbrBackground: HBRUSH((COLOR_BTNFACE.0 + 1) as *mut core::ffi::c_void),
            lpszClassName: class_name,
            ..Default::default()
        };
        let _ = RegisterClassW(&wc);

        let menu = build_menu();
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
            Some(menu),
            Some(instance),
            None,
        )?;
        state::store_hwnd(&state::MAIN_HWND, hwnd);

        // Ikon kecil & besar untuk taskbar/titlebar.
        let small = load_app_icon(GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON));
        let big = load_app_icon(GetSystemMetrics(SM_CXICON), GetSystemMetrics(SM_CYICON));
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
                layout(hwnd);
                LRESULT(0)
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
                handle_notify(hwnd, lparam);
                LRESULT(0)
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
    // Toolbar.
    let tb = CreateWindowExW(
        WINDOW_EX_STYLE::default(),
        w!("ToolbarWindow32"),
        PCWSTR::null(),
        WS_CHILD | WS_VISIBLE | WINDOW_STYLE(TBSTYLE_FLAT | TBSTYLE_TOOLTIPS | CCS_TOP as u32),
        0, 0, 0, 0,
        Some(hwnd),
        None,
        Some(instance),
        None,
    )
    .unwrap_or_default();
    SendMessageW(tb, TB_BUTTONSTRUCTSIZE, Some(WPARAM(std::mem::size_of::<TBBUTTON>())), Some(LPARAM(0)));
    SendMessageW(tb, TB_SETEXTENDEDSTYLE, Some(WPARAM(0)), Some(LPARAM(TBSTYLE_EX_MIXEDBUTTONS as isize)));
    let himl = build_toolbar_imagelist();
    SendMessageW(tb, TB_SETIMAGELIST, Some(WPARAM(0)), Some(LPARAM(himl.0)));
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

unsafe fn layout(hwnd: HWND) {
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);

    let tb = state::load_hwnd(&state::TOOLBAR_HWND);
    let sb = state::load_hwnd(&state::STATUS_HWND);
    let tv = state::load_hwnd(&state::TREE_HWND);
    let lv = state::load_hwnd(&state::LIST_HWND);

    let mut top = 0;
    if let Some(tb) = tb {
        SendMessageW(tb, TB_AUTOSIZE, Some(WPARAM(0)), Some(LPARAM(0)));
        let mut tr = RECT::default();
        let _ = GetWindowRect(tb, &mut tr);
        top = tr.bottom - tr.top;
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

unsafe fn build_menu() -> HMENU {
    let menu = CreateMenu().unwrap_or_default();

    let tasks = CreatePopupMenu().unwrap();
    append(tasks, ID_ADD, w!("Add new download...\tCtrl+N"));
    append(tasks, ID_ADD_BATCH, w!("Add batch download..."));
    append(tasks, ID_ADD_BATCH_CLIP, w!("Add batch download from clipboard"));
    append(tasks, ID_SITE_GRABBER, w!("Run site grabber..."));
    append(tasks, ID_DROP_TARGET, w!("Show drop target"));
    sep(tasks);
    append(tasks, ID_EXPORT, w!("Export..."));
    append(tasks, ID_IMPORT, w!("Import..."));
    sep(tasks);
    append(tasks, ID_EXIT, w!("Exit\tCtrl+Q"));
    popup(menu, tasks, w!("Tasks"));

    let file = CreatePopupMenu().unwrap();
    append(file, ID_STOP, w!("Stop Download"));
    append(file, ID_REMOVE, w!("Remove"));
    append(file, ID_DOWNLOAD_NOW, w!("Download Now"));
    append(file, ID_REDOWNLOAD, w!("Redownload"));
    popup(menu, file, w!("File"));

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
    append(dl, ID_SPEED_LIMITER, w!("Speed Limiter"));
    sep(dl);
    append(dl, ID_OPTIONS, w!("Options..."));
    popup(menu, dl, w!("Downloads"));

    let view = CreatePopupMenu().unwrap();
    append(view, ID_HIDE_CATEGORIES, w!("Hide categories"));
    append(view, ID_ARRANGE, w!("Arrange files"));
    append(view, ID_TOOLBAR, w!("Toolbar"));
    append(view, ID_TRAY_ICON, w!("ADM tray icon"));
    append(view, ID_CUSTOMIZE, w!("Customize URL List..."));
    sep(view);
    let theme = CreatePopupMenu().unwrap();
    append(theme, ID_THEME_DARK, w!("Dark"));
    append(theme, ID_THEME_LIGHT, w!("Light"));
    append(theme, ID_THEME_SYSTEM, w!("System"));
    popup(view, theme, w!("Theme"));
    append(view, ID_FONT, w!("Font..."));
    append(view, ID_LANGUAGE, w!("Language"));
    popup(menu, view, w!("View"));

    let help = CreatePopupMenu().unwrap();
    append(help, ID_HELP, w!("Help contents"));
    append(help, ID_CHECK_UPDATES, w!("Check for updates..."));
    popup(menu, help, w!("Help"));

    let about = CreatePopupMenu().unwrap();
    append(about, ID_ABOUT, w!("About ADM"));
    popup(menu, about, w!("About"));

    menu
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
        style |= BTNS_WHOLEDROPDOWN;
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
    mkbtn(&mut buttons, ID_DELETE_COMPLETED, "Delete Co...", 5, false);
    mksep(&mut buttons);
    mkbtn(&mut buttons, ID_OPTIONS, "Options", 6, false);
    mkbtn(&mut buttons, ID_SCHEDULER, "Scheduler", 7, false);
    mksep(&mut buttons);
    mkbtn(&mut buttons, ID_START_QUEUE, "Start Qu...", 8, true);
    mkbtn(&mut buttons, ID_STOP_QUEUE, "Stop Qu...", 9, true);
    mksep(&mut buttons);
    mkbtn(&mut buttons, ID_TELL_FRIEND, "Tell a Fri...", 10, false);

    SendMessageW(
        tb,
        TB_ADDBUTTONSW,
        Some(WPARAM(buttons.len())),
        Some(LPARAM(buttons.as_ptr() as isize)),
    );
}

// ============================ Categories ============================

unsafe fn tv_insert(tv: HWND, parent: HTREEITEM, text: PCWSTR) -> HTREEITEM {
    let mut item = TVINSERTSTRUCTW {
        hParent: parent,
        hInsertAfter: TVI_LAST,
        ..Default::default()
    };
    item.Anonymous.item = TVITEMW {
        mask: TVIF_TEXT,
        pszText: PWSTR(text.0 as *mut u16),
        ..Default::default()
    };
    let r = SendMessageW(tv, TVM_INSERTITEMW, Some(WPARAM(0)), Some(LPARAM(&item as *const _ as isize)));
    HTREEITEM(r.0)
}

unsafe fn populate_categories(tv: HWND) {
    let all = tv_insert(tv, TVI_ROOT, w!("All Downloads"));
    tv_insert(tv, all, w!("Compressed"));
    tv_insert(tv, all, w!("Documents"));
    tv_insert(tv, all, w!("Music"));
    tv_insert(tv, all, w!("Programs"));
    tv_insert(tv, all, w!("Video"));
    let _ = SendMessageW(tv, TVM_EXPAND, Some(WPARAM(TVE_EXPAND.0 as usize)), Some(LPARAM(all.0)));
    tv_insert(tv, TVI_ROOT, w!("Unfinished"));
    tv_insert(tv, TVI_ROOT, w!("Finished"));
    tv_insert(tv, TVI_ROOT, w!("Grabber projects"));
    tv_insert(tv, TVI_ROOT, w!("Queues"));
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

unsafe fn insert_row(lv: HWND, item: i32, text: &str) {
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    wide.push(0);
    let mut lvi = LVITEMW {
        mask: LVIF_TEXT,
        iItem: item,
        iSubItem: 0,
        pszText: PWSTR(wide.as_mut_ptr()),
        ..Default::default()
    };
    SendMessageW(lv, LVM_INSERTITEMW, Some(WPARAM(0)), Some(LPARAM(&mut lvi as *mut _ as isize)));
}

unsafe fn refresh_list(lv: HWND) {
    let count = SendMessageW(lv, LVM_GETITEMCOUNT, Some(WPARAM(0)), Some(LPARAM(0))).0 as usize;
    let rebuild = count != store::len();
    if rebuild {
        SendMessageW(lv, LVM_DELETEALLITEMS, Some(WPARAM(0)), Some(LPARAM(0)));
    }
    store::with_rows(|rows| {
        for (i, r) in rows.iter().enumerate() {
            let idx = i as i32;
            if rebuild {
                insert_row(lv, idx, &r.filename());
            } else {
                set_subitem(lv, idx, 0, &r.filename());
            }
            set_subitem(lv, idx, 2, &fmt_size(r.size));
            set_subitem(lv, idx, 3, &status_text(r));
            set_subitem(lv, idx, 4, &fmt_eta(r.eta_secs()));
            set_subitem(lv, idx, 5, &fmt_speed(r.speed_bps));
        }
    });
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
    store::id_at(idx as usize)
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
        ID_THEME_DARK | ID_THEME_LIGHT | ID_THEME_SYSTEM => {
            info(hwnd, "Tema akan diterapkan penuh di WM7.");
        }
        ID_ABOUT => {
            info(hwnd, "Alpha Download Manager (ADM)\nVersi 0.1.0\nDownload manager native Windows.");
        }
        ID_EXIT => request_exit(hwnd),
        // Fitur milestone lain.
        ID_SCHEDULER => info(hwnd, "Scheduler menyusul (WM6)."),
        ID_OPTIONS => info(hwnd, "Options menyusul (WM7)."),
        ID_SPEED_LIMITER => info(hwnd, "Speed Limiter UI menyusul (WM6)."),
        ID_START_QUEUE | ID_STOP_QUEUE => info(hwnd, "Antrian menyusul (WM6)."),
        ID_FIND | ID_FIND_NEXT => info(hwnd, "Find menyusul."),
        ID_ADD_BATCH | ID_ADD_BATCH_CLIP => info(hwnd, "Batch download menyusul."),
        ID_SITE_GRABBER => info(hwnd, "Site grabber = fase lanjutan."),
        ID_EXPORT | ID_IMPORT => info(hwnd, "Export/Import menyusul."),
        ID_REDOWNLOAD => info(hwnd, "Redownload menyusul."),
        ID_TELL_FRIEND => info(hwnd, "Bagikan ADM ke teman :)"),
        ID_HELP | ID_CHECK_UPDATES => info(hwnd, "Menyusul."),
        ID_DROP_TARGET | ID_ARRANGE | ID_TOOLBAR | ID_TRAY_ICON | ID_CUSTOMIZE | ID_FONT
        | ID_LANGUAGE => info(hwnd, "Menyusul."),
        // Tray.
        ID_TRAY_SHOW => toggle_window(hwnd),
        ID_TRAY_AUTOSTART => {
            let _ = autostart::toggle();
        }
        ID_TRAY_EXIT => request_exit(hwnd),
        _ => {}
    }
}

unsafe fn do_add(hwnd: HWND) {
    let Some(engine) = ENGINE.get() else { return };
    if let Some(params) = dialogs::add_dialog(hwnd, "", engine.download_dir()) {
        engine.add(params);
        refresh_ui(hwnd);
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
    let Some(id) = store::id_at(index as usize) else { return };
    let Some(row) = store::get(id) else { return };
    if row.status == store::Status::Complete {
        let h = HSTRING::from(row.output.to_string_lossy().into_owned());
        ShellExecuteW(None, w!("open"), PCWSTR(h.as_ptr()), None, None, SW_SHOWNORMAL);
    } else {
        crate::progress::open(hwnd, id);
    }
}

unsafe fn open_selected(_hwnd: HWND, index: i32) {
    if let Some(id) = store::id_at(index as usize) {
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
            let _ = std::process::Command::new("explorer")
                .arg(format!("/select,{}", row.output.display()))
                .spawn();
        }
    }
}

unsafe fn show_context_menu(hwnd: HWND) {
    let menu = CreatePopupMenu().unwrap_or_default();
    append(menu, ID_OPEN, w!("Open"));
    append(menu, ID_OPEN_FOLDER, w!("Open folder"));
    sep(menu);
    append(menu, ID_RESUME, w!("Resume / Start"));
    append(menu, ID_STOP, w!("Stop"));
    sep(menu);
    append(menu, ID_REMOVE, w!("Remove from list"));
    append(menu, ID_DELETE, w!("Delete (file)"));
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
    // Status bar.
    if let Some(sb) = state::load_hwnd(&state::STATUS_HWND) {
        let text = HSTRING::from(format!("{total} unduhan — {active} aktif"));
        SendMessageW(sb, SB_SETTEXTW, Some(WPARAM(0)), Some(LPARAM(text.as_ptr() as isize)));
    }
    // Judul.
    let title = if active == 0 {
        "Alpha Download Manager".to_string()
    } else {
        format!("Alpha Download Manager — {active} aktif")
    };
    let h = HSTRING::from(title);
    let _ = SetWindowTextW(hwnd, PCWSTR(h.as_ptr()));

    // Dialog "Download complete" untuk unduhan yang baru selesai (§9.14).
    for row in store::take_newly_completed() {
        crate::progress::show_complete(hwnd, &row);
    }
}

fn info(hwnd: HWND, msg: &str) {
    let h = HSTRING::from(msg);
    unsafe {
        MessageBoxW(Some(hwnd), PCWSTR(h.as_ptr()), w!("ADM"), MB_OK | MB_ICONINFORMATION);
    }
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

// ============================ Toolbar icons (GDI) ============================

fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF((r as u32) | ((g as u32) << 8) | ((b as u32) << 16))
}

const GREEN: (u8, u8, u8) = (40, 160, 70);
const DGREEN: (u8, u8, u8) = (59, 91, 67);
const RED: (u8, u8, u8) = (210, 60, 55);
const GOLD: (u8, u8, u8) = (217, 180, 4);
const GRAY: (u8, u8, u8) = (96, 96, 100);
const BLUE: (u8, u8, u8) = (70, 130, 180);
const WHITE: (u8, u8, u8) = (255, 255, 255);

unsafe fn frect(dc: HDC, l: i32, t: i32, r: i32, b: i32, c: (u8, u8, u8)) {
    let br = CreateSolidBrush(rgb(c.0, c.1, c.2));
    let rc = RECT { left: l, top: t, right: r, bottom: b };
    FillRect(dc, &rc, br);
    let _ = DeleteObject(br.into());
}

unsafe fn fellipse(dc: HDC, l: i32, t: i32, r: i32, b: i32, c: (u8, u8, u8)) {
    let br = CreateSolidBrush(rgb(c.0, c.1, c.2));
    let pen = CreatePen(PS_SOLID, 1, rgb(c.0, c.1, c.2));
    let ob = SelectObject(dc, br.into());
    let op = SelectObject(dc, pen.into());
    let _ = Ellipse(dc, l, t, r, b);
    SelectObject(dc, ob);
    SelectObject(dc, op);
    let _ = DeleteObject(br.into());
    let _ = DeleteObject(pen.into());
}

unsafe fn line(dc: HDC, x1: i32, y1: i32, x2: i32, y2: i32, w: i32, c: (u8, u8, u8)) {
    let pen = CreatePen(PS_SOLID, w, rgb(c.0, c.1, c.2));
    let op = SelectObject(dc, pen.into());
    let _ = MoveToEx(dc, x1, y1, None);
    let _ = LineTo(dc, x2, y2);
    SelectObject(dc, op);
    let _ = DeleteObject(pen.into());
}

unsafe fn tri(dc: HDC, pts: [(i32, i32); 3], c: (u8, u8, u8)) {
    let p: [POINT; 3] = [
        POINT { x: pts[0].0, y: pts[0].1 },
        POINT { x: pts[1].0, y: pts[1].1 },
        POINT { x: pts[2].0, y: pts[2].1 },
    ];
    let br = CreateSolidBrush(rgb(c.0, c.1, c.2));
    let pen = CreatePen(PS_SOLID, 1, rgb(c.0, c.1, c.2));
    let ob = SelectObject(dc, br.into());
    let op = SelectObject(dc, pen.into());
    let _ = Polygon(dc, &p);
    SelectObject(dc, ob);
    SelectObject(dc, op);
    let _ = DeleteObject(br.into());
    let _ = DeleteObject(pen.into());
}

unsafe fn draw_icon(dc: HDC, idx: i32) {
    match idx {
        0 => {
            // Add URL: lingkaran hijau + plus putih.
            fellipse(dc, 2, 2, 22, 22, GREEN);
            frect(dc, 10, 6, 14, 18, WHITE);
            frect(dc, 6, 10, 18, 14, WHITE);
        }
        1 => tri(dc, [(7, 5), (7, 19), (19, 12)], GREEN), // Resume: play
        2 => frect(dc, 5, 5, 19, 19, RED),                // Stop
        3 => {
            // Stop All: kotak merah + garis putih bertumpuk.
            frect(dc, 4, 5, 20, 19, RED);
            frect(dc, 7, 9, 17, 10, WHITE);
            frect(dc, 7, 13, 17, 14, WHITE);
        }
        4 => {
            // Delete: X merah tebal.
            line(dc, 6, 6, 18, 18, 3, RED);
            line(dc, 18, 6, 6, 18, 3, RED);
        }
        5 => {
            // Delete Completed: X merah kecil + centang emas.
            line(dc, 5, 6, 13, 14, 2, RED);
            line(dc, 13, 6, 5, 14, 2, RED);
            line(dc, 13, 16, 16, 20, 2, GOLD);
            line(dc, 16, 20, 22, 11, 2, GOLD);
        }
        6 => {
            // Options: roda gigi (gear).
            frect(dc, 11, 1, 13, 5, GRAY);
            frect(dc, 11, 19, 13, 23, GRAY);
            frect(dc, 1, 11, 5, 13, GRAY);
            frect(dc, 19, 11, 23, 13, GRAY);
            fellipse(dc, 4, 4, 20, 20, GRAY);
            fellipse(dc, 9, 9, 15, 15, WHITE);
        }
        7 => {
            // Scheduler: jam.
            fellipse(dc, 2, 2, 22, 22, GRAY);
            fellipse(dc, 4, 4, 20, 20, WHITE);
            line(dc, 12, 12, 12, 6, 2, GRAY);
            line(dc, 12, 12, 16, 14, 2, GRAY);
        }
        8 => {
            // Start Queue: play + bar.
            tri(dc, [(6, 5), (6, 19), (16, 12)], GREEN);
            frect(dc, 17, 5, 21, 19, DGREEN);
        }
        9 => {
            // Stop Queue: kotak + bar.
            frect(dc, 5, 5, 15, 19, RED);
            frect(dc, 17, 5, 21, 19, RED);
        }
        10 => {
            // Tell a Friend: amplop biru.
            frect(dc, 3, 7, 21, 18, BLUE);
            line(dc, 3, 7, 12, 13, 1, WHITE);
            line(dc, 21, 7, 12, 13, 1, WHITE);
        }
        _ => {}
    }
}

/// Bangun ImageList 24x24 berisi 11 ikon toolbar (GDI, masking magenta).
unsafe fn build_toolbar_imagelist() -> HIMAGELIST {
    const N: i32 = 11;
    const SZ: i32 = 24;
    let key = rgb(255, 0, 255);
    let himl = ImageList_Create(SZ, SZ, ILC_COLOR24 | ILC_MASK, N, 0);
    let screen = GetDC(None);
    let dc = CreateCompatibleDC(Some(screen));
    for idx in 0..N {
        let bmp = CreateCompatibleBitmap(screen, SZ, SZ);
        let old = SelectObject(dc, bmp.into());
        let kb = CreateSolidBrush(key);
        let rc = RECT { left: 0, top: 0, right: SZ, bottom: SZ };
        FillRect(dc, &rc, kb);
        let _ = DeleteObject(kb.into());
        draw_icon(dc, idx);
        SelectObject(dc, old);
        ImageList_AddMasked(himl, bmp, key);
        let _ = DeleteObject(bmp.into());
    }
    let _ = DeleteDC(dc);
    ReleaseDC(None, screen);
    himl
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
