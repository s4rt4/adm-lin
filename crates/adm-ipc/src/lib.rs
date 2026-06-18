//! adm-ipc — protokol JSON-RPC + framing LSP-style untuk jalur **bridge <-> app**.
//!
//! Lihat plan §6. v2.x: jalur ini satu-satunya IPC yang tersisa; GUI<->engine
//! sekarang in-process, jadi protokol di sini sengaja minimal.

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Nama Named Pipe per-user (plan §16, Windows). WM2 akan menambah suffix SID + ACL.
pub const PIPE_NAME: &str = r"\\.\pipe\adm";

/// Path Unix domain socket per-user (Linux/macOS). Default
/// `$XDG_RUNTIME_DIR/adm.sock` (sudah per-user & dibersihkan otomatis saat
/// logout); fallback `/tmp/adm-<uid>.sock` bila `XDG_RUNTIME_DIR` tak ada.
#[cfg(unix)]
pub fn unix_socket_path() -> std::path::PathBuf {
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        if !dir.is_empty() {
            return std::path::PathBuf::from(dir).join("adm.sock");
        }
    }
    // Fallback (jarang — XDG_RUNTIME_DIR hampir selalu ada di sesi desktop):
    // sertakan nama user agar tak bentrok antar-user di `/tmp`.
    let user = std::env::var("USER").unwrap_or_else(|_| "default".into());
    std::env::temp_dir().join(format!("adm-{user}.sock"))
}

/// Path lockfile single-instance (mendampingi `unix_socket_path`). Dipakai
/// `flock` untuk pemilihan primary yang atomik — mencegah race "banyak instance
/// merasa jadi yang pertama" saat beberapa proses lahir nyaris bersamaan
/// (mis. browser menjalankan satu bridge per-pesan, tiap bridge men-spawn app).
#[cfg(unix)]
pub fn unix_lock_path() -> std::path::PathBuf {
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        if !dir.is_empty() {
            return std::path::PathBuf::from(dir).join("adm.lock");
        }
    }
    let user = std::env::var("USER").unwrap_or_else(|_| "default".into());
    std::env::temp_dir().join(format!("adm-{user}.lock"))
}

/// Path lockfile **spawn** untuk bridge. Berbeda dari `unix_lock_path` (yang
/// dipegang app selama hidup): kunci ini dipegang sebentar oleh satu bridge saat
/// men-spawn app, agar beberapa bridge yang dijalankan browser bersamaan (satu
/// per pesan native-messaging) tak ramai-ramai men-spawn app. Harus terpisah
/// dari kunci app — bila sama, bridge yang memegangnya akan memblokir app.
#[cfg(unix)]
pub fn unix_spawn_lock_path() -> std::path::PathBuf {
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        if !dir.is_empty() {
            return std::path::PathBuf::from(dir).join("adm-spawn.lock");
        }
    }
    let user = std::env::var("USER").unwrap_or_else(|_| "default".into());
    std::env::temp_dir().join(format!("adm-{user}-spawn.lock"))
}

/// Nama metode yang didukung jalur bridge<->app.
pub mod method {
    /// Cek apakah `adm-app` hidup. Dipakai bridge & single-instance.
    pub const PING: &str = "daemon.ping";
    /// Tambah unduhan (dari browser). Params = [`super::DownloadAddParams`].
    pub const DOWNLOAD_ADD: &str = "download.add";
    /// Munculkan jendela `adm-app`.
    pub const APP_ACTIVATE: &str = "app.activate";
}

/// Kode error JSON-RPC standar.
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub jsonrpc: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

impl Request {
    pub fn new(id: u64, method: &str, params: Option<serde_json::Value>) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: Some(id),
            method: method.into(),
            params,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub jsonrpc: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Response {
    pub fn ok(id: Option<u64>, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: Option<u64>, code: i64, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(RpcError {
                code,
                message: message.into(),
            }),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

/// Parameter `download.add` — data unduhan yang dititip browser (plan §6, §11.2).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DownloadAddParams {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub referrer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cookies: Option<String>,
    /// Abaikan verifikasi sertifikat TLS (saat user memilih "terima risiko").
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub insecure: bool,
}

/// Tulis satu pesan ber-framing `Content-Length: N\r\n\r\n<body>`.
pub async fn write_message<W>(w: &mut W, value: &impl Serialize) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let body = serde_json::to_vec(value)?;
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    w.write_all(header.as_bytes()).await?;
    w.write_all(&body).await?;
    w.flush().await?;
    Ok(())
}

/// Baca satu pesan ber-framing. `Ok(None)` = EOF (peer menutup pipe).
pub async fn read_message<R>(r: &mut R) -> std::io::Result<Option<Vec<u8>>>
where
    R: AsyncBufReadExt + Unpin,
{
    let mut content_length: Option<usize> = None;
    let mut line = String::new();
    loop {
        line.clear();
        let n = r.read_line(&mut line).await?;
        if n == 0 {
            return Ok(None); // EOF
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break; // akhir header
        }
        if let Some(v) = trimmed.strip_prefix("Content-Length:") {
            content_length = v.trim().parse().ok();
        }
    }

    let len = content_length.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "framing tanpa Content-Length",
        )
    })?;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    Ok(Some(buf))
}
