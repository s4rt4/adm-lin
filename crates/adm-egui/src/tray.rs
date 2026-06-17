//! System tray (StatusNotifierItem) via `ksni` (plan §3/§12, port dari tray
//! Windows). Menu: Show / Start-Stop queue / Exit. Klik-kiri memunculkan jendela.
//!
//! Catatan: GNOME polos tak menampilkan SNI tanpa ekstensi AppIndicator. Karena
//! itu kita kabarkan keberhasilan registrasi lewat `active` (AtomicBool) agar
//! pemanggil hanya mengaktifkan "close-to-tray" bila tray benar-benar muncul —
//! supaya jendela tak pernah tersembunyi tanpa cara memunculkannya kembali.

use crate::engine::EngineHandle;
use eframe::egui;
use ksni::blocking::TrayMethods; // API blocking → jalan di runtime internal ksni
use ksni::menu::StandardItem;
use ksni::{Icon, MenuItem, Tray};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

struct AdmTray {
    engine: EngineHandle,
    ctx: egui::Context,
}

impl AdmTray {
    fn show(&self) {
        self.ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        self.ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        self.ctx.request_repaint();
    }
}

impl Tray for AdmTray {
    fn title(&self) -> String {
        "Alpha Download Manager".into()
    }

    fn id(&self) -> String {
        "adm-egui".into()
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        tray_icon().into_iter().collect()
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.show();
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            StandardItem {
                label: "Show ADM".into(),
                activate: Box::new(|t: &mut Self| t.show()),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Start queue".into(),
                activate: Box::new(|t: &mut Self| t.engine.start_queue()),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Stop queue".into(),
                activate: Box::new(|t: &mut Self| t.engine.stop_queue()),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Exit".into(),
                activate: Box::new(|_| std::process::exit(0)),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// Daftarkan tray pada thread khusus (API blocking ksni mengelola runtime D-Bus
/// sendiri — terisolasi dari runtime tokio app). `active` di-set `true` HANYA
/// bila registrasi SNI berhasil, sehingga close-to-tray cuma aktif saat tray
/// betul-betul ada (di GNOME polos tetap `false` → fallback minimize ke dock).
pub fn launch(engine: EngineHandle, ctx: egui::Context, active: Arc<AtomicBool>) {
    let tray = AdmTray { engine, ctx };
    let _ = std::thread::Builder::new().name("adm-tray".into()).spawn(move || {
        match tray.spawn() {
            Ok(handle) => {
                active.store(true, Ordering::SeqCst);
                // Pertahankan handle hidup selama proses (drop = tray hilang).
                let _keep = handle;
                loop {
                    std::thread::park();
                }
            }
            Err(e) => eprintln!("[tray] StatusNotifierItem tak tersedia: {e}"),
        }
    });
}

/// Render `logo.svg` → ikon tray ARGB32 (alpha lurus, byte A,R,G,B).
fn tray_icon() -> Option<Icon> {
    use resvg::{tiny_skia, usvg};
    let svg = include_str!("../assets/logo.svg");
    let tree = usvg::Tree::from_str(svg, &usvg::Options::default()).ok()?;
    let size = 64u32;
    let mut pixmap = tiny_skia::Pixmap::new(size, size)?;
    let s = tree.size();
    let scale = (size as f32 / s.width()).min(size as f32 / s.height());
    resvg::render(&tree, tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());

    // tiny_skia = RGBA premultiplied → ARGB32 alpha-lurus (unpremultiply).
    let rgba = pixmap.take();
    let mut data = Vec::with_capacity(rgba.len());
    for px in rgba.chunks_exact(4) {
        let a = px[3];
        let (mut r, mut g, mut b) = (px[0], px[1], px[2]);
        if a > 0 && a < 255 {
            r = ((r as u16 * 255) / a as u16) as u8;
            g = ((g as u16 * 255) / a as u16) as u8;
            b = ((b as u16 * 255) / a as u16) as u8;
        }
        data.extend_from_slice(&[a, r, g, b]);
    }
    Some(Icon {
        width: size as i32,
        height: size as i32,
        data,
    })
}
