//! Generator aset ikon. Dijalankan manual saat aset berubah; hasil di-commit.
//! - logo.svg            -> crates/adm-app/assets/adm.ico (multi-ukuran)
//! - assets/icons/*.svg  -> crates/adm-app/assets/icons/toolbar24.bin
//!                          (11 ikon 24x24, premultiplied BGRA, urutan tetap)
//! Build aplikasi normal tidak butuh crate ini.

use std::path::{Path, PathBuf};

/// Urutan HARUS sama dengan indeks iBitmap toolbar di gui.rs.
const TOOLBAR_ORDER: [&str; 11] = [
    "add-url",
    "resume",
    "stop",
    "stop-all",
    "delete",
    "delete-completed",
    "options",
    "scheduler",
    "start-queue",
    "stop-queue",
    "updates",
];

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();

    gen_logo_ico(&root);
    gen_toolbar_blob(&root);
}

fn render(svg_path: &Path, size: u32) -> tiny_skia::Pixmap {
    let svg = std::fs::read(svg_path).unwrap_or_else(|_| panic!("baca {}", svg_path.display()));
    let tree = usvg::Tree::from_data(&svg, &usvg::Options::default()).expect("parse svg");
    let mut pm = tiny_skia::Pixmap::new(size, size).expect("pixmap");
    let dim = tree.size().width().max(tree.size().height());
    let scale = size as f32 / dim;
    resvg::render(&tree, tiny_skia::Transform::from_scale(scale, scale), &mut pm.as_mut());
    pm
}

fn gen_logo_ico(root: &Path) {
    let svg_path = root.join("logo.svg");
    let out = root.join("crates/adm-app/assets/adm.ico");
    let sizes = [16u32, 20, 24, 32, 40, 48, 64, 128, 256];
    let mut dir = ico::IconDir::new(ico::ResourceType::Icon);
    for &sz in &sizes {
        let pm = render(&svg_path, sz);
        let png = pm.encode_png().expect("png");
        let image = ico::IconImage::read_png(&png[..]).expect("read png");
        dir.add_entry(ico::IconDirEntry::encode(&image).expect("entry"));
    }
    std::fs::create_dir_all(out.parent().unwrap()).unwrap();
    dir.write(std::fs::File::create(&out).expect("buat ico")).expect("tulis ico");
    println!("OK -> {} ({} ukuran)", out.display(), sizes.len());
}

fn gen_toolbar_blob(root: &Path) {
    const SZ: u32 = 24;
    let icons_dir = root.join("crates/adm-app/assets/icons");
    let out = icons_dir.join("toolbar24.bin");
    let mut blob: Vec<u8> = Vec::with_capacity(TOOLBAR_ORDER.len() * (SZ * SZ * 4) as usize);

    for name in TOOLBAR_ORDER {
        let svg = icons_dir.join(format!("{name}.svg"));
        let pm = render(&svg, SZ);
        // tiny-skia: premultiplied RGBA. ImageList ILC_COLOR32 ingin BGRA premultiplied.
        for px in pm.data().chunks_exact(4) {
            blob.push(px[2]); // B
            blob.push(px[1]); // G
            blob.push(px[0]); // R
            blob.push(px[3]); // A
        }
    }

    std::fs::write(&out, &blob).expect("tulis toolbar24.bin");
    println!("OK -> {} ({} ikon, {}x{} BGRA)", out.display(), TOOLBAR_ORDER.len(), SZ, SZ);
}
