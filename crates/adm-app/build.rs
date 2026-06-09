use std::path::Path;

fn main() {
    // Embed manifest (visual styles comctl32 v6 + DPI) lewat linker MSVC,
    // tanpa butuh rc.exe. Memerlukan link.exe (default linker MSVC Rust).
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("app.manifest");
        println!("cargo:rerun-if-changed=app.manifest");
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}",
            manifest.display()
        );
    }

    // Tanam ikon (ID 1) ke resource exe → tampil di Explorer & taskbar.
    println!("cargo:rerun-if-changed=app.rc");
    println!("cargo:rerun-if-changed=assets/adm.ico");
    let _ = embed_resource::compile("app.rc", embed_resource::NONE);
}
