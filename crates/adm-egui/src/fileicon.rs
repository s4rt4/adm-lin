//! Ikon tipe-berkas dari tema ikon sistem (freedesktop), gaya IDM.
//!
//! Alur: nama berkas → MIME (`mime_guess`) → kandidat nama ikon freedesktop
//! (mis. `application/zip` → `application-zip`, fallback `package-x-generic`) →
//! cari path di tema ikon aktif (`freedesktop-icons`) → dekode jadi `ColorImage`
//! (SVG via resvg, raster via crate `image`).

use eframe::egui;
use std::path::{Path, PathBuf};

/// Tema ikon aktif (GNOME) via gsettings; fallback `Adwaita`.
pub fn detect_theme() -> String {
    std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "icon-theme"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().trim_matches('\'').trim_matches('"').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Adwaita".to_string())
}

/// Cari path berkas ikon untuk `filename` pada tema `theme` & ukuran `size`.
pub fn lookup(filename: &str, theme: &str, size: u16) -> Option<PathBuf> {
    for name in icon_names(filename) {
        if let Some(p) = freedesktop_icons::lookup(&name)
            .with_theme(theme)
            .with_size(size)
            .with_cache()
            .find()
        {
            return Some(p);
        }
    }
    None
}

/// Kandidat nama ikon freedesktop, dari spesifik ke generik.
fn icon_names(filename: &str) -> Vec<String> {
    let mime = mime_guess::from_path(filename).first_or_octet_stream();
    let top = mime.type_().as_str().to_string();
    let sub = mime.subtype().as_str().to_string();

    let mut v = vec![format!("{top}-{sub}")];
    match top.as_str() {
        "text" => v.push("text-x-generic".into()),
        "image" => v.push("image-x-generic".into()),
        "audio" => v.push("audio-x-generic".into()),
        "video" => v.push("video-x-generic".into()),
        "font" => v.push("font-x-generic".into()),
        "application" => {
            if is_archive(&sub) {
                v.push("package-x-generic".into());
            }
            if is_office(&sub) {
                v.push("x-office-document".into());
            }
            v.push("application-x-generic".into());
            v.push("application-x-executable".into());
        }
        _ => {}
    }
    v.push("text-x-generic".into()); // fallback paling akhir (umum ada di tema)
    v
}

fn is_archive(sub: &str) -> bool {
    ["zip", "tar", "7z", "rar", "gzip", "bzip", "compress", "xz", "zstd"]
        .iter()
        .any(|k| sub.contains(k))
}

fn is_office(sub: &str) -> bool {
    ["word", "excel", "powerpoint", "officedocument", "opendocument", "spreadsheet", "presentation"]
        .iter()
        .any(|k| sub.contains(k))
}

/// Dekode berkas ikon → `ColorImage` RGBA (kotak `size`×`size` untuk SVG).
pub fn load(path: &Path, size: u32) -> Option<egui::ColorImage> {
    let svg = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("svg"))
        .unwrap_or(false);
    if svg {
        render_svg(path, size)
    } else {
        render_raster(path)
    }
}

fn render_svg(path: &Path, size: u32) -> Option<egui::ColorImage> {
    use resvg::{tiny_skia, usvg};
    let data = std::fs::read(path).ok()?;
    let tree = usvg::Tree::from_data(&data, &usvg::Options::default()).ok()?;
    let sz = tree.size();
    let scale = (size as f32 / sz.width().max(sz.height())).max(0.01);
    let mut pixmap = tiny_skia::Pixmap::new(size, size)?;
    let ts = tiny_skia::Transform::from_scale(scale, scale);
    resvg::render(&tree, ts, &mut pixmap.as_mut());
    // tiny_skia = premultiplied; egui minta alpha lurus → unpremultiply.
    let mut rgba = pixmap.data().to_vec();
    for px in rgba.chunks_exact_mut(4) {
        let a = px[3] as u32;
        if a > 0 {
            px[0] = ((px[0] as u32 * 255) / a).min(255) as u8;
            px[1] = ((px[1] as u32 * 255) / a).min(255) as u8;
            px[2] = ((px[2] as u32 * 255) / a).min(255) as u8;
        }
    }
    Some(egui::ColorImage::from_rgba_unmultiplied(
        [size as usize, size as usize],
        &rgba,
    ))
}

fn render_raster(path: &Path) -> Option<egui::ColorImage> {
    let img = image::open(path).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    Some(egui::ColorImage::from_rgba_unmultiplied(
        [w as usize, h as usize],
        img.as_raw(),
    ))
}
