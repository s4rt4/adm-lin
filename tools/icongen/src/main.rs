//! Generator ikon: logo.svg -> adm.ico (multi-ukuran). Dijalankan manual saat
//! aset logo berubah; hasilnya di-commit. Build normal tidak butuh crate ini.

use std::path::PathBuf;

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let svg_path = root.join("logo.svg");
    let out_path = root.join("crates/adm-app/assets/adm.ico");

    let svg = std::fs::read(&svg_path).expect("baca logo.svg");
    let tree = usvg::Tree::from_data(&svg, &usvg::Options::default()).expect("parse svg");
    let svg_size = tree.size();

    let sizes = [16u32, 20, 24, 32, 40, 48, 64, 128, 256];
    let mut icon_dir = ico::IconDir::new(ico::ResourceType::Icon);

    for &sz in &sizes {
        let mut pixmap = tiny_skia::Pixmap::new(sz, sz).expect("pixmap");
        let scale = sz as f32 / svg_size.width().max(svg_size.height());
        let transform = tiny_skia::Transform::from_scale(scale, scale);
        resvg::render(&tree, transform, &mut pixmap.as_mut());

        // PNG = straight alpha (tiny-skia meng-unpremultiply saat encode).
        let png = pixmap.encode_png().expect("encode png");
        let image = ico::IconImage::read_png(&png[..]).expect("read png");
        icon_dir
            .add_entry(ico::IconDirEntry::encode(&image).expect("encode entry"));
    }

    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent).expect("buat dir assets");
    }
    let file = std::fs::File::create(&out_path).expect("buat adm.ico");
    icon_dir.write(file).expect("tulis ico");
    println!("OK -> {} ({} ukuran)", out_path.display(), sizes.len());
}
