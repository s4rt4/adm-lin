//! adm-bridge — native messaging host (plan §11.2), port Linux.
//!
//! Mode:
//!   (default)            : loop stdio native-messaging (dipanggil browser).
//!   ping                 : uji koneksi ke adm (Unix socket).
//!   add <url>            : kirim download.add (uji).
//!   register <chrome-id> [firefox-id] : tulis manifest host ke direktori
//!                          NativeMessagingHosts per-browser (XDG).
//!   unregister           : hapus manifest host.
//!
//! Protokol stdio: 4-byte length LE + JSON UTF-8 (Chrome/Edge/Firefox).
//! Pesan dari extension: {"method":"download.add","url":..,"filename":..}.
//!
//! Jalur ke app: **Unix domain socket** `adm_ipc::unix_socket_path()`
//! (pengganti Named Pipe Windows).

use adm_ipc::{method, DownloadAddParams, Request, Response};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::BufReader;
use tokio::net::UnixStream;

const HOST_NAME: &str = "com.adm.bridge";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("ping") => cli_ping(),
        Some("add") => cli_add(&args),
        Some("register") => register(&args),
        Some("unregister") => unregister(),
        // Browser meluncurkan host dengan arg path-manifest/origin → mode stdio.
        _ => run_host(),
    }
}

// ---------------- Native messaging stdio host ----------------

fn run_host() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let stdin = std::io::stdin();
    let mut lock = stdin.lock();
    loop {
        let mut len_buf = [0u8; 4];
        if lock.read_exact(&mut len_buf).is_err() {
            break; // EOF → browser menutup
        }
        let len = u32::from_le_bytes(len_buf) as usize;
        if len == 0 || len > 4 * 1024 * 1024 {
            break;
        }
        let mut buf = vec![0u8; len];
        if lock.read_exact(&mut buf).is_err() {
            break;
        }
        let msg: serde_json::Value = serde_json::from_slice(&buf).unwrap_or_default();
        let resp = rt.block_on(handle(msg));
        if write_message(&resp).is_err() {
            break;
        }
    }
}

async fn handle(msg: serde_json::Value) -> serde_json::Value {
    let url = msg.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if url.is_empty() {
        return serde_json::json!({ "ok": false, "error": "url kosong" });
    }
    let params = DownloadAddParams {
        url,
        filename: msg.get("filename").and_then(|v| v.as_str()).map(String::from),
        referrer: msg.get("referrer").and_then(|v| v.as_str()).map(String::from),
        user_agent: msg.get("userAgent").and_then(|v| v.as_str()).map(String::from),
        cookies: msg.get("cookies").and_then(|v| v.as_str()).map(String::from),
        ..Default::default()
    };

    if !ensure_app().await {
        return serde_json::json!({ "ok": false, "error": "adm tak bisa dijalankan" });
    }

    match request(method::DOWNLOAD_ADD, Some(serde_json::to_value(&params).unwrap())).await {
        Ok(resp) => serde_json::json!({ "ok": resp.error.is_none(), "result": resp.result }),
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
    }
}

fn write_message(value: &serde_json::Value) -> std::io::Result<()> {
    let body = serde_json::to_vec(value)?;
    let mut out = std::io::stdout().lock();
    out.write_all(&(body.len() as u32).to_le_bytes())?;
    out.write_all(&body)?;
    out.flush()
}

/// Pastikan app hidup; jika tidak, spawn `adm-egui` terlepas lalu tunggu PING.
///
/// Koordinasi via **spawn-lock (flock)**: browser menjalankan satu bridge per
/// pesan native-messaging, jadi banyak bridge bisa lahir bersamaan. Hanya bridge
/// yang memegang kunci yang men-spawn app; sisanya cukup menunggu app naik.
/// (App sendiri sudah single-instance, jadi spawn ganda tak fatal — ini sekadar
/// menghindari kawanan proses app berumur pendek.)
async fn ensure_app() -> bool {
    if request(method::PING, None).await.is_ok() {
        return true;
    }
    match acquire_spawn_lock() {
        // Kita yang men-spawn. Tahan kunci selama menunggu app naik agar bridge
        // lain tak ikut spawn (kunci lepas saat `_lock` di-drop di akhir arm).
        Some(_lock) => {
            // Cek ulang: app mungkin sudah naik saat kita menunggu kunci.
            if request(method::PING, None).await.is_ok() {
                return true;
            }
            if let Some(app) = find_app() {
                use std::process::Stdio;
                // Lepaskan stdio agar pipe native-messaging tak diwariskan ke app.
                let _ = std::process::Command::new(app)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn();
            }
            wait_for_app().await
        }
        // Bridge lain sedang men-spawn — cukup tunggu app naik.
        None => wait_for_app().await,
    }
}

/// Tunggu app membalas PING (hingga ~5 dtk). `true` bila app naik.
async fn wait_for_app() -> bool {
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if request(method::PING, None).await.is_ok() {
            return true;
        }
    }
    false
}

/// Ambil spawn-lock (flock non-blok). `Some` = kita berhak men-spawn; `None` =
/// bridge lain sedang men-spawn. Kunci lepas otomatis saat `File` di-drop.
fn acquire_spawn_lock() -> Option<std::fs::File> {
    use std::os::unix::io::AsRawFd;
    let path = adm_ipc::unix_spawn_lock_path();
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .ok()?;
    // SAFETY: fd valid selama `file` hidup; flock tak menyentuh memori Rust.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        Some(file)
    } else {
        None
    }
}

/// Cari binary `adm-egui`: di samping bridge (instalasi), lalu andalkan PATH.
fn find_app() -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("adm-egui");
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    Some(PathBuf::from("adm-egui")) // resolusi via PATH
}

// ---------------- Unix socket client ----------------

async fn request(method: &str, params: Option<serde_json::Value>) -> std::io::Result<Response> {
    let client = UnixStream::connect(adm_ipc::unix_socket_path()).await?;
    let mut reader = BufReader::new(client);
    let req = Request::new(1, method, params);
    adm_ipc::write_message(reader.get_mut(), &req).await?;
    match adm_ipc::read_message(&mut reader).await? {
        Some(bytes) => Ok(serde_json::from_slice(&bytes)?),
        None => Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "socket ditutup sebelum balasan",
        )),
    }
}

// ---------------- CLI helpers ----------------

fn cli_ping() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    match rt.block_on(request(method::PING, None)) {
        Ok(resp) => println!("[bridge] ping OK: {}", serde_json::to_string(&resp).unwrap()),
        Err(e) => {
            eprintln!("[bridge] ping GAGAL: {e}");
            std::process::exit(1);
        }
    }
}

fn cli_add(args: &[String]) {
    let Some(url) = args.get(1) else {
        eprintln!("usage: adm-bridge add <url>");
        std::process::exit(2);
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let params = serde_json::json!({ "url": url });
    match rt.block_on(request(method::DOWNLOAD_ADD, Some(params))) {
        Ok(resp) => println!("[bridge] add OK: {}", serde_json::to_string(&resp).unwrap()),
        Err(e) => {
            eprintln!("[bridge] add GAGAL: {e}");
            std::process::exit(1);
        }
    }
}

// ---------- Registrasi native messaging host (Linux/XDG, §11.2) ----------

/// `$HOME/<rel>` (helper path config browser).
fn home_join(rel: &str) -> Option<PathBuf> {
    std::env::var("HOME").ok().map(|h| PathBuf::from(h).join(rel))
}

/// Direktori NativeMessagingHosts gaya Chromium (allowed_origins).
fn chromium_dirs() -> Vec<PathBuf> {
    [
        ".config/google-chrome/NativeMessagingHosts",
        ".config/chromium/NativeMessagingHosts",
        ".config/microsoft-edge/NativeMessagingHosts",
    ]
    .iter()
    .filter_map(|r| home_join(r))
    .collect()
}

/// Direktori NativeMessagingHosts Firefox (allowed_extensions).
fn firefox_dir() -> Option<PathBuf> {
    home_join(".mozilla/native-messaging-hosts")
}

fn register(args: &[String]) {
    let exe = std::env::current_exe().expect("current_exe");
    let exe_str = exe.to_string_lossy();
    let Some(chrome_id) = args.get(1).cloned() else {
        eprintln!("usage: adm-bridge register <chrome/edge-extension-id> [firefox-extension-id]");
        std::process::exit(2);
    };

    // Chrome/Edge/Chromium — allowed_origins.
    let chrome_json = format!(
        "{{\n  \"name\": \"{HOST_NAME}\",\n  \"description\": \"Alpha Download Manager host\",\n  \"path\": \"{exe_str}\",\n  \"type\": \"stdio\",\n  \"allowed_origins\": [\"chrome-extension://{chrome_id}/\"]\n}}\n"
    );
    for dir in chromium_dirs() {
        write_manifest(&dir, &chrome_json);
    }

    // Firefox — allowed_extensions.
    if let Some(fid) = args.get(2) {
        if let Some(dir) = firefox_dir() {
            let ff_json = format!(
                "{{\n  \"name\": \"{HOST_NAME}\",\n  \"description\": \"Alpha Download Manager host\",\n  \"path\": \"{exe_str}\",\n  \"type\": \"stdio\",\n  \"allowed_extensions\": [\"{fid}\"]\n}}\n"
            );
            write_manifest(&dir, &ff_json);
        }
    }
    println!("[bridge] terdaftar (path host: {exe_str}).");
}

fn write_manifest(dir: &Path, json: &str) {
    let file = dir.join(format!("{HOST_NAME}.json"));
    if let Err(e) = std::fs::create_dir_all(dir).and_then(|_| std::fs::write(&file, json)) {
        eprintln!("  ! gagal menulis {}: {e}", file.display());
    } else {
        println!("  + {}", file.display());
    }
}

fn unregister() {
    let mut dirs = chromium_dirs();
    dirs.extend(firefox_dir());
    for dir in dirs {
        let file = dir.join(format!("{HOST_NAME}.json"));
        match std::fs::remove_file(&file) {
            Ok(()) => println!("  - {}", file.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => eprintln!("  ! gagal menghapus {}: {e}", file.display()),
        }
    }
    println!("[bridge] manifest host dihapus.");
}
