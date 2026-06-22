//! ADM (Linux/egui) — UI bergaya IDM (clone). Layout meniru versi Windows
//! (`adm-app/gui.rs`): menu bar, toolbar tombol, sidebar pohon kategori,
//! tabel unduhan multi-kolom, status bar. Engine `adm-core` jalan in-process.

mod autostart;
mod category;
mod engine;
mod fileicon;
mod ipc;
mod scheduler;
mod settings;
mod store;
mod tasks;
mod tray;

use category::Category;
use eframe::egui;
use egui_extras::{Column, TableBuilder};
use engine::{EngineEvent, EngineHandle};
use ipc::IpcCommand;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

fn main() -> eframe::Result<()> {
    // Single-instance, dua lapis:
    // 1. Jalur cepat: bila ada ADM yang sudah hidup & membalas, aktifkan ia, keluar.
    if ipc::try_activate_existing() {
        return Ok(());
    }
    // 2. Pemilihan primary atomik via flock. Tanpa ini, beberapa proses yang
    //    lahir nyaris bersamaan (browser men-spawn satu app per pesan bridge)
    //    sama-sama lolos jalur (1) lalu membuka banyak jendela utama.
    //    Pemegang kunci = primary; `_primary` ditahan selama proses hidup.
    let _primary = match ipc::acquire_primary() {
        Some(lock) => lock,
        None => {
            // Primary lain sedang start — aktifkan ia (retry hingga socket siap)
            // lalu keluar tanpa membuka jendela.
            ipc::activate_with_retry();
            return Ok(());
        }
    };

    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1180.0, 720.0])
        .with_min_inner_size([860.0, 440.0])
        .with_title("Alpha Download Manager")
        // app_id = WM_CLASS (X11) / app_id (Wayland). Dock GNOME mencocokkan ini
        // ke `adm-egui.desktop` (StartupWMClass=adm-egui) untuk ambil ikonnya;
        // tanpa ini dock memakai ikon generik.
        .with_app_id("adm-egui");
    if let Some(icon) = load_app_icon() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "adm-egui",
        options,
        Box::new(|cc| Ok(Box::new(AdmApp::new(cc)))),
    )
}

/// Render `logo.svg` → `IconData` RGBA untuk ikon jendela/taskbar (pakai resvg).
fn load_app_icon() -> Option<egui::IconData> {
    use resvg::{tiny_skia, usvg};
    let svg = include_str!("../assets/logo.svg");
    let tree = usvg::Tree::from_str(svg, &usvg::Options::default()).ok()?;
    let size = 128u32;
    let mut pixmap = tiny_skia::Pixmap::new(size, size)?;
    let s = tree.size();
    let scale = (size as f32 / s.width()).min(size as f32 / s.height());
    let ts = tiny_skia::Transform::from_scale(scale, scale);
    resvg::render(&tree, ts, &mut pixmap.as_mut());

    // tiny_skia = premultiplied; IconData minta alpha terpisah → unpremultiply.
    let mut rgba = pixmap.take();
    for px in rgba.chunks_exact_mut(4) {
        let a = px[3];
        if a > 0 && a < 255 {
            px[0] = ((px[0] as u16 * 255) / a as u16) as u8;
            px[1] = ((px[1] as u16 * 255) / a as u16) as u8;
            px[2] = ((px[2] as u16 * 255) / a as u16) as u8;
        }
    }
    Some(egui::IconData {
        rgba,
        width: size,
        height: size,
    })
}

/// Folder unduhan default (XDG): `$XDG_DOWNLOAD_DIR` → `$HOME/Downloads` → `.`.
fn default_download_dir() -> PathBuf {
    if let Ok(d) = std::env::var("XDG_DOWNLOAD_DIR") {
        if !d.is_empty() {
            return PathBuf::from(d);
        }
    }
    let base = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let dir = PathBuf::from(base).join("Downloads");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Filter sidebar (mirror `gui.rs` F_*). Menentukan baris mana yang tampil.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Filter {
    All,
    Category(Category),
    Unfinished,
    Finished,
    Queues,
    Grabber,
}

/// Aksi yang dipicu dari context-menu / double-click pada baris tabel.
/// Dikumpulkan saat render lalu dieksekusi setelah tabel (hindari borrow ganda).
enum RowAction {
    Open,
    OpenFolder,
    Resume,
    Stop,
    Delete,
    Move(Category),
    /// Buka dialog progress/status (gaya IDM) untuk baris ini.
    Progress,
}

/// Operasi berkas (Tasks ▸ Export/Import) yang menunggu hasil dialog file async.
#[derive(Clone, Copy)]
enum FileOp {
    Export,
    Import,
}

#[derive(Clone, Copy, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
enum Status {
    Queued,
    Active,
    Completed,
    Paused,
    Failed,
}

impl Status {
    fn label(self) -> &'static str {
        match self {
            Status::Queued => "Queued",
            Status::Active => "Downloading",
            Status::Completed => "Complete",
            Status::Paused => "Paused",
            Status::Failed => "Error",
        }
    }
}

/// Satu baris unduhan di tabel.
struct Row {
    id: u64,
    url: String,
    filename: String,
    category: Category,
    downloaded: u64,
    total: Option<u64>,
    speed_bps: u64,
    status: Status,
    error: Option<String>,
    last_try: SystemTime,
    /// Progres per koneksi `(start, end, downloaded)` — transien (dialog progress).
    segments: Vec<(u64, u64, u64)>,
}

impl Row {
    fn matches(&self, f: Filter) -> bool {
        match f {
            Filter::All => true,
            Filter::Category(c) => self.category == c,
            Filter::Unfinished => self.status != Status::Completed,
            Filter::Finished => self.status == Status::Completed,
            Filter::Queues => self.status == Status::Queued,
            // Grabber ditangani lewat keanggotaan set (lihat `in_filter`).
            Filter::Grabber => false,
        }
    }
}

/// Apakah baris cocok dengan filter. Sama dgn `Row::matches`, kecuali Grabber
/// yang dicocokkan via set id (baris hasil site-grabber, dicatat in-memory).
fn in_filter(r: &Row, f: Filter, grabber: &HashSet<u64>) -> bool {
    match f {
        Filter::Grabber => grabber.contains(&r.id),
        _ => r.matches(f),
    }
}

/// Aksi "Options on completion" (tab ke-3 dialog progress, gaya IDM).
#[derive(Clone, Copy, PartialEq)]
enum WhenDone {
    ShutDown,
    Hibernate,
    Sleep,
    Exit,
}

impl WhenDone {
    fn label(self) -> &'static str {
        match self {
            WhenDone::ShutDown => "Shut down",
            WhenDone::Hibernate => "Hibernate",
            WhenDone::Sleep => "Sleep",
            WhenDone::Exit => "Exit",
        }
    }
}

/// Setelan "Options on completion" (tab ke-3). Dipersist per-id di `AdmApp`
/// agar tetap dieksekusi saat unduhan selesai walau dialog sudah ditutup.
#[derive(Clone, Copy)]
struct CompletionOpts {
    /// Tampilkan dialog "Download complete" saat selesai.
    show_complete: bool,
    /// Keluar dari ADM saat unduhan ini selesai.
    exit_done: bool,
    /// Jalankan aksi daya (`when_done`) saat selesai.
    poweroff_done: bool,
    when_done: WhenDone,
}

impl Default for CompletionOpts {
    fn default() -> Self {
        Self {
            show_complete: true, // gaya IDM: dialog complete tampil secara default
            exit_done: false,
            poweroff_done: false,
            when_done: WhenDone::ShutDown,
        }
    }
}

/// State per-dialog progress (gaya IDM, 3 tab). Disimpan per-id agar pilihan tab,
/// detail tampil, dan setelan Speed Limiter / Options bertahan antar-frame.
struct ProgressUi {
    /// Tab aktif: 0=Download status, 1=Speed Limiter, 2=Options on completion.
    tab: usize,
    /// Area detail (segment bar + tabel koneksi) tampil — tombol Hide/Show details.
    details: bool,
    /// Tampilkan URL unduhan (default sembunyi — link panjang merusak layout).
    show_url: bool,
    // Tab Speed Limiter.
    limit_on: bool,
    limit_kbps: u64,
    // Tab Options on completion.
    completion: CompletionOpts,
}

impl Default for ProgressUi {
    fn default() -> Self {
        Self {
            tab: 0,
            details: true,
            show_url: false,
            limit_on: false,
            limit_kbps: 0,
            completion: CompletionOpts::default(),
        }
    }
}

/// Status probe ukuran untuk dialog Add (diisi async oleh `probe_url`).
#[derive(Clone, Copy, Default)]
enum SizeState {
    #[default]
    Idle,
    Probing,
    Known(Option<u64>),
}

/// Status dialog Site Grabber, diisi oleh task async `grab_links`.
#[derive(Default)]
struct GrabState {
    fetching: bool,
    /// Sudah pernah Fetch (agar bedakan "kosong" vs "belum jalan").
    done: bool,
    links: Vec<String>,
    checked: Vec<bool>,
    error: Option<String>,
}

struct AdmApp {
    _rt: tokio::runtime::Runtime,
    engine: EngineHandle,
    rx: mpsc::Receiver<EngineEvent>,
    ipc_rx: mpsc::Receiver<IpcCommand>,
    download_dir: PathBuf,
    rows: Vec<Row>,
    index: HashMap<u64, usize>,
    filter: Filter,
    selected: Option<u64>,
    // Dialog "Add URL" / "Download File Info" (gaya IDM).
    show_add: bool,
    add_url: String,
    add_category: Category,
    add_filename: String,
    add_size: Arc<Mutex<SizeState>>,
    /// Nama berkas hasil probe (Content-Disposition) — diisi async, dikonsumsi
    /// sekali oleh dialog Add (mengisi "Save As" bila user belum mengetik manual).
    add_probe_name: Arc<Mutex<Option<String>>>,
    /// `true` bila user mengetik sendiri di "Save As" → jangan ditimpa hasil probe.
    add_filename_edited: bool,
    /// `false` saat dialog Add baru dibuka → frame berikut posisikan jendela
    /// viewport-nya ke tengah layar & fokus (sekali).
    add_centered: bool,
    /// Tema ikon sistem aktif (untuk ikon tipe-berkas gaya IDM).
    icon_theme: String,
    /// Cache tekstur ikon tipe-berkas per-ekstensi (lower-case).
    icon_cache: HashMap<String, Option<egui::TextureHandle>>,
    /// `true` bila dialog dibuka dari browser/bridge → judul "Download File Info".
    add_info: bool,
    /// Metadata titipan browser (referrer/UA/cookie) untuk add via IPC; dipakai
    /// saat dialog Add dikonfirmasi agar header tak hilang.
    pending_add: Option<adm_ipc::DownloadAddParams>,
    /// Kategori pilihan user di dialog Add (override deteksi otomatis), per id.
    cat_override: HashMap<u64, Category>,
    /// Dialog progress/status terbuka (gaya IDM, modeless): id → state UI dialog.
    progress_open: HashMap<u64, ProgressUi>,
    /// Setelan "Options on completion" per-id (persist walau dialog ditutup),
    /// dibaca saat unduhan selesai.
    completion: HashMap<u64, CompletionOpts>,
    /// Id dengan dialog "Download complete" terbuka (modeless).
    complete_open: HashSet<u64>,
    /// Permintaan tertunda dari "Options on completion" (dieksekusi setelah
    /// daftar dipersist, di akhir `drain_events`).
    pending_exit: bool,
    pending_power: Option<WhenDone>,
    // Dialog Options.
    show_options: bool,
    opt_dir: String,
    opt_queue_max: usize,
    opt_limit_kbps: u64,
    opt_autostart: bool,
    // Dialog About.
    show_about: bool,
    // Dialog Refresh Link (id baris target + buffer URL baru).
    refresh_target: Option<u64>,
    refresh_url: String,
    // Dialog batch download.
    show_batch: bool,
    batch_text: String,
    // Dialog site grabber.
    show_grabber: bool,
    grab_url: String,
    grab: Arc<Mutex<GrabState>>,
    /// Id baris yang berasal dari site-grabber (untuk node sidebar "Grabber").
    grabber_ids: HashSet<u64>,
    // Find.
    show_find: bool,
    find_query: String,
    /// Indeks baris (dalam daftar tampak) yang akan di-scroll oleh tabel.
    find_scroll: Option<usize>,
    // Dialog Scheduler (salinan kerja; disimpan ke scheduler.json saat OK).
    show_scheduler: bool,
    sched_edit: scheduler::Schedule,
    /// Hasil dialog file Export/Import (async portal) yang menunggu diproses.
    file_pick: Arc<Mutex<Option<(FileOp, PathBuf)>>>,
    /// `true` bila tray SNI berhasil didaftarkan. Menentukan perilaku tombol
    /// tutup jendela: ada tray → sembunyikan ke tray; tanpa tray (mis. GNOME
    /// polos) → minimize ke dock agar jendela selalu bisa dipanggil kembali.
    tray_active: Arc<AtomicBool>,
    /// Tema gelap (palet One Dark) aktif. Dipersist di settings.json.
    dark: bool,
}

impl AdmApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // Setelan persist (tema, folder, antrian, limit).
        let cfg = settings::load();
        // Tema dikunci ke pilihan app (tak ikut sistem); terang / One Dark.
        cc.egui_ctx.set_theme(egui::ThemePreference::Light);
        apply_theme(&cc.egui_ctx, cfg.dark);
        // Font lebih tebal + ukuran teks/kontrol/padding lebih lega.
        configure_style(&cc.egui_ctx);
        // Loader gambar (untuk ikon toolbar SVG Lucide).
        egui_extras::install_image_loaders(&cc.egui_ctx);

        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("gagal membangun runtime tokio");

        let (tx, rx) = mpsc::channel::<EngineEvent>();
        let ctx = cc.egui_ctx.clone();
        let sink: engine::EventSink = std::sync::Arc::new(move |ev| {
            let _ = tx.send(ev);
            ctx.request_repaint();
        });

        let download_dir = cfg
            .download_dir
            .clone()
            .map(PathBuf::from)
            .unwrap_or_else(default_download_dir);
        let engine = EngineHandle::new(rt.handle().clone(), download_dir.clone(), sink);
        engine.set_queue_max(cfg.queue_max);
        engine.set_global_limit(cfg.limit_kbps * 1024);

        // Server IPC bridge→app (native messaging + single-instance activate).
        let (ipc_tx, ipc_rx) = mpsc::channel::<IpcCommand>();
        let ipc_ctx = cc.egui_ctx.clone();
        let ipc_engine = engine.clone();
        rt.spawn(ipc::serve(ipc_tx, ipc_ctx, ipc_engine));

        // Tray SNI (best-effort): dipakai bila lingkungan mendukungnya.
        let tray_active = Arc::new(AtomicBool::new(false));
        tray::launch(engine.clone(), cc.egui_ctx.clone(), tray_active.clone());

        // Scheduler: thread pemicu start/stop antrian otomatis (jam & hari).
        scheduler::start(engine.clone());

        // Pulihkan daftar unduhan dari disk (M2) & cegah id baru bentrok.
        let (rows, max_id) = store::load();
        if max_id > 0 {
            engine.reserve_ids(max_id + 1);
        }
        let index = rows
            .iter()
            .enumerate()
            .map(|(i, r)| (r.id, i))
            .collect();
        let opt_dir = download_dir.display().to_string();

        Self {
            _rt: rt,
            engine,
            rx,
            ipc_rx,
            download_dir,
            rows,
            index,
            filter: Filter::All,
            selected: None,
            show_add: false,
            add_url: String::new(),
            add_category: Category::General,
            add_filename: String::new(),
            add_size: Arc::new(Mutex::new(SizeState::Idle)),
            add_probe_name: Arc::new(Mutex::new(None)),
            add_filename_edited: false,
            add_centered: false,
            icon_theme: fileicon::detect_theme(),
            icon_cache: HashMap::new(),
            add_info: false,
            pending_add: None,
            cat_override: HashMap::new(),
            progress_open: HashMap::new(),
            completion: HashMap::new(),
            complete_open: HashSet::new(),
            pending_exit: false,
            pending_power: None,
            show_options: false,
            opt_dir,
            opt_queue_max: cfg.queue_max,
            opt_limit_kbps: cfg.limit_kbps,
            opt_autostart: false,
            show_about: false,
            refresh_target: None,
            refresh_url: String::new(),
            show_batch: false,
            batch_text: String::new(),
            show_grabber: false,
            grab_url: String::new(),
            grab: Arc::new(Mutex::new(GrabState::default())),
            grabber_ids: HashSet::new(),
            show_find: false,
            find_query: String::new(),
            find_scroll: None,
            show_scheduler: false,
            sched_edit: scheduler::get(),
            file_pick: Arc::new(Mutex::new(None)),
            tray_active,
            dark: cfg.dark,
        }
    }

    /// Tulis setelan terkini ke disk.
    fn save_settings(&self) {
        settings::save(&settings::Settings {
            dark: self.dark,
            download_dir: Some(self.download_dir.display().to_string()),
            queue_max: self.opt_queue_max,
            limit_kbps: self.opt_limit_kbps,
        });
    }

    /// Ganti tema (terang ⇄ One Dark), terapkan ke konteks, lalu persist.
    fn set_dark(&mut self, ctx: &egui::Context, dark: bool) {
        if self.dark == dark {
            return;
        }
        self.dark = dark;
        apply_theme(ctx, dark);
        configure_style(ctx); // pertahankan font/ukuran setelah ganti visuals
        self.save_settings();
    }

    /// Tombol tutup jendela: beradaptasi dgn lingkungan.
    /// Ada tray → batalkan close & sembunyikan jendela (tetap jalan di tray;
    /// keluar betulan lewat menu Exit / tray Exit).
    /// Tanpa tray (GNOME polos) → JANGAN batalkan close: biarkan app keluar.
    /// Kalau di-intercept, "Quit" dari klik-kanan dock (yang bekerja dgn menutup
    /// semua window) tak akan mematikan app — dan tanpa tray tak ada cara lain
    /// memunculkannya kembali selain dock. Jadi close = exit, seperti app lain.
    fn handle_close(&self, ctx: &egui::Context) {
        if !ctx.input(|i| i.viewport().close_requested()) {
            return;
        }
        if self.tray_active.load(Ordering::SeqCst) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
        // Tanpa tray: tidak ada CancelClose → eframe menutup & proses keluar.
    }

    /// Proses perintah dari jalur IPC (browser bridge / instance kedua).
    fn drain_ipc(&mut self, ctx: &egui::Context) {
        while let Ok(cmd) = self.ipc_rx.try_recv() {
            match cmd {
                IpcCommand::Add(params) => {
                    self.add_url = params.url.clone();
                    let fname = params.filename.clone().unwrap_or_default();
                    self.pending_add = Some(params);
                    // Dialog Add kini jendela viewport tersendiri yang muncul di
                    // tengah & di atas browser — tak perlu menaikkan jendela utama.
                    self.open_add(ctx, true, &fname);
                }
                IpcCommand::Activate => Self::bring_to_front(ctx),
            }
        }
    }

    /// Munculkan & fokuskan jendela (dipakai single-instance & klik bridge).
    /// Penting: un-minimize dulu — bila jendela diminimalkan ke dock (mode tanpa
    /// tray), `Visible/Focus` saja tak mengembalikannya ke tengah layar.
    fn bring_to_front(ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    fn drain_events(&mut self) {
        // Tandai bila ada perubahan struktural (bukan sekadar progress) agar
        // daftar dipersist sekali di akhir, bukan tiap event progress.
        let mut dirty = false;
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                EngineEvent::Queued { id, url, output } => {
                    self.upsert(id, url, output, Status::Queued);
                    dirty = true;
                }
                EngineEvent::Started { id, url, output } => {
                    self.upsert(id, url, output, Status::Active);
                    dirty = true;
                }
                EngineEvent::Renamed { id, output } => {
                    if let Some(&i) = self.index.get(&id) {
                        self.rows[i].filename = filename_of(&output);
                        self.rows[i].category = Category::from_filename(&self.rows[i].filename);
                        dirty = true;
                    }
                }
                EngineEvent::Progress {
                    id,
                    downloaded,
                    total,
                    speed_bps,
                    segments,
                } => {
                    if let Some(&i) = self.index.get(&id) {
                        let r = &mut self.rows[i];
                        r.downloaded = downloaded;
                        r.total = total;
                        r.speed_bps = speed_bps;
                        r.segments = segments;
                        if r.status == Status::Queued {
                            r.status = Status::Active;
                        }
                    }
                }
                EngineEvent::Completed { id, bytes } => {
                    if let Some(&i) = self.index.get(&id) {
                        let r = &mut self.rows[i];
                        r.downloaded = bytes;
                        if r.total.is_none() {
                            r.total = Some(bytes);
                        }
                        r.speed_bps = 0;
                        r.status = Status::Completed;
                        dirty = true;
                    }
                    // Jendela proses auto-tutup saat selesai (digantikan jendela
                    // complete). "Options on completion": dialog complete / antrekan
                    // exit / aksi daya (dieksekusi setelah persist di bawah).
                    if let Some(opts) = self.completion.get(&id).copied() {
                        self.progress_open.remove(&id);
                        if opts.show_complete {
                            self.complete_open.insert(id);
                        }
                        if opts.exit_done {
                            self.pending_exit = true;
                        }
                        if opts.poweroff_done {
                            self.pending_power = Some(opts.when_done);
                        }
                    }
                }
                EngineEvent::Paused { id, downloaded } => {
                    if let Some(&i) = self.index.get(&id) {
                        let r = &mut self.rows[i];
                        r.downloaded = downloaded;
                        r.speed_bps = 0;
                        r.status = Status::Paused;
                        dirty = true;
                    }
                }
                EngineEvent::Failed { id, error } => {
                    if let Some(&i) = self.index.get(&id) {
                        let r = &mut self.rows[i];
                        r.speed_bps = 0;
                        r.status = Status::Failed;
                        r.error = Some(error);
                        dirty = true;
                    }
                }
            }
        }
        if dirty {
            store::save(&self.rows);
        }
        // Eksekusi aksi penyelesaian setelah daftar dipersist. Aksi daya dijalankan
        // lebih dulu (lalu app keluar); exit murni bila tak ada aksi daya.
        if let Some(w) = self.pending_power.take() {
            perform_power(w);
        }
        if self.pending_exit {
            std::process::exit(0);
        }
    }

    fn upsert(&mut self, id: u64, url: String, output: PathBuf, status: Status) {
        let filename = filename_of(&output);
        // Hormati kategori pilihan user di dialog Add bila ada; else deteksi.
        let category = self
            .cat_override
            .get(&id)
            .copied()
            .unwrap_or_else(|| Category::from_filename(&filename));
        if let Some(&i) = self.index.get(&id) {
            let r = &mut self.rows[i];
            r.url = url;
            r.filename = filename;
            r.category = category;
            r.status = status;
        } else {
            self.index.insert(id, self.rows.len());
            self.rows.push(Row {
                id,
                url,
                filename,
                category,
                downloaded: 0,
                total: None,
                speed_bps: 0,
                status,
                error: None,
                last_try: SystemTime::now(),
                segments: Vec::new(),
            });
        }
    }

    fn selected_row(&self) -> Option<&Row> {
        let id = self.selected?;
        self.index.get(&id).map(|&i| &self.rows[i])
    }

    fn add_download(&mut self, later: bool) {
        let url = self.add_url.trim().to_string();
        if url.is_empty() {
            return;
        }
        // Pakai metadata titipan browser (referrer/UA/cookie) bila add via IPC;
        // URL & nama hasil edit di dialog tetap diutamakan.
        let mut params = self.pending_add.take().unwrap_or_default();
        params.url = url;
        let fname = self.add_filename.trim();
        if !fname.is_empty() {
            params.filename = Some(fname.to_string());
        }
        let id = if later {
            self.engine.enqueue(params)
        } else {
            self.engine.add(params)
        };
        // Kategori pilihan user (override deteksi otomatis di upsert).
        self.cat_override.insert(id, self.add_category);
        // Buka dialog progress/status (gaya IDM) untuk unduhan yang langsung jalan.
        if !later {
            self.progress_open.entry(id).or_default();
        }
        self.add_url.clear();
        self.add_filename.clear();
        self.show_add = false;
    }

    /// Inisialisasi & buka dialog Add. `info` → judul "Download File Info"
    /// (dipicu browser/bridge). `filename_hint` mengisi field Save As.
    fn open_add(&mut self, ctx: &egui::Context, info: bool, filename_hint: &str) {
        self.add_info = info;
        self.add_filename = if !filename_hint.is_empty() {
            filename_hint.to_string()
        } else if !self.add_url.trim().is_empty() {
            guess_filename(self.add_url.trim())
        } else {
            String::new()
        };
        self.add_category = Category::from_filename(&self.add_filename);
        // Nama dari hint browser dianggap "sudah pasti" → jangan ditimpa probe.
        self.add_filename_edited = !filename_hint.is_empty();
        // Jendela dialog (viewport) belum dipusatkan untuk pembukaan ini.
        self.add_centered = false;
        *self.add_size.lock().unwrap() = SizeState::Idle;
        *self.add_probe_name.lock().unwrap() = None;
        let url = self.add_url.trim().to_string();
        if !url.is_empty() {
            self.probe_add_size(ctx, url);
        }
        self.show_add = true;
    }

    /// Probe URL (async): ukuran + nama berkas (Content-Disposition) untuk dialog Add.
    fn probe_add_size(&self, ctx: &egui::Context, url: String) {
        *self.add_size.lock().unwrap() = SizeState::Probing;
        let size_slot = self.add_size.clone();
        let name_slot = self.add_probe_name.clone();
        let ctx = ctx.clone();
        self.engine.runtime().spawn(async move {
            let probe = adm_core::probe_url(&url).await.ok();
            let total = probe.as_ref().and_then(|p| p.total);
            let name = probe.as_ref().and_then(|p| p.suggested_filename.clone());
            *size_slot.lock().unwrap() = SizeState::Known(total);
            if name.is_some() {
                *name_slot.lock().unwrap() = name;
            }
            ctx.request_repaint();
        });
    }

    /// Tekstur ikon tipe-berkas (tema sistem) untuk `filename`, di-cache per-ekstensi.
    fn file_icon(&mut self, ctx: &egui::Context, filename: &str) -> Option<egui::TextureHandle> {
        let key = std::path::Path::new(filename)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_default();
        if let Some(slot) = self.icon_cache.get(&key) {
            return slot.clone();
        }
        let tex = fileicon::lookup(filename, &self.icon_theme, 48)
            .and_then(|p| fileicon::load(&p, 48))
            .map(|img| {
                ctx.load_texture(format!("fileicon-{key}"), img, egui::TextureOptions::LINEAR)
            });
        self.icon_cache.insert(key, tex.clone());
        tex
    }

    fn resume_selected(&mut self) {
        let Some((id, url, filename)) = self
            .selected_row()
            .map(|r| (r.id, r.url.clone(), r.filename.clone()))
        else {
            return;
        };
        self.engine.resume(id, url, filename, false);
        // Gaya IDM: lanjutkan unduhan & buka kembali dialog progress/status.
        self.progress_open.entry(id).or_default();
    }

    fn stop_selected(&self) {
        if let Some(id) = self.selected {
            self.engine.cancel(id);
        }
    }

    fn delete_selected(&mut self) {
        if let Some(id) = self.selected.take() {
            self.engine.cancel(id);
            self.remove_row(id);
        }
    }

    fn delete_completed(&mut self) {
        let ids: Vec<u64> = self
            .rows
            .iter()
            .filter(|r| r.status == Status::Completed)
            .map(|r| r.id)
            .collect();
        for id in ids {
            self.remove_row(id);
        }
    }

    fn remove_row(&mut self, id: u64) {
        if let Some(&i) = self.index.get(&id) {
            self.rows.remove(i);
            // Bangun ulang indeks (posisi geser setelah remove).
            self.index.clear();
            for (i, r) in self.rows.iter().enumerate() {
                self.index.insert(r.id, i);
            }
            // Bersihkan state terkait id agar tak ada sisa basi (mis. aksi
            // penyelesaian yang tak akan pernah dipicu).
            self.progress_open.remove(&id);
            self.complete_open.remove(&id);
            self.completion.remove(&id);
            store::save(&self.rows);
        }
    }

    /// Path berkas baris = folder unduhan [/ subfolder kategori] / nama berkas.
    /// (Engine menulis ke lokasi ini; baris tak menyimpan path penuh.)
    fn row_path(&self, r: &Row) -> PathBuf {
        let mut dir = self.download_dir.clone();
        if let Some(folder) = r.category.folder() {
            dir.push(folder);
        }
        dir.join(&r.filename)
    }

    /// Jalankan aksi context-menu/double-click pada satu baris.
    fn do_row_action(&mut self, id: u64, action: RowAction) {
        let Some(&i) = self.index.get(&id) else { return };
        match action {
            RowAction::Open => open_path(&self.row_path(&self.rows[i])),
            RowAction::OpenFolder => {
                if let Some(parent) = self.row_path(&self.rows[i]).parent() {
                    open_path(parent);
                }
            }
            RowAction::Resume => {
                self.selected = Some(id);
                self.resume_selected();
            }
            RowAction::Stop => self.engine.cancel(id),
            RowAction::Delete => {
                self.selected = Some(id);
                self.delete_selected();
            }
            RowAction::Move(cat) => self.move_category(id, cat),
            RowAction::Progress => {
                self.progress_open.entry(id).or_default();
            }
        }
    }

    /// Pindahkan baris ke kategori lain: pindahkan berkas ke subfolder kategori
    /// baru (bila ada di disk) lalu perbarui baris. Persist setelahnya.
    fn move_category(&mut self, id: u64, cat: Category) {
        let Some(&i) = self.index.get(&id) else { return };
        if self.rows[i].category == cat {
            return;
        }
        let old = self.row_path(&self.rows[i]);
        let mut new_dir = self.download_dir.clone();
        if let Some(folder) = cat.folder() {
            new_dir.push(folder);
        }
        let new = new_dir.join(&self.rows[i].filename);
        if old.exists() {
            let _ = std::fs::create_dir_all(&new_dir);
            let _ = std::fs::rename(&old, &new);
        }
        self.rows[i].category = cat;
        store::save(&self.rows);
    }

    /// Buka dialog Options dengan folder unduhan & status autostart terkini.
    fn open_options(&mut self) {
        self.opt_dir = self.download_dir.display().to_string();
        self.opt_autostart = autostart::is_enabled();
        self.show_options = true;
    }

    /// Buka dialog Refresh Link untuk baris terpilih (prefill URL saat ini).
    fn open_refresh(&mut self) {
        if let Some(r) = self.selected_row() {
            let (url, id) = (r.url.clone(), r.id);
            self.refresh_url = url;
            self.refresh_target = Some(id);
        }
    }

    /// Terapkan setelan dari dialog Options ke engine.
    fn apply_options(&mut self) {
        let dir = PathBuf::from(self.opt_dir.trim());
        if !self.opt_dir.trim().is_empty() {
            let _ = std::fs::create_dir_all(&dir);
            self.download_dir = dir.clone();
            self.engine.set_download_dir(dir);
        }
        self.engine.set_queue_max(self.opt_queue_max);
        self.engine.set_global_limit(self.opt_limit_kbps * 1024);
        // Autostart hanya ditulis bila status berubah dari kondisi di disk.
        if self.opt_autostart != autostart::is_enabled() {
            autostart::set(self.opt_autostart);
        }
        self.save_settings();
    }

    /// Refresh Link: ganti URL baris terpilih lalu lanjutkan unduhan dengan URL baru.
    fn apply_refresh(&mut self) {
        let Some(id) = self.refresh_target.take() else { return };
        let url = self.refresh_url.trim().to_string();
        if url.is_empty() {
            return;
        }
        if let Some(&i) = self.index.get(&id) {
            self.rows[i].url = url.clone();
            let filename = self.rows[i].filename.clone();
            store::save(&self.rows);
            self.engine.resume(id, url, filename, false);
        }
        self.refresh_url.clear();
    }

    // ---- Batch download ----

    /// Buka dialog batch kosong (Tasks → Add batch download...).
    fn open_batch(&mut self) {
        self.batch_text.clear();
        self.show_batch = true;
    }

    /// Buka dialog batch dengan URL hasil ekstraksi dari clipboard
    /// (Tasks → Add batch download from clipboard).
    fn open_batch_from_clipboard(&mut self) {
        let text = tasks::read_clipboard().unwrap_or_default();
        self.batch_text = tasks::extract_urls(&text).join("\n");
        self.show_batch = true;
    }

    /// Enqueue semua URL hasil parse teks batch (wildcard diekspansi).
    fn add_batch(&mut self) {
        let urls = tasks::parse_batch(&self.batch_text);
        for url in urls {
            self.engine.enqueue(adm_ipc::DownloadAddParams {
                url,
                ..Default::default()
            });
        }
        self.batch_text.clear();
        self.show_batch = false;
    }

    // ---- Site grabber ----

    /// Buka dialog site grabber (reset state hasil sebelumnya).
    fn open_grabber(&mut self) {
        self.grab_url.clear();
        *self.grab.lock().unwrap() = GrabState::default();
        self.show_grabber = true;
    }

    /// Mulai ambil tautan dari URL halaman (async di runtime engine).
    fn fetch_grabber(&self, ctx: &egui::Context) {
        let url = self.grab_url.trim().to_string();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            self.grab.lock().unwrap().error = Some("URL halaman tidak valid.".into());
            return;
        }
        {
            let mut g = self.grab.lock().unwrap();
            g.fetching = true;
            g.error = None;
        }
        let state = self.grab.clone();
        let ctx = ctx.clone();
        self.engine.runtime().spawn(async move {
            let res = adm_core::grab_links(&url).await;
            {
                let mut g = state.lock().unwrap();
                g.fetching = false;
                g.done = true;
                match res {
                    Ok(links) => {
                        g.checked = vec![true; links.len()];
                        g.links = links;
                        g.error = None;
                    }
                    Err(e) => g.error = Some(e.to_string()),
                }
            }
            ctx.request_repaint();
        });
    }

    /// Enqueue tautan grabber yang tercentang; catat id untuk node "Grabber".
    fn download_grabbed(&mut self, urls: Vec<String>) {
        for url in urls {
            let id = self.engine.enqueue(adm_ipc::DownloadAddParams {
                url,
                ..Default::default()
            });
            self.grabber_ids.insert(id);
        }
        self.show_grabber = false;
    }

    // ---- Find ----

    /// Cari (substring, case-insensitive) pada nama berkas di daftar tampak,
    /// melingkar. `from_next` → mulai setelah baris terpilih (Find Next).
    fn run_find(&mut self, from_next: bool) {
        let q = self.find_query.trim().to_lowercase();
        if q.is_empty() {
            return;
        }
        let visible: Vec<usize> = (0..self.rows.len())
            .filter(|&i| in_filter(&self.rows[i], self.filter, &self.grabber_ids))
            .collect();
        if visible.is_empty() {
            return;
        }
        let start = if from_next {
            self.selected
                .and_then(|sel| visible.iter().position(|&i| self.rows[i].id == sel))
                .map(|p| p + 1)
                .unwrap_or(0)
        } else {
            0
        };
        let n = visible.len();
        for off in 0..n {
            let vi = (start + off) % n;
            let r = &self.rows[visible[vi]];
            if r.filename.to_lowercase().contains(&q) {
                self.selected = Some(r.id);
                self.find_scroll = Some(vi);
                return;
            }
        }
    }

    // ---- Scheduler ----

    /// Buka dialog Scheduler dengan setelan terkini.
    fn open_scheduler(&mut self) {
        self.sched_edit = scheduler::get();
        self.show_scheduler = true;
    }

    // ---- Export / Import daftar URL ----

    /// Pilih lokasi simpan (.txt) lewat portal file (dialog sync di thread
    /// terpisah agar UI tak beku); hasilnya diproses di `drain_file_pick`.
    fn export_urls(&self, ctx: &egui::Context) {
        if self.rows.is_empty() {
            return;
        }
        let slot = self.file_pick.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            if let Some(path) = rfd::FileDialog::new()
                .set_title("Export URL list")
                .set_file_name("adm-downloads.txt")
                .add_filter("Text", &["txt"])
                .save_file()
            {
                *slot.lock().unwrap() = Some((FileOp::Export, path));
                ctx.request_repaint();
            }
        });
    }

    /// Pilih berkas daftar URL untuk diimpor (dialog portal sync di thread lain).
    fn import_urls(&self, ctx: &egui::Context) {
        let slot = self.file_pick.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            if let Some(path) = rfd::FileDialog::new()
                .set_title("Import URL list")
                .add_filter("Text", &["txt"])
                .pick_file()
            {
                *slot.lock().unwrap() = Some((FileOp::Import, path));
                ctx.request_repaint();
            }
        });
    }

    /// Proses hasil dialog Export/Import yang sudah tiba dari portal.
    fn drain_file_pick(&mut self) {
        let pick = self.file_pick.lock().unwrap().take();
        let Some((op, path)) = pick else { return };
        match op {
            FileOp::Export => {
                let urls: Vec<String> = self.rows.iter().map(|r| r.url.clone()).collect();
                let _ = std::fs::write(&path, urls.join("\n"));
            }
            FileOp::Import => {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    for url in tasks::parse_batch(&text) {
                        self.engine.enqueue(adm_ipc::DownloadAddParams {
                            url,
                            ..Default::default()
                        });
                    }
                }
            }
        }
    }
}

impl eframe::App for AdmApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.drain_events();
        self.drain_ipc(ui.ctx());
        self.drain_file_pick();
        self.handle_close(ui.ctx());

        // Pintasan: Ctrl+F = Find, F3 = Find Next.
        if ui.ctx().input(|i| i.modifiers.command && i.key_pressed(egui::Key::F)) {
            self.show_find = true;
        }
        if ui.ctx().input(|i| i.key_pressed(egui::Key::F3)) {
            self.run_find(true);
        }

        self.menu_bar(ui);
        self.toolbar(ui);
        self.status_bar(ui);
        self.sidebar(ui);
        self.table(ui);
        self.add_dialog(ui.ctx());
        self.options_dialog(ui);
        self.about_dialog(ui);
        self.refresh_dialog(ui);
        self.batch_dialog(ui);
        self.grabber_dialog(ui);
        self.find_dialog(ui);
        self.scheduler_dialog(ui);
        self.progress_dialogs(ui.ctx());
        self.complete_dialogs(ui.ctx());
    }
}

impl AdmApp {
    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("menubar").show_inside(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                let has_sel = self.selected.is_some();

                // ---- Tasks ----
                ui.menu_button("Tasks", |ui| {
                    if ui.button("Add new download...\tCtrl+N").clicked() {
                        self.add_url.clear();
                        self.pending_add = None;
                        self.open_add(ui.ctx(), false, "");
                        ui.close();
                    }
                    if ui.button("Add batch download...").clicked() {
                        self.open_batch();
                        ui.close();
                    }
                    if ui.button("Add batch download from clipboard").clicked() {
                        self.open_batch_from_clipboard();
                        ui.close();
                    }
                    if ui.button("Run site grabber...").clicked() {
                        self.open_grabber();
                        ui.close();
                    }
                    ui.separator();
                    if ui.add_enabled(!self.rows.is_empty(), egui::Button::new("Export...")).clicked() {
                        self.export_urls(ui.ctx());
                        ui.close();
                    }
                    if ui.button("Import...").clicked() {
                        self.import_urls(ui.ctx());
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Exit\tCtrl+Q").clicked() {
                        std::process::exit(0);
                    }
                });

                // ---- File ----
                ui.menu_button("File", |ui| {
                    if ui.add_enabled(has_sel, egui::Button::new("Stop Download")).clicked() {
                        self.stop_selected();
                        ui.close();
                    }
                    if ui.add_enabled(has_sel, egui::Button::new("Remove")).clicked() {
                        self.delete_selected();
                        ui.close();
                    }
                    if ui.add_enabled(has_sel, egui::Button::new("Download Now")).clicked() {
                        self.resume_selected();
                        ui.close();
                    }
                    if ui.add_enabled(has_sel, egui::Button::new("Redownload")).clicked() {
                        self.resume_selected();
                        ui.close();
                    }
                });

                // ---- Downloads ----
                ui.menu_button("Downloads", |ui| {
                    if ui.button("Pause All").clicked() {
                        self.engine.cancel_all();
                        ui.close();
                    }
                    if ui.button("Stop All").clicked() {
                        self.engine.cancel_all();
                        ui.close();
                    }
                    if ui.button("Delete All Completed").clicked() {
                        self.delete_completed();
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Find...\tCtrl+F").clicked() {
                        self.show_find = true;
                        ui.close();
                    }
                    if ui.add_enabled(!self.find_query.is_empty(), egui::Button::new("Find Next\tF3")).clicked() {
                        self.run_find(true);
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Scheduler...").clicked() {
                        self.open_scheduler();
                        ui.close();
                    }
                    if ui.button("Start queue").clicked() {
                        self.engine.start_queue();
                        ui.close();
                    }
                    if ui.button("Stop queue").clicked() {
                        self.engine.stop_queue();
                        ui.close();
                    }
                    ui.menu_button("Speed Limiter", |ui| {
                        for (label, kbps) in [
                            ("Unlimited", 0u64),
                            ("50 KB/s", 50),
                            ("100 KB/s", 100),
                            ("500 KB/s", 500),
                            ("1 MB/s", 1024),
                            ("5 MB/s", 5120),
                        ] {
                            if ui.button(label).clicked() {
                                self.engine.set_global_limit(kbps * 1024);
                                ui.close();
                            }
                        }
                    });
                    ui.separator();
                    if ui.button("Options...").clicked() {
                        self.open_options();
                        ui.close();
                    }
                });

                // ---- View ----
                ui.menu_button("View", |ui| {
                    let mut dark = self.dark;
                    if ui.checkbox(&mut dark, "Dark mode (One Dark)").clicked() {
                        self.set_dark(ui.ctx(), dark);
                        ui.close();
                    }
                    ui.separator();
                    ui.add_enabled(false, egui::Button::new("Hide categories"));
                    ui.add_enabled(false, egui::Button::new("Arrange files"));
                    ui.add_enabled(false, egui::Button::new("Toolbar"));
                    ui.add_enabled(false, egui::Button::new("ADM tray icon"));
                    ui.add_enabled(false, egui::Button::new("Customize URL List..."));
                });

                // ---- Help ----
                ui.menu_button("Help", |ui| {
                    if ui.button("About ADM").clicked() {
                        self.show_about = true;
                        ui.close();
                    }
                });
            });
        });
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("toolbar").show_inside(ui, |ui| {
            ui.add_space(4.0);
            let has_sel = self.selected.is_some();
            let sel_resumable = self
                .selected_row()
                .map(|r| matches!(r.status, Status::Paused | Status::Failed))
                .unwrap_or(false);
            let sel_active = self
                .selected_row()
                .map(|r| r.status == Status::Active)
                .unwrap_or(false);
            let queued = self.rows.iter().filter(|r| r.status == Status::Queued).count();
            let sched = scheduler::get();
            let mut open_sched = false;

            ui.horizontal(|ui| {
                // Urutan persis versi Windows (adm-app/gui.rs::add_toolbar_buttons).
                if tbtn(ui, icon_add_url(), "Add URL", true) {
                    self.add_url.clear();
                    self.pending_add = None;
                    self.open_add(ui.ctx(), false, "");
                }
                ui.separator();
                if tbtn(ui, icon_resume(), "Resume", sel_resumable) {
                    self.resume_selected();
                }
                if tbtn(ui, icon_stop(), "Stop", sel_active) {
                    self.stop_selected();
                }
                if tbtn(ui, icon_stop_all(), "Stop All", true) {
                    self.engine.cancel_all();
                }
                if tbtn(ui, icon_delete(), "Delete", has_sel) {
                    self.delete_selected();
                }
                if tbtn(ui, icon_delete_completed(), "Delete Completed", true) {
                    self.delete_completed();
                }
                ui.separator();
                if tbtn(ui, icon_options(), "Options", true) {
                    self.open_options();
                }
                if tbtn(ui, icon_scheduler(), "Scheduler", true) {
                    open_sched = true;
                }
                if tbtn(ui, icon_refresh_link(), "Refresh Link", has_sel) {
                    self.open_refresh();
                }
                ui.separator();
                let (sq_clicked, sq_arrow) = tbtn_dd(ui, icon_start_queue(), "Start Queue", true);
                if sq_clicked {
                    self.engine.start_queue();
                }
                if queue_popup(&sq_arrow, queued, &sched) {
                    open_sched = true;
                }
                let (tq_clicked, tq_arrow) = tbtn_dd(ui, icon_stop_queue(), "Stop Queue", true);
                if tq_clicked {
                    self.engine.stop_queue();
                }
                if queue_popup(&tq_arrow, queued, &sched) {
                    open_sched = true;
                }
                ui.separator();
                if tbtn(ui, icon_updates(), "Updates", true) {
                    open_url("https://github.com/s4rt4/adm-lin");
                }
            });
            ui.add_space(4.0);
            if open_sched {
                self.open_scheduler();
            }
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("statusbar").show_inside(ui, |ui| {
            let active = self.rows.iter().filter(|r| r.status == Status::Active).count();
            let total_speed: u64 = self.rows.iter().map(|r| r.speed_bps).sum();
            ui.horizontal(|ui| {
                ui.label(format!("{} download(s)", self.rows.len()));
                ui.separator();
                ui.label(format!("{active} active"));
                ui.separator();
                ui.label(format!("{}/s", human_bytes(total_speed)));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(self.download_dir.display().to_string());
                });
            });
        });
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::left("sidebar")
            .resizable(true)
            .default_size(180.0)
            .show_inside(ui, |ui| {
                ui.add_space(4.0);
                let grabber = &self.grabber_ids;
                let rows = &self.rows;
                let count = |f: Filter| rows.iter().filter(|r| in_filter(r, f, grabber)).count();
                let node = |ui: &mut egui::Ui, cur: &mut Filter, f: Filter, label: &str, indent: bool| {
                    let txt = format!("{label} ({})", count(f));
                    ui.horizontal(|ui| {
                        if indent {
                            ui.add_space(16.0);
                        }
                        if ui.selectable_label(*cur == f, txt).clicked() {
                            *cur = f;
                        }
                    });
                };

                egui::ScrollArea::vertical().show(ui, |ui| {
                    node(ui, &mut self.filter, Filter::All, "📥 All Downloads", false);
                    node(ui, &mut self.filter, Filter::Category(Category::Compressed), "🗜 Compressed", true);
                    node(ui, &mut self.filter, Filter::Category(Category::Documents), "📄 Documents", true);
                    node(ui, &mut self.filter, Filter::Category(Category::Music), "🎵 Music", true);
                    node(ui, &mut self.filter, Filter::Category(Category::Programs), "⚙ Programs", true);
                    node(ui, &mut self.filter, Filter::Category(Category::Video), "🎬 Video", true);
                    ui.separator();
                    node(ui, &mut self.filter, Filter::Unfinished, "⏳ Unfinished", false);
                    node(ui, &mut self.filter, Filter::Finished, "✔ Finished", false);
                    ui.separator();
                    node(ui, &mut self.filter, Filter::Queues, "🗂 Queues", false);
                    node(ui, &mut self.filter, Filter::Grabber, "🌐 Grabber projects", false);
                });
            });
    }

    fn table(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().show_inside(ui, |ui| {
            // Rapatkan baris: tanpa ini, item_spacing vertikal global (7px) jadi
            // celah antar-baris yang ikut tertutup highlight seleksi (tampak
            // "tumpah" ke baris bawah) & membuat garis grid 26px meleset dari
            // baris asli (26+7px). 0 → pitch baris = 26px, selaras dgn grid.
            ui.spacing_mut().item_spacing.y = 0.0;
            let filter = self.filter;
            let visible: Vec<usize> = (0..self.rows.len())
                .filter(|&i| in_filter(&self.rows[i], filter, &self.grabber_ids))
                .collect();

            let mut clicked: Option<u64> = None;
            let mut action: Option<(u64, RowAction)> = None;
            // Area penuh tabel (dipakai menggambar grid horizontal termasuk baris kosong).
            let body_area = {
                let top = ui.cursor().min.y;
                let full = ui.max_rect();
                egui::Rect::from_min_max(egui::pos2(full.left(), top), full.max)
            };
            let mut table = TableBuilder::new(ui)
                .striped(true)
                .resizable(true)
                .sense(egui::Sense::click())
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::initial(240.0).at_least(140.0)) // File Name
                .column(Column::initial(28.0).at_least(24.0)) // Q
                .column(Column::initial(90.0).at_least(60.0)) // Size
                .column(Column::initial(100.0).at_least(70.0)) // Status
                .column(Column::initial(90.0).at_least(60.0)) // Time left
                .column(Column::initial(110.0).at_least(70.0)) // Transfer rate
                .column(Column::initial(130.0).at_least(80.0)) // Last Try
                .column(Column::remainder().at_least(80.0)); // Description
            // Scroll ke baris hasil Find (sekali, lalu reset).
            if let Some(idx) = self.find_scroll.take() {
                table = table.scroll_to_row(idx, Some(egui::Align::Center));
            }
            table
                .header(26.0, |mut h| {
                    for title in ["File Name", "Q", "Size", "Status", "Time left", "Transfer rate", "Last Try", "Description"] {
                        h.col(|ui| {
                            ui.centered_and_justified(|ui| {
                                ui.strong(title);
                            });
                        });
                    }
                })
                .body(|mut body| {
                    for &i in &visible {
                        let r = &self.rows[i];
                        let is_sel = self.selected == Some(r.id);
                        body.row(26.0, |mut row| {
                            row.set_selected(is_sel);
                            row.col(|ui| {
                                ui.label(&r.filename);
                            });
                            row.col(|ui| {
                                ui.label(if r.status == Status::Queued { "•" } else { "" });
                            });
                            row.col(|ui| {
                                ui.label(match r.total {
                                    Some(t) => human_bytes(t),
                                    None => "—".into(),
                                });
                            });
                            row.col(|ui| {
                                ui.label(status_text(r));
                            });
                            row.col(|ui| {
                                ui.label(time_left(r));
                            });
                            row.col(|ui| {
                                ui.label(if r.speed_bps > 0 {
                                    format!("{}/s", human_bytes(r.speed_bps))
                                } else {
                                    "—".into()
                                });
                            });
                            row.col(|ui| {
                                ui.label(fmt_time(r.last_try));
                            });
                            row.col(|ui| {
                                if let Some(e) = &r.error {
                                    ui.colored_label(egui::Color32::from_rgb(200, 80, 80), e);
                                } else {
                                    ui.weak(&r.url);
                                }
                            });
                            let resp = row.response();
                            if resp.clicked() {
                                clicked = Some(r.id);
                            }
                            // Double-click (gaya IDM): selesai → buka berkas;
                            // masih berjalan/jeda → buka dialog progress/status.
                            if resp.double_clicked() {
                                let act = if r.status == Status::Completed {
                                    RowAction::Open
                                } else {
                                    RowAction::Progress
                                };
                                action = Some((r.id, act));
                            }
                            let (id, status) = (r.id, r.status);
                            resp.context_menu(|ui| {
                                clicked = Some(id);
                                let resumable = matches!(status, Status::Paused | Status::Failed);
                                if ui.button("Open").clicked() {
                                    action = Some((id, RowAction::Open));
                                    ui.close();
                                }
                                if ui.button("Open containing folder").clicked() {
                                    action = Some((id, RowAction::OpenFolder));
                                    ui.close();
                                }
                                ui.separator();
                                if ui.add_enabled(resumable, egui::Button::new("Resume")).clicked() {
                                    action = Some((id, RowAction::Resume));
                                    ui.close();
                                }
                                if ui
                                    .add_enabled(status == Status::Active, egui::Button::new("Stop"))
                                    .clicked()
                                {
                                    action = Some((id, RowAction::Stop));
                                    ui.close();
                                }
                                ui.menu_button("Move to category", |ui| {
                                    for cat in [
                                        Category::Compressed,
                                        Category::Documents,
                                        Category::Music,
                                        Category::Programs,
                                        Category::Video,
                                        Category::General,
                                    ] {
                                        if ui.button(cat.label()).clicked() {
                                            action = Some((id, RowAction::Move(cat)));
                                            ui.close();
                                        }
                                    }
                                });
                                ui.separator();
                                if ui.button("Delete").clicked() {
                                    action = Some((id, RowAction::Delete));
                                    ui.close();
                                }
                            });
                        });
                    }
                });

            // Garis tipis pemisah antar-baris (gaya list IDM) — digambar merata
            // di seluruh area tabel, termasuk baris kosong.
            let line_color = if ui.visuals().dark_mode {
                egui::Color32::from_gray(58)
            } else {
                egui::Color32::from_gray(222)
            };
            let painter = ui.painter();
            let stroke = egui::Stroke::new(1.0, line_color);
            const HEADER_H: f32 = 26.0;
            const ROW_H: f32 = 26.0;
            let mut y = body_area.top() + HEADER_H;
            while y <= body_area.bottom() {
                painter.hline(body_area.x_range(), y, stroke);
                y += ROW_H;
            }

            if let Some(id) = clicked {
                self.selected = Some(id);
            }
            if let Some((id, act)) = action {
                self.do_row_action(id, act);
            }
        });
    }

    fn add_dialog(&mut self, ctx: &egui::Context) {
        if !self.show_add {
            return;
        }
        let mut action: Option<bool> = None; // Some(later)
        let mut reprobe = false;
        let mut close = false;
        // Terapkan nama berkas hasil probe (Content-Disposition) bila user belum
        // mengetik manual — agar "Save As" tak terhenti di download.bin.
        if let Some(name) = self.add_probe_name.lock().unwrap().take() {
            if !self.add_filename_edited && !name.trim().is_empty() {
                self.add_filename = name;
                self.add_category = Category::from_filename(&self.add_filename);
            }
        }
        // Ikon tipe-berkas dari tema sistem (gaya IDM) + teks ukuran.
        let icon = self.file_icon(ctx, &self.add_filename.clone());
        let size_txt = match *self.add_size.lock().unwrap() {
            SizeState::Idle => "—".to_string(),
            SizeState::Probing => "menghitung…".to_string(),
            SizeState::Known(Some(n)) => human_bytes(n),
            SizeState::Known(None) => "Tidak diketahui".to_string(),
        };
        // Judul mengikuti versi Windows (browser → "Download File Info").
        let title = if self.add_info { "Download File Info" } else { "Add new download" };

        // Dialog = jendela OS tersendiri (viewport) agar mengambang bebas, terlepas
        // dari jendela utama: muncul di tengah layar & (saat dipicu ekstensi) di atas
        // jendela browser yang sedang fokus — bukan ikut induk di dock.
        let vid = egui::ViewportId::from_hash_of("adm-add-dialog");
        let win_size = egui::vec2(560.0, 230.0);
        let mut builder = egui::ViewportBuilder::default()
            .with_title(title)
            .with_inner_size(win_size)
            .with_resizable(false)
            .with_minimize_button(false)
            .with_maximize_button(false);
        if self.add_info {
            builder = builder.with_window_level(egui::WindowLevel::AlwaysOnTop);
        }

        ctx.show_viewport_immediate(vid, builder, |ctx, _class| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    egui::Grid::new("add_grid").num_columns(2).spacing([10.0, 10.0]).show(ui, |ui| {
                        ui.label("URL:");
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut self.add_url)
                                .desired_width(360.0)
                                .hint_text("https://…"),
                        );
                        // URL selesai diedit (fokus lepas) → probe ukuran & tebak nama.
                        if resp.lost_focus() {
                            reprobe = true;
                        }
                        ui.end_row();

                        ui.label("Category:");
                        egui::ComboBox::from_id_salt("add_cat")
                            .selected_text(self.add_category.label())
                            .show_ui(ui, |ui| {
                                for cat in [
                                    Category::General,
                                    Category::Compressed,
                                    Category::Documents,
                                    Category::Music,
                                    Category::Programs,
                                    Category::Video,
                                ] {
                                    ui.selectable_value(&mut self.add_category, cat, cat.label());
                                }
                            });
                        ui.end_row();

                        ui.label("Save As:");
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut self.add_filename)
                                .desired_width(360.0)
                                .hint_text("nama berkas"),
                        );
                        // User mengetik sendiri → jangan ditimpa hasil probe.
                        if resp.changed() {
                            self.add_filename_edited = true;
                        }
                        ui.end_row();
                    });

                    // Kolom kanan gaya IDM: ikon tipe-berkas besar + ukuran di bawahnya.
                    ui.add_space(8.0);
                    ui.vertical_centered(|ui| {
                        if let Some(tex) = &icon {
                            let src = egui::load::SizedTexture::from_handle(tex);
                            ui.add(
                                egui::Image::new(src).fit_to_exact_size(egui::vec2(48.0, 48.0)),
                            );
                        } else {
                            ui.add_space(48.0);
                        }
                        ui.add_space(4.0);
                        ui.label(&size_txt);
                    });
                });
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    if ui.button("Start Download").clicked() {
                        action = Some(false);
                    }
                    if ui.button("Download Later").clicked() {
                        action = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            });

            // Pusatkan jendela ke layar & fokus, sekali per pembukaan.
            if !self.add_centered {
                if let Some(mon) = ctx.input(|i| i.viewport().monitor_size) {
                    let pos = ((mon - win_size) * 0.5).max(egui::Vec2::ZERO).to_pos2();
                    ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos));
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                self.add_centered = true;
            }
            // Tombol X jendela.
            if ctx.input(|i| i.viewport().close_requested()) {
                close = true;
            }
        });

        if close {
            self.add_url.clear();
            self.add_filename.clear();
            self.pending_add = None;
            self.show_add = false;
        }
        if reprobe {
            let url = self.add_url.trim().to_string();
            if self.add_filename.trim().is_empty() && !url.is_empty() {
                self.add_filename = guess_filename(&url);
                self.add_category = Category::from_filename(&self.add_filename);
            }
            if url.is_empty() {
                *self.add_size.lock().unwrap() = SizeState::Idle;
            } else {
                self.probe_add_size(ctx, url);
            }
        }
        if let Some(later) = action {
            if !self.add_url.trim().is_empty() {
                self.add_download(later);
            }
        }
    }

    fn options_dialog(&mut self, ui: &mut egui::Ui) {
        if !self.show_options {
            return;
        }
        let mut open = true;
        let mut apply = false;
        egui::Window::new("Options")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ui.ctx(), |ui| {
                ui.set_min_width(460.0);
                egui::Grid::new("opt_grid").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
                    ui.label("Download folder:");
                    ui.add(egui::TextEdit::singleline(&mut self.opt_dir).desired_width(300.0));
                    ui.end_row();

                    ui.label("Max concurrent (queue):");
                    ui.add(egui::DragValue::new(&mut self.opt_queue_max).range(1..=16));
                    ui.end_row();

                    ui.label("Speed limit:");
                    egui::ComboBox::from_id_salt("opt_limit")
                        .selected_text(if self.opt_limit_kbps == 0 {
                            "Unlimited".to_string()
                        } else {
                            format!("{} KB/s", self.opt_limit_kbps)
                        })
                        .show_ui(ui, |ui| {
                            for (label, kbps) in [
                                ("Unlimited", 0u64),
                                ("50 KB/s", 50),
                                ("100 KB/s", 100),
                                ("500 KB/s", 500),
                                ("1 MB/s", 1024),
                                ("5 MB/s", 5120),
                            ] {
                                ui.selectable_value(&mut self.opt_limit_kbps, kbps, label);
                            }
                        });
                    ui.end_row();

                    ui.label("Run at login:");
                    ui.checkbox(&mut self.opt_autostart, "Start ADM when I log in");
                    ui.end_row();
                });
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("OK").clicked() {
                        apply = true;
                    }
                    if ui.button("Cancel").clicked() {
                        self.show_options = false;
                    }
                });
            });
        if apply {
            self.apply_options();
            self.show_options = false;
        }
        if !open {
            self.show_options = false;
        }
    }

    fn about_dialog(&mut self, ui: &mut egui::Ui) {
        if !self.show_about {
            return;
        }
        let mut open = true;
        egui::Window::new("About ADM")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ui.ctx(), |ui| {
                ui.vertical_centered(|ui| {
                    ui.add(
                        egui::Image::new(egui::include_image!("../assets/logo.svg"))
                            .fit_to_exact_size(egui::vec2(72.0, 72.0)),
                    );
                    ui.add_space(6.0);
                    ui.heading("Alpha Download Manager");
                    ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
                    ui.label(format!("Engine {}", adm_core::version()));
                    ui.add_space(4.0);
                    ui.weak("Linux/Fedora port — egui");
                });
            });
        if !open {
            self.show_about = false;
        }
    }

    fn refresh_dialog(&mut self, ui: &mut egui::Ui) {
        if self.refresh_target.is_none() {
            return;
        }
        let mut open = true;
        let mut apply = false;
        egui::Window::new("Refresh Link")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ui.ctx(), |ui| {
                ui.set_min_width(440.0);
                ui.label("New address (URL):");
                ui.add(
                    egui::TextEdit::singleline(&mut self.refresh_url)
                        .desired_width(f32::INFINITY)
                        .hint_text("https://…"),
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Update & Resume").clicked() {
                        apply = true;
                    }
                    if ui.button("Cancel").clicked() {
                        self.refresh_target = None;
                        self.refresh_url.clear();
                    }
                });
            });
        if apply {
            self.apply_refresh();
        }
        if !open {
            self.refresh_target = None;
            self.refresh_url.clear();
        }
    }

    fn batch_dialog(&mut self, ui: &mut egui::Ui) {
        if !self.show_batch {
            return;
        }
        let mut open = true;
        let mut add = false;
        egui::Window::new("Add batch download")
            .collapsible(false)
            .resizable(true)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ui.ctx(), |ui| {
                ui.set_min_width(520.0);
                ui.label("Masukkan satu URL per baris. Pola angka didukung, mis. file[1-10].zip");
                ui.add_space(6.0);
                egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut self.batch_text)
                            .desired_width(f32::INFINITY)
                            .desired_rows(12)
                            .code_editor(),
                    );
                });
                let n = tasks::parse_batch(&self.batch_text).len();
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.add_enabled(n > 0, egui::Button::new(format!("Add {n} download(s)"))).clicked() {
                        add = true;
                    }
                    if ui.button("Cancel").clicked() {
                        self.batch_text.clear();
                        self.show_batch = false;
                    }
                });
            });
        if add {
            self.add_batch();
        }
        if !open {
            self.show_batch = false;
        }
    }

    fn grabber_dialog(&mut self, ui: &mut egui::Ui) {
        if !self.show_grabber {
            return;
        }
        let mut open = true;
        let mut fetch = false;
        let mut download: Option<Vec<String>> = None;
        egui::Window::new("Run site grabber")
            .collapsible(false)
            .resizable(true)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ui.ctx(), |ui| {
                ui.set_min_width(560.0);
                ui.horizontal(|ui| {
                    ui.label("Page URL:");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.grab_url)
                            .desired_width(380.0)
                            .hint_text("https://…"),
                    );
                    let fetching = self.grab.lock().unwrap().fetching;
                    if ui.add_enabled(!fetching, egui::Button::new("Fetch")).clicked() {
                        fetch = true;
                    }
                });
                ui.add_space(6.0);

                let mut g = self.grab.lock().unwrap();
                if g.fetching {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Mengambil tautan…");
                    });
                } else if let Some(e) = g.error.clone() {
                    ui.colored_label(egui::Color32::from_rgb(200, 80, 80), e);
                } else if g.done {
                    ui.label(format!("{} tautan ditemukan:", g.links.len()));
                }

                egui::ScrollArea::vertical().max_height(280.0).show(ui, |ui| {
                    for i in 0..g.links.len() {
                        let mut on = g.checked[i];
                        let label = g.links[i].clone();
                        if ui.checkbox(&mut on, label).changed() {
                            g.checked[i] = on;
                        }
                    }
                });
                drop(g);

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let g = self.grab.lock().unwrap();
                    let sel: Vec<String> = g
                        .links
                        .iter()
                        .zip(g.checked.iter())
                        .filter(|(_, &c)| c)
                        .map(|(u, _)| u.clone())
                        .collect();
                    drop(g);
                    if ui
                        .add_enabled(!sel.is_empty(), egui::Button::new(format!("Download selected ({})", sel.len())))
                        .clicked()
                    {
                        download = Some(sel);
                    }
                    if ui.button("Cancel").clicked() {
                        self.show_grabber = false;
                    }
                });
            });
        if fetch {
            self.fetch_grabber(ui.ctx());
        }
        if let Some(urls) = download {
            self.download_grabbed(urls);
        }
        if !open {
            self.show_grabber = false;
        }
    }

    fn find_dialog(&mut self, ui: &mut egui::Ui) {
        if !self.show_find {
            return;
        }
        let mut open = true;
        let mut go = false;
        egui::Window::new("Find")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ui.ctx(), |ui| {
                ui.set_min_width(360.0);
                ui.label("Cari berkas (nama):");
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.find_query)
                        .desired_width(f32::INFINITY),
                );
                resp.request_focus();
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    go = true;
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Find").clicked() {
                        go = true;
                    }
                    if ui.button("Close").clicked() {
                        self.show_find = false;
                    }
                });
            });
        if go {
            self.run_find(false);
            self.show_find = false;
        }
        if !open {
            self.show_find = false;
        }
    }

    fn scheduler_dialog(&mut self, ui: &mut egui::Ui) {
        if !self.show_scheduler {
            return;
        }
        let mut open = true;
        let mut save = false;
        egui::Window::new("Scheduler")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ui.ctx(), |ui| {
                ui.set_min_width(420.0);
                ui.checkbox(&mut self.sched_edit.enabled, "Aktifkan penjadwalan otomatis");
                ui.add_space(6.0);
                egui::Grid::new("sched_grid").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
                    ui.label("Mulai antrian:");
                    time_picker(ui, "sched_start", &mut self.sched_edit.start);
                    ui.end_row();
                    ui.label("Hentikan antrian:");
                    time_picker(ui, "sched_stop", &mut self.sched_edit.stop);
                    ui.end_row();
                });
                ui.add_space(8.0);
                ui.label("Hari aktif:");
                ui.horizontal(|ui| {
                    for (i, name) in ["Min", "Sen", "Sel", "Rab", "Kam", "Jum", "Sab"].iter().enumerate() {
                        ui.toggle_value(&mut self.sched_edit.days[i], *name);
                    }
                });
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("OK").clicked() {
                        save = true;
                    }
                    if ui.button("Cancel").clicked() {
                        self.show_scheduler = false;
                    }
                });
            });
        if save {
            scheduler::set(self.sched_edit.clone());
            self.show_scheduler = false;
        }
        if !open {
            self.show_scheduler = false;
        }
    }

    /// Dialog progress/status per-unduhan (gaya IDM): modeless, bisa banyak.
    /// Dialog progress/status gaya IDM (3 tab + segment bar + Hide details).
    fn progress_dialogs(&mut self, ctx: &egui::Context) {
        if self.progress_open.is_empty() {
            return;
        }
        let ids: Vec<u64> = self.progress_open.keys().copied().collect();
        let mut to_close = Vec::new();
        let mut to_pause = Vec::new();
        let mut to_resume = Vec::new();
        let mut to_cancel = Vec::new();
        let mut to_open = Vec::new();
        let mut to_limit: Vec<(u64, u64)> = Vec::new();
        for id in ids {
            let Some(&i) = self.index.get(&id) else {
                to_close.push(id);
                continue;
            };
            // Keluarkan state dialog (owned) agar bisa di-mutate sembari meminjam
            // `self.rows` immutable; dikembalikan setelah render bila masih dibuka.
            let mut st = self.progress_open.remove(&id).unwrap_or_default();
            let r = &self.rows[i];

            let pct = match r.total {
                Some(t) if t > 0 => ((r.downloaded as f64 / t as f64) * 100.0) as u32,
                _ => 0,
            };
            // Judul gaya IDM: "41%  namafile". Id window dipisah (stabil) agar
            // posisi/ukuran tak ter-reset tiap persentase berubah.
            let title = if r.total.is_some() {
                format!("{pct}%  {}", r.filename)
            } else {
                r.filename.clone()
            };

            // Dialog progress = jendela OS tersendiri (viewport), terlepas dari
            // jendela utama: bisa digeser/ditutup sendiri, tak ikut induk di dock.
            let vid = egui::ViewportId::from_hash_of(("adm-progress", id));
            let builder = egui::ViewportBuilder::default()
                .with_title(&title)
                .with_inner_size([560.0, 380.0])
                .with_resizable(true)
                .with_minimize_button(true);
            let mut closed = false;
            ctx.show_viewport_immediate(vid, builder, |ctx, _class| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.set_min_width(500.0);

                    // ---- Bar tab (emulasi SysTabControl versi Windows) ----
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut st.tab, 0usize, "Download status");
                        ui.selectable_value(&mut st.tab, 1usize, "Speed Limiter");
                        ui.selectable_value(&mut st.tab, 2usize, "Options on completion");
                    });
                    ui.separator();

                    match st.tab {
                        0 => Self::pg_tab_status(ui, id, r, &mut st),
                        1 => {
                            if Self::pg_tab_limiter(ui, &mut st) {
                                let bps = if st.limit_on { st.limit_kbps * 1024 } else { 0 };
                                to_limit.push((id, bps));
                            }
                        }
                        _ => Self::pg_tab_options(ui, id, &mut st),
                    }

                    ui.add_space(8.0);
                    ui.separator();
                    // ---- Tombol bawah (semua tab): Hide details | Resume/Pause Cancel Close ----
                    ui.horizontal(|ui| {
                        let det_label = if st.details { "<< Hide details" } else { "Show details >>" };
                        if ui
                            .add_enabled(st.tab == 0, egui::Button::new(det_label))
                            .clicked()
                        {
                            st.details = !st.details;
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Close").clicked() {
                                to_close.push(id);
                            }
                            if r.status == Status::Completed {
                                if ui.button("Open").clicked() {
                                    to_open.push(id);
                                }
                            } else if ui.button("Cancel").clicked() {
                                to_cancel.push(id);
                            }
                            match r.status {
                                Status::Active => {
                                    if ui.button("Pause").clicked() {
                                        to_pause.push(id);
                                    }
                                }
                                Status::Paused | Status::Failed => {
                                    if ui.button("Resume").clicked() {
                                        to_resume.push(id);
                                    }
                                }
                                _ => {}
                            }
                        });
                    });
                });
                // Tombol X jendela.
                if ctx.input(|i| i.viewport().close_requested()) {
                    closed = true;
                }
            });
            // Persist setelan "Options on completion" (tetap berlaku walau dialog
            // ditutup, dieksekusi saat unduhan selesai).
            self.completion.insert(id, st.completion);
            if closed {
                to_close.push(id);
            } else {
                self.progress_open.insert(id, st); // kembalikan state dialog
            }
        }
        for id in to_pause {
            self.engine.cancel(id); // stop, simpan parsial → jadi Paused
        }
        for id in to_resume {
            if let Some(&i) = self.index.get(&id) {
                let (url, fname) = (self.rows[i].url.clone(), self.rows[i].filename.clone());
                self.engine.resume(id, url, fname, false);
            }
        }
        for (id, bps) in to_limit {
            self.engine.set_limit(id, bps);
        }
        for id in to_cancel {
            self.engine.cancel(id);
            self.progress_open.remove(&id);
        }
        for id in to_open {
            if let Some(&i) = self.index.get(&id) {
                open_path(&self.row_path(&self.rows[i]));
            }
        }
        for id in to_close {
            self.progress_open.remove(&id);
        }
    }

    /// Tab 1: "Download status" — info + progress bar + (detail) segment bar &
    /// tabel koneksi (N. / Downloaded / Info), mirror versi Windows.
    fn pg_tab_status(ui: &mut egui::Ui, id: u64, r: &Row, st: &mut ProgressUi) {
        ui.add_space(4.0);
        ui.strong(&r.filename);
        // URL disembunyikan default — link panjang bikin layout jelek. Toggle.
        let link_label = if st.show_url { "▾ Hide link" } else { "▸ Show link" };
        if ui.small_button(link_label).clicked() {
            st.show_url = !st.show_url;
        }
        if st.show_url {
            ui.add(egui::Label::new(egui::RichText::new(&r.url).weak().small()));
        }
        ui.add_space(6.0);
        egui::Grid::new(format!("pg-grid-{id}"))
            .num_columns(2)
            .spacing([16.0, 7.0])
            .show(ui, |ui| {
                ui.label("Status:");
                ui.label(status_line_idm(r));
                ui.end_row();
                ui.label("File size:");
                ui.label(r.total.map(human_bytes).unwrap_or_else(|| "?".into()));
                ui.end_row();
                ui.label("Downloaded:");
                let pct = match r.total {
                    Some(t) if t > 0 => (r.downloaded as f64 / t as f64 * 100.0) as u32,
                    _ => 0,
                };
                ui.label(format!("{} ({pct}%)", human_bytes(r.downloaded)));
                ui.end_row();
                ui.label("Transfer rate:");
                ui.label(if r.speed_bps > 0 {
                    format!("{}/s", human_bytes(r.speed_bps))
                } else {
                    "—".into()
                });
                ui.end_row();
                ui.label("Time left:");
                ui.label(time_left(r));
                ui.end_row();
                ui.label("Resume capability:");
                ui.label(if r.total.is_some() { "Yes" } else { "No" });
                ui.end_row();
            });
        ui.add_space(8.0);
        let frac = match r.total {
            Some(t) if t > 0 => (r.downloaded as f32 / t as f32).clamp(0.0, 1.0),
            _ => 0.0,
        };
        ui.add(egui::ProgressBar::new(frac).show_percentage());

        if st.details {
            ui.add_space(10.0);
            ui.label("Start positions and download progress by connections:");
            ui.add_space(4.0);
            segment_bar(ui, &r.segments, r.total);
            ui.add_space(8.0);
            egui::ScrollArea::vertical()
                .max_height(150.0)
                .show(ui, |ui| {
                    egui::Grid::new(format!("seg-grid-{id}"))
                        .num_columns(3)
                        .striped(true)
                        .min_col_width(60.0)
                        .show(ui, |ui| {
                            ui.strong("N.");
                            ui.strong("Downloaded");
                            ui.strong("Info");
                            ui.end_row();
                            if r.segments.is_empty() {
                                ui.label("1");
                                ui.label(human_bytes(r.downloaded));
                                ui.label(conn_info(r.status, r.downloaded, r.total.unwrap_or(0)));
                                ui.end_row();
                            } else {
                                for (n, (start, end, dl)) in r.segments.iter().enumerate() {
                                    let len = end - start + 1;
                                    ui.label(format!("{}", n + 1));
                                    ui.label(human_bytes(*dl));
                                    ui.label(conn_info(r.status, *dl, len));
                                    ui.end_row();
                                }
                            }
                        });
                });
        }
    }

    /// Tab 2: "Speed Limiter" — batas kecepatan per-unduhan (live via engine).
    /// Mengembalikan `true` bila setelan berubah (pemanggil terapkan ke engine).
    fn pg_tab_limiter(ui: &mut egui::Ui, st: &mut ProgressUi) -> bool {
        let mut changed = false;
        ui.add_space(6.0);
        ui.label("Use the speed limiter to reduce bandwidth usage for this download.");
        ui.add_space(10.0);
        changed |= ui.checkbox(&mut st.limit_on, "Use Speed Limiter").changed();
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label("Maximum download speed:");
            let resp = ui.add_enabled(
                st.limit_on,
                egui::DragValue::new(&mut st.limit_kbps)
                    .speed(10.0)
                    .range(0..=10_000_000u64),
            );
            changed |= resp.changed();
            ui.label("KB/s");
        });
        ui.add_space(18.0);
        changed
    }

    /// Tab 3: "Options on completion" — dieksekusi saat unduhan selesai
    /// (dialog complete / exit ADM / aksi daya). Lihat `apply_completion`.
    fn pg_tab_options(ui: &mut egui::Ui, id: u64, st: &mut ProgressUi) {
        let c = &mut st.completion;
        ui.add_space(6.0);
        ui.checkbox(&mut c.show_complete, "Show download complete dialog");
        ui.checkbox(&mut c.exit_done, "Exit ADM when done");
        ui.checkbox(&mut c.poweroff_done, "Run action when done");
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label("When done:");
            egui::ComboBox::from_id_salt(("when-done", id))
                .selected_text(c.when_done.label())
                .show_ui(ui, |ui| {
                    for w in [WhenDone::ShutDown, WhenDone::Hibernate, WhenDone::Sleep, WhenDone::Exit] {
                        ui.selectable_value(&mut c.when_done, w, w.label());
                    }
                });
        });
    }

    /// Dialog modeless "Download complete" (gaya IDM §9.14): Open / Open folder /
    /// Close. Dipicu opsi "Show download complete dialog" saat unduhan selesai.
    fn complete_dialogs(&mut self, ctx: &egui::Context) {
        if self.complete_open.is_empty() {
            return;
        }
        let ids: Vec<u64> = self.complete_open.iter().copied().collect();
        let mut to_close = Vec::new();
        let mut to_open = Vec::new();
        let mut to_folder = Vec::new();
        for id in ids {
            let Some(&i) = self.index.get(&id) else {
                to_close.push(id);
                continue;
            };
            let path = self.row_path(&self.rows[i]);
            let r = &self.rows[i];
            // Jendela OS tersendiri (viewport), terlepas dari jendela utama.
            let vid = egui::ViewportId::from_hash_of(("adm-complete", id));
            let builder = egui::ViewportBuilder::default()
                .with_title("Download complete")
                .with_inner_size([440.0, 200.0])
                .with_resizable(false)
                .with_minimize_button(false)
                .with_maximize_button(false);
            let mut closed = false;
            ctx.show_viewport_immediate(vid, builder, |ctx, _class| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.add_space(4.0);
                    ui.strong("Download complete");
                    ui.add_space(8.0);
                    ui.label(&r.filename);
                    let sz = r.total.map(human_bytes).unwrap_or_else(|| human_bytes(r.downloaded));
                    ui.weak(format!("Downloaded {sz}"));
                    ui.add_space(6.0);
                    ui.weak(path.display().to_string());
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if ui.button("Open").clicked() {
                            to_open.push(id);
                        }
                        if ui.button("Open folder").clicked() {
                            to_folder.push(id);
                        }
                        if ui.button("Close").clicked() {
                            to_close.push(id);
                        }
                    });
                });
                if ctx.input(|i| i.viewport().close_requested()) {
                    closed = true;
                }
            });
            if closed {
                to_close.push(id);
            }
        }
        for id in to_open {
            if let Some(&i) = self.index.get(&id) {
                open_path(&self.row_path(&self.rows[i]));
            }
            self.complete_open.remove(&id);
        }
        for id in to_folder {
            if let Some(&i) = self.index.get(&id) {
                if let Some(p) = self.row_path(&self.rows[i]).parent() {
                    open_path(p);
                }
            }
            self.complete_open.remove(&id);
        }
        for id in to_close {
            self.complete_open.remove(&id);
        }
    }
}

/// Pemilih jam:menit (dua DragValue) untuk dialog Scheduler.
fn time_picker(ui: &mut egui::Ui, id: &str, t: &mut (u8, u8)) {
    ui.horizontal(|ui| {
        ui.add(egui::DragValue::new(&mut t.0).range(0..=23).custom_formatter(|n, _| format!("{:02}", n as u32)));
        ui.label(":");
        ui.add(egui::DragValue::new(&mut t.1).range(0..=59).custom_formatter(|n, _| format!("{:02}", n as u32)));
        let _ = id;
    });
}

/// Tombol toolbar gaya IDM: ikon besar di atas, teks di bawah (vertikal).
/// Lebar menyesuaikan teks; `enabled=false` → ikon & teks dipudarkan.
fn tbtn(ui: &mut egui::Ui, src: egui::ImageSource<'static>, label: &str, enabled: bool) -> bool {
    const ICON: f32 = 26.0;
    const FONT: f32 = 13.0;
    const PAD_X: f32 = 10.0;
    const H: f32 = 62.0;

    let font = egui::FontId::proportional(FONT);
    let text_w = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font.clone(), egui::Color32::PLACEHOLDER)
        .size()
        .x;
    let w = (text_w + PAD_X * 2.0).max(ICON + 12.0);

    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, H), egui::Sense::click());
    let painter = ui.painter();
    let visuals = ui.visuals();

    // Latar hover/aktif (hanya bila enabled).
    if enabled {
        if resp.is_pointer_button_down_on() {
            painter.rect_filled(rect, 4.0, visuals.widgets.active.bg_fill);
        } else if resp.hovered() {
            painter.rect_filled(rect, 4.0, visuals.widgets.hovered.weak_bg_fill);
        }
    }

    // Ikon di atas, di tengah horizontal.
    let icon_rect = egui::Rect::from_min_size(
        egui::pos2(rect.center().x - ICON / 2.0, rect.top() + 6.0),
        egui::vec2(ICON, ICON),
    );
    // Ikon SVG berstroke putih → tint mengatur warna akhir. Gelap di tema
    // terang; terang di tema gelap. Dipudarkan saat disabled.
    let base = if ui.visuals().dark_mode {
        egui::Color32::from_rgb(0xab, 0xb2, 0xbf)
    } else {
        egui::Color32::from_gray(43)
    };
    let tint = if enabled { base } else { base.gamma_multiply(0.38) };
    egui::Image::new(src).tint(tint).paint_at(ui, icon_rect);

    // Teks di bawah ikon.
    let text_color = if enabled {
        visuals.text_color()
    } else {
        visuals.weak_text_color()
    };
    ui.painter().text(
        egui::pos2(rect.center().x, icon_rect.bottom() + 4.0),
        egui::Align2::CENTER_TOP,
        label,
        font,
        text_color,
    );

    enabled && resp.clicked()
}

/// Varian tombol toolbar dengan panah dropdown ▾ di sisi kanan (gaya tombol
/// Start/Stop Queue versi Windows yang ber-flag BTNS_DROPDOWN). Mengembalikan
/// `(tombol_utama_diklik, response_panah)`; pemanggil membuka popup daftar
/// antrian via `egui::Popup::menu(&response_panah)`.
fn tbtn_dd(
    ui: &mut egui::Ui,
    src: egui::ImageSource<'static>,
    label: &str,
    enabled: bool,
) -> (bool, egui::Response) {
    const ICON: f32 = 26.0;
    const FONT: f32 = 13.0;
    const PAD_X: f32 = 10.0;
    const H: f32 = 62.0;
    const ARROW_W: f32 = 18.0;

    let font = egui::FontId::proportional(FONT);
    let text_w = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font.clone(), egui::Color32::PLACEHOLDER)
        .size()
        .x;
    let main_w = (text_w + PAD_X * 2.0).max(ICON + 12.0);

    let (rect, _) = ui.allocate_exact_size(egui::vec2(main_w + ARROW_W, H), egui::Sense::hover());
    let main_rect = egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x - ARROW_W, rect.max.y));
    let arrow_rect = egui::Rect::from_min_max(egui::pos2(rect.max.x - ARROW_W, rect.min.y), rect.max);

    let main_resp = ui.interact(main_rect, ui.id().with((label, "qmain")), egui::Sense::click());
    let arrow_resp = ui.interact(arrow_rect, ui.id().with((label, "qarrow")), egui::Sense::click());

    let painter = ui.painter();
    let visuals = ui.visuals();

    if enabled {
        for (r, resp) in [(main_rect, &main_resp), (arrow_rect, &arrow_resp)] {
            if resp.is_pointer_button_down_on() {
                painter.rect_filled(r, 4.0, visuals.widgets.active.bg_fill);
            } else if resp.hovered() {
                painter.rect_filled(r, 4.0, visuals.widgets.hovered.weak_bg_fill);
            }
        }
    }

    let icon_rect = egui::Rect::from_min_size(
        egui::pos2(main_rect.center().x - ICON / 2.0, main_rect.top() + 6.0),
        egui::vec2(ICON, ICON),
    );
    let base = if ui.visuals().dark_mode {
        egui::Color32::from_rgb(0xab, 0xb2, 0xbf)
    } else {
        egui::Color32::from_gray(43)
    };
    let tint = if enabled { base } else { base.gamma_multiply(0.38) };
    egui::Image::new(src).tint(tint).paint_at(ui, icon_rect);

    let text_color = if enabled {
        visuals.text_color()
    } else {
        visuals.weak_text_color()
    };
    ui.painter().text(
        egui::pos2(main_rect.center().x, icon_rect.bottom() + 4.0),
        egui::Align2::CENTER_TOP,
        label,
        font,
        text_color,
    );
    // Panah dropdown ▾ digambar manual (font statik tak punya glyph U+25BE).
    let c = arrow_rect.center();
    let tri = vec![
        egui::pos2(c.x - 4.0, c.y - 3.0),
        egui::pos2(c.x + 4.0, c.y - 3.0),
        egui::pos2(c.x, c.y + 3.0),
    ];
    ui.painter()
        .add(egui::Shape::convex_polygon(tri, text_color, egui::Stroke::NONE));

    (enabled && main_resp.clicked(), arrow_resp)
}

/// Popup dropdown tombol queue: ringkas jumlah antrian + status scheduler, plus
/// pintasan membuka dialog Scheduler. Mengembalikan true bila Scheduler diklik.
fn queue_popup(arrow: &egui::Response, queued: usize, sched: &scheduler::Schedule) -> bool {
    let mut open_sched = false;
    egui::Popup::menu(arrow).show(|ui| {
        ui.set_min_width(220.0);
        ui.label(format!("Antrian: {queued} menunggu"));
        ui.separator();
        if sched.enabled {
            ui.label(format!(
                "Jadwal: {:02}:{:02}–{:02}:{:02}",
                sched.start.0, sched.start.1, sched.stop.0, sched.stop.1
            ));
        } else {
            ui.weak("Jadwal: nonaktif");
        }
        if ui.button("Scheduler…").clicked() {
            open_sched = true;
            ui.close();
        }
    });
    open_sched
}

// Ikon Lucide (sama dengan versi Windows); di-embed saat kompilasi.
fn icon_add_url() -> egui::ImageSource<'static> {
    egui::include_image!("../assets/icons/add-url.svg")
}
fn icon_resume() -> egui::ImageSource<'static> {
    egui::include_image!("../assets/icons/resume.svg")
}
fn icon_stop() -> egui::ImageSource<'static> {
    egui::include_image!("../assets/icons/stop.svg")
}
fn icon_stop_all() -> egui::ImageSource<'static> {
    egui::include_image!("../assets/icons/stop-all.svg")
}
fn icon_delete() -> egui::ImageSource<'static> {
    egui::include_image!("../assets/icons/delete.svg")
}
fn icon_delete_completed() -> egui::ImageSource<'static> {
    egui::include_image!("../assets/icons/delete-completed.svg")
}
fn icon_options() -> egui::ImageSource<'static> {
    egui::include_image!("../assets/icons/options.svg")
}
fn icon_scheduler() -> egui::ImageSource<'static> {
    egui::include_image!("../assets/icons/scheduler.svg")
}
fn icon_refresh_link() -> egui::ImageSource<'static> {
    egui::include_image!("../assets/icons/refresh-link.svg")
}
fn icon_start_queue() -> egui::ImageSource<'static> {
    egui::include_image!("../assets/icons/start-queue.svg")
}
fn icon_stop_queue() -> egui::ImageSource<'static> {
    egui::include_image!("../assets/icons/stop-queue.svg")
}
fn icon_updates() -> egui::ImageSource<'static> {
    egui::include_image!("../assets/icons/updates.svg")
}

fn status_text(r: &Row) -> String {
    match r.status {
        Status::Active => match (r.downloaded, r.total) {
            (d, Some(t)) if t > 0 => format!("{:.0}%", (d as f64 / t as f64) * 100.0),
            _ => "Downloading".into(),
        },
        s => s.label().to_string(),
    }
}

/// Teks status gaya IDM untuk dialog progress ("Receiving data..." dst).
fn status_line_idm(r: &Row) -> String {
    match r.status {
        Status::Active => "Receiving data...".into(),
        Status::Queued => "Queued".into(),
        Status::Completed => "Complete".into(),
        Status::Paused => "Stopped".into(),
        Status::Failed => r.error.clone().unwrap_or_else(|| "Error".into()),
    }
}

/// Teks kolom "Info" per-koneksi (tabel dialog progress, gaya IDM).
fn conn_info(status: Status, dl: u64, len: u64) -> String {
    if len > 0 && dl >= len {
        return "Finished".into();
    }
    match status {
        Status::Active => "Receiving data...".into(),
        Status::Paused => "Stopped".into(),
        Status::Failed => "Error".into(),
        Status::Queued => "Waiting".into(),
        Status::Completed => "Finished".into(),
    }
}

/// Segment bar (gaya IDM): porsi terunduh tiap koneksi (biru) di atas rentang
/// byte-nya pada keseluruhan berkas; sisa rentang abu-abu, dibatasi garis pemisah.
fn segment_bar(ui: &mut egui::Ui, segments: &[(u64, u64, u64)], total: Option<u64>) {
    let width = ui.available_width();
    let (rect, _resp) = ui.allocate_exact_size(egui::vec2(width, 26.0), egui::Sense::hover());
    let dark = ui.visuals().dark_mode;
    let bg = if dark { egui::Color32::from_gray(38) } else { egui::Color32::from_gray(245) };
    let pending = if dark { egui::Color32::from_gray(70) } else { egui::Color32::from_gray(210) };
    let filled = if dark {
        egui::Color32::from_rgb(86, 156, 214)
    } else {
        egui::Color32::from_rgb(51, 122, 204)
    };
    let sep = if dark { egui::Color32::from_gray(96) } else { egui::Color32::from_gray(150) };
    let painter = ui.painter();
    painter.rect_filled(rect, 3.0, bg);
    if let Some(total) = total.filter(|t| *t > 0) {
        let total = total as f32;
        let x_of = |b: f32| rect.left() + rect.width() * (b / total);
        for (start, end, dl) in segments {
            let len = (end - start + 1) as f32;
            let x0 = x_of(*start as f32);
            let x1 = x_of((*end + 1) as f32);
            // Rentang koneksi (porsi belum terunduh) — abu-abu.
            let span = egui::Rect::from_min_max(
                egui::pos2(x0, rect.top()),
                egui::pos2(x1, rect.bottom()),
            );
            painter.rect_filled(span, 0.0, pending);
            // Porsi terunduh — biru.
            let fw = (x1 - x0) * (*dl as f32 / len).clamp(0.0, 1.0);
            let fill = egui::Rect::from_min_max(
                egui::pos2(x0, rect.top()),
                egui::pos2(x0 + fw, rect.bottom()),
            );
            painter.rect_filled(fill, 0.0, filled);
            // Garis pemisah antar-segmen.
            painter.vline(x1, rect.y_range(), egui::Stroke::new(1.0, sep));
        }
    }
}

fn time_left(r: &Row) -> String {
    if r.status != Status::Active || r.speed_bps == 0 {
        return "—".into();
    }
    match r.total {
        Some(t) if t > r.downloaded => {
            let secs = (t - r.downloaded) / r.speed_bps.max(1);
            fmt_duration(secs)
        }
        _ => "—".into(),
    }
}

fn fmt_duration(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

/// Setel font + ukuran teks/kontrol + padding dialog. Default egui terlalu
/// kecil & tipis untuk gaya IDM; kita perbesar dan pakai font sistem yg tegas.
fn configure_style(ctx: &egui::Context) {
    use egui::{FontFamily, FontId, TextStyle};
    install_fonts(ctx);

    let mut style = (*ctx.style()).clone();
    style.text_styles = [
        (TextStyle::Small, FontId::new(12.5, FontFamily::Proportional)),
        (TextStyle::Body, FontId::new(15.0, FontFamily::Proportional)),
        (TextStyle::Button, FontId::new(15.0, FontFamily::Proportional)),
        (TextStyle::Heading, FontId::new(21.0, FontFamily::Proportional)),
        (TextStyle::Monospace, FontId::new(13.5, FontFamily::Monospace)),
    ]
    .into();
    // Kontrol lebih besar & lega.
    style.spacing.button_padding = egui::vec2(10.0, 6.0);
    style.spacing.interact_size.y = 28.0;
    style.spacing.item_spacing = egui::vec2(8.0, 7.0);
    // Padding isi semua jendela dialog (Window) agar tak mepet tepi.
    style.spacing.window_margin = egui::Margin::same(16);
    ctx.set_style(style);
}

/// Terapkan tema terang atau gelap (One Dark) ke konteks.
fn apply_theme(ctx: &egui::Context, dark: bool) {
    if dark {
        ctx.set_visuals(one_dark_visuals());
    } else {
        ctx.set_visuals(egui::Visuals::light());
    }
}

/// Palet gelap bergaya **Atom One Dark** (populer). Dibangun dari `Visuals::dark`
/// lalu menimpa warna kunci agar konsisten dengan One Dark.
fn one_dark_visuals() -> egui::Visuals {
    use egui::{Color32, Stroke};
    let base = Color32::from_rgb(40, 44, 52); // #282c34
    let panel = Color32::from_rgb(33, 37, 43); // #21252b
    let widget = Color32::from_rgb(44, 49, 58); // #2c313a
    let hovered = Color32::from_rgb(58, 63, 75); // #3a3f4b
    let active = Color32::from_rgb(62, 68, 81); // #3e4451
    let fg = Color32::from_rgb(171, 178, 191); // #abb2bf
    let fg_bright = Color32::from_rgb(215, 218, 224);
    let accent = Color32::from_rgb(97, 175, 239); // #61afef

    let mut v = egui::Visuals::dark();
    v.dark_mode = true;
    v.override_text_color = Some(fg);
    v.panel_fill = panel;
    v.window_fill = base;
    v.extreme_bg_color = Color32::from_rgb(27, 30, 36); // bg text edit
    v.faint_bg_color = Color32::from_rgb(36, 40, 48);
    v.window_stroke = Stroke::new(1.0, Color32::from_rgb(24, 26, 31));
    v.selection.bg_fill = active;
    v.selection.stroke = Stroke::new(1.0, accent);
    v.hyperlink_color = accent;

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = panel;
    w.noninteractive.weak_bg_fill = panel;
    w.noninteractive.fg_stroke = Stroke::new(1.0, fg);
    w.inactive.bg_fill = widget;
    w.inactive.weak_bg_fill = widget;
    w.inactive.fg_stroke = Stroke::new(1.0, fg);
    w.hovered.bg_fill = hovered;
    w.hovered.weak_bg_fill = hovered;
    w.hovered.fg_stroke = Stroke::new(1.5, fg_bright);
    w.active.bg_fill = active;
    w.active.weak_bg_fill = active;
    w.active.fg_stroke = Stroke::new(1.5, Color32::WHITE);
    v
}

/// Pasang font sistem yang lebih tebal (Noto Sans / DejaVu) sebagai font utama
/// proporsional. Bila tak ada satupun ditemukan, biarkan default egui.
fn install_fonts(ctx: &egui::Context) {
    // HANYA font statik — ab_glyph (backend egui) bisa hang pada variable font
    // seperti `NotoSans[wght].ttf`. Liberation Sans / Cantarell tegas & umum di
    // Fedora; DejaVu sebagai cadangan.
    const CANDIDATES: &[&str] = &[
        "/usr/share/fonts/liberation-sans-fonts/LiberationSans-Regular.ttf",
        "/usr/share/fonts/abattis-cantarell-fonts/Cantarell-Regular.otf",
        "/usr/share/fonts/dejavu-sans-fonts/DejaVuSans.ttf",
        "/usr/share/fonts/dejavu/DejaVuSans.ttf",
    ];
    let Some(bytes) = CANDIDATES.iter().find_map(|p| std::fs::read(p).ok()) else {
        return;
    };
    let mut fonts = egui::FontDefinitions::default();
    fonts
        .font_data
        .insert("system-sans".to_owned(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .insert(0, "system-sans".to_owned());
    ctx.set_fonts(fonts);
}

/// Buka berkas/folder dengan handler default desktop (`xdg-open`), terlepas.
fn open_path(path: &std::path::Path) {
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
}

/// Buka URL di browser default desktop (`xdg-open`), terlepas.
fn open_url(url: &str) {
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

/// Aksi daya "Options on completion": jalankan via systemd lalu keluar.
/// `Exit` hanya menutup app. Best-effort (systemctl tunduk pada polkit/logind).
fn perform_power(w: WhenDone) {
    let action = match w {
        WhenDone::ShutDown => Some("poweroff"),
        WhenDone::Hibernate => Some("hibernate"),
        WhenDone::Sleep => Some("suspend"),
        WhenDone::Exit => None,
    };
    if let Some(a) = action {
        let _ = std::process::Command::new("systemctl").arg(a).spawn();
    }
    std::process::exit(0);
}

fn filename_of(p: &std::path::Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "(no name)".into())
}

/// Tebak nama berkas dari URL (segmen path terakhir, tanpa query/fragment).
fn guess_filename(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or("");
    path.rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("download.bin")
        .to_string()
}

fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

/// Format `SystemTime` → "YYYY-MM-DD HH:MM:SS" (UTC), tanpa dependensi eksternal.
/// Konversi hari→tanggal pakai algoritma civil-from-days (Howard Hinnant).
fn fmt_time(t: SystemTime) -> String {
    let secs = match t.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => d.as_secs(),
        Err(_) => return "—".into(),
    };
    let days = (secs / 86_400) as i64;
    let tod = secs % 86_400;
    let (hh, mm, ss) = (tod / 3600, (tod % 3600) / 60, tod % 60);

    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}:{ss:02}")
}
