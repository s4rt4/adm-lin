//! ADM (Linux/egui) — UI bergaya IDM (clone). Layout meniru versi Windows
//! (`adm-app/gui.rs`): menu bar, toolbar tombol, sidebar pohon kategori,
//! tabel unduhan multi-kolom, status bar. Engine `adm-core` jalan in-process.

mod autostart;
mod category;
mod engine;
mod ipc;
mod settings;
mod store;
mod tray;

use category::Category;
use eframe::egui;
use egui_extras::{Column, TableBuilder};
use engine::{EngineEvent, EngineHandle};
use ipc::IpcCommand;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::SystemTime;

fn main() -> eframe::Result<()> {
    // Single-instance: bila sudah ada ADM yang berjalan, minta ia muncul lalu keluar.
    if ipc::try_activate_existing() {
        return Ok(());
    }

    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1180.0, 720.0])
        .with_min_inner_size([860.0, 440.0])
        .with_title("Alpha Download Manager");
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
}

impl Row {
    fn matches(&self, f: Filter) -> bool {
        match f {
            Filter::All => true,
            Filter::Category(c) => self.category == c,
            Filter::Unfinished => self.status != Status::Completed,
            Filter::Finished => self.status == Status::Completed,
            Filter::Queues => self.status == Status::Queued,
            Filter::Grabber => false,
        }
    }
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
    // Dialog "Add URL".
    show_add: bool,
    add_url: String,
    /// Metadata titipan browser (referrer/UA/cookie) untuk add via IPC; dipakai
    /// saat dialog Add dikonfirmasi agar header tak hilang.
    pending_add: Option<adm_ipc::DownloadAddParams>,
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
            pending_add: None,
            show_options: false,
            opt_dir,
            opt_queue_max: cfg.queue_max,
            opt_limit_kbps: cfg.limit_kbps,
            opt_autostart: false,
            show_about: false,
            refresh_target: None,
            refresh_url: String::new(),
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

    /// Tombol tutup jendela: jangan keluar — beradaptasi dgn lingkungan.
    /// Ada tray → sembunyikan jendela (tetap jalan di tray). Tanpa tray (GNOME
    /// polos) → minimize ke dock. Keluar betulan lewat menu Exit / tray Exit.
    fn handle_close(&self, ctx: &egui::Context) {
        if !ctx.input(|i| i.viewport().close_requested()) {
            return;
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        if self.tray_active.load(Ordering::SeqCst) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        } else {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }
    }

    /// Proses perintah dari jalur IPC (browser bridge / instance kedua).
    fn drain_ipc(&mut self, ctx: &egui::Context) {
        while let Ok(cmd) = self.ipc_rx.try_recv() {
            match cmd {
                IpcCommand::Add(params) => {
                    self.add_url = params.url.clone();
                    self.pending_add = Some(params);
                    self.show_add = true;
                    Self::bring_to_front(ctx);
                }
                IpcCommand::Activate => Self::bring_to_front(ctx),
            }
        }
    }

    /// Munculkan & fokuskan jendela (dipakai single-instance & klik bridge).
    fn bring_to_front(ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
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
                    ..
                } => {
                    if let Some(&i) = self.index.get(&id) {
                        let r = &mut self.rows[i];
                        r.downloaded = downloaded;
                        r.total = total;
                        r.speed_bps = speed_bps;
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
    }

    fn upsert(&mut self, id: u64, url: String, output: PathBuf, status: Status) {
        let filename = filename_of(&output);
        let category = Category::from_filename(&filename);
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
        // URL hasil edit di dialog tetap diutamakan.
        let mut params = self.pending_add.take().unwrap_or_default();
        params.url = url;
        if later {
            self.engine.enqueue(params);
        } else {
            self.engine.add(params);
        }
        self.add_url.clear();
        self.show_add = false;
    }

    fn resume_selected(&self) {
        if let Some(r) = self.selected_row() {
            self.engine
                .resume(r.id, r.url.clone(), r.filename.clone(), false);
        }
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
}

impl eframe::App for AdmApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.drain_events();
        self.drain_ipc(ui.ctx());
        self.handle_close(ui.ctx());

        self.menu_bar(ui);
        self.toolbar(ui);
        self.status_bar(ui);
        self.sidebar(ui);
        self.table(ui);
        self.add_dialog(ui);
        self.options_dialog(ui);
        self.about_dialog(ui);
        self.refresh_dialog(ui);
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
                        self.show_add = true;
                        ui.close();
                    }
                    ui.add_enabled(false, egui::Button::new("Add batch download..."));
                    ui.add_enabled(false, egui::Button::new("Add batch download from clipboard"));
                    ui.add_enabled(false, egui::Button::new("Run site grabber..."));
                    ui.separator();
                    ui.add_enabled(false, egui::Button::new("Export..."));
                    ui.add_enabled(false, egui::Button::new("Import..."));
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
                    ui.add_enabled(false, egui::Button::new("Find...\tCtrl+F"));
                    ui.add_enabled(false, egui::Button::new("Find Next\tF3"));
                    ui.separator();
                    ui.add_enabled(false, egui::Button::new("Scheduler..."));
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

            ui.horizontal(|ui| {
                // Urutan persis versi Windows (adm-app/gui.rs::add_toolbar_buttons).
                if tbtn(ui, icon_add_url(), "Add URL", true) {
                    self.show_add = true;
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
                tbtn(ui, icon_scheduler(), "Scheduler", false);
                if tbtn(ui, icon_refresh_link(), "Refresh Link", has_sel) {
                    self.open_refresh();
                }
                ui.separator();
                if tbtn(ui, icon_start_queue(), "Start Queue", true) {
                    self.engine.start_queue();
                }
                if tbtn(ui, icon_stop_queue(), "Stop Queue", true) {
                    self.engine.stop_queue();
                }
                ui.separator();
                tbtn(ui, icon_updates(), "Updates", false);
            });
            ui.add_space(4.0);
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
                let count = |f: Filter| self.rows.iter().filter(|r| r.matches(f)).count();
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
            let filter = self.filter;
            let visible: Vec<usize> = (0..self.rows.len())
                .filter(|&i| self.rows[i].matches(filter))
                .collect();

            let mut clicked: Option<u64> = None;
            let mut action: Option<(u64, RowAction)> = None;
            let mut row_rects: Vec<egui::Rect> = Vec::new();
            TableBuilder::new(ui)
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
                .column(Column::remainder().at_least(80.0)) // Description
                .header(26.0, |mut h| {
                    for title in ["File Name", "Q", "Size", "Status", "Time left", "Transfer rate", "Last Try", "Description"] {
                        h.col(|ui| {
                            ui.strong(title);
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
                            row_rects.push(resp.rect);
                            if resp.clicked() {
                                clicked = Some(r.id);
                            }
                            // Double-click membuka berkas (gaya IDM).
                            if resp.double_clicked() {
                                action = Some((r.id, RowAction::Open));
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

            // Garis tipis pemisah antar-baris (gaya list IDM).
            let line_color = if ui.visuals().dark_mode {
                egui::Color32::from_gray(58)
            } else {
                egui::Color32::from_gray(222)
            };
            let painter = ui.painter();
            let stroke = egui::Stroke::new(1.0, line_color);
            for rect in &row_rects {
                painter.hline(rect.x_range(), rect.bottom(), stroke);
            }

            if let Some(id) = clicked {
                self.selected = Some(id);
            }
            if let Some((id, act)) = action {
                self.do_row_action(id, act);
            }
        });
    }

    fn add_dialog(&mut self, ui: &mut egui::Ui) {
        if !self.show_add {
            return;
        }
        let mut open = true;
        let mut action: Option<bool> = None; // Some(later)
        egui::Window::new("Add download")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ui.ctx(), |ui| {
                ui.set_min_width(420.0);
                ui.label("Address (URL):");
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.add_url)
                        .desired_width(f32::INFINITY)
                        .hint_text("https://…"),
                );
                resp.request_focus();
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Start Download").clicked() {
                        action = Some(false);
                    }
                    if ui.button("Download Later").clicked() {
                        action = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        action = Some(false);
                        self.add_url.clear();
                        self.pending_add = None;
                        self.show_add = false;
                    }
                });
            });
        if !open {
            self.show_add = false;
            self.pending_add = None;
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

fn filename_of(p: &std::path::Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "(no name)".into())
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
