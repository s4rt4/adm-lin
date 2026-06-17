//! Server IPC bridge→app via **Unix domain socket** (port `ipc_server.rs`
//! Windows yang pakai Named Pipe). Socket di `$XDG_RUNTIME_DIR/adm.sock`.
//!
//! Dua peran:
//! 1. **Bridge native-messaging** menitip `download.add` → diteruskan ke UI
//!    (dialog Add terisi otomatis, user memutuskan mulai/queue).
//! 2. **Single-instance**: instance kedua mendeteksi socket hidup, mengirim
//!    `app.activate` agar jendela pertama muncul, lalu keluar.

use crate::engine::EngineHandle;
use adm_ipc::{method, DownloadAddParams, Request, Response, METHOD_NOT_FOUND};
use eframe::egui;
use serde_json::json;
use std::sync::mpsc::Sender;
use tokio::io::BufReader;
use tokio::net::{UnixListener, UnixStream};

/// Perintah dari jalur IPC yang harus diproses di UI thread.
pub enum IpcCommand {
    /// Browser menitip unduhan → buka dialog Add terisi.
    Add(DownloadAddParams),
    /// Munculkan & fokuskan jendela (instance kedua / klik bridge).
    Activate,
}

/// Coba aktifkan instance yang sudah berjalan. `true` = ada instance hidup yang
/// sudah menerima `app.activate` (pemanggil harus keluar). `false` = tak ada
/// instance (socket absen/basi) → pemanggil lanjut jadi instance pertama.
///
/// Sinkron: dipakai di `main()` sebelum membangun runtime utama.
pub fn try_activate_existing() -> bool {
    let path = adm_ipc::unix_socket_path();
    if !path.exists() {
        return false;
    }
    let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
        return false;
    };
    rt.block_on(async move {
        // Socket basi (tak ada listener) → connect gagal → bukan instance hidup.
        let Ok(stream) = UnixStream::connect(&path).await else {
            return false;
        };
        let mut reader = BufReader::new(stream);
        let req = Request::new(1, method::APP_ACTIVATE, None);
        if adm_ipc::write_message(reader.get_mut(), &req).await.is_err() {
            return false;
        }
        // Tunggu balasan singkat agar yakin pesan terproses sebelum kita keluar.
        adm_ipc::read_message(&mut reader).await.ok().flatten().is_some()
    })
}

/// Bind socket & layani koneksi bridge. Socket basi dari proses mati dibersihkan
/// dulu (`try_activate_existing` sudah memastikan tak ada instance hidup).
pub async fn serve(tx: Sender<IpcCommand>, ctx: egui::Context, engine: EngineHandle) {
    let path = adm_ipc::unix_socket_path();
    let _ = tokio::fs::remove_file(&path).await; // hapus socket basi bila ada
    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("[ipc] gagal bind {}: {e}", path.display());
            return;
        }
    };
    eprintln!("[ipc] listen di {}", path.display());

    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let (tx, ctx, engine) = (tx.clone(), ctx.clone(), engine.clone());
                tokio::spawn(async move {
                    if let Err(e) = handle_conn(stream, tx, ctx, engine).await {
                        eprintln!("[ipc] koneksi berakhir: {e}");
                    }
                });
            }
            Err(e) => {
                eprintln!("[ipc] accept gagal: {e}");
                break;
            }
        }
    }
}

async fn handle_conn(
    stream: UnixStream,
    tx: Sender<IpcCommand>,
    ctx: egui::Context,
    engine: EngineHandle,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream);
    while let Some(bytes) = adm_ipc::read_message(&mut reader).await? {
        let resp = match serde_json::from_slice::<Request>(&bytes) {
            Ok(req) => dispatch(req, &tx, &ctx, &engine),
            Err(e) => Response::err(None, adm_ipc::INTERNAL_ERROR, format!("parse: {e}")),
        };
        adm_ipc::write_message(reader.get_mut(), &resp).await?;
    }
    Ok(())
}

fn dispatch(
    req: Request,
    tx: &Sender<IpcCommand>,
    ctx: &egui::Context,
    engine: &EngineHandle,
) -> Response {
    match req.method.as_str() {
        method::PING => Response::ok(
            req.id,
            json!({
                "pong": true,
                "app": "adm-egui",
                "version": env!("CARGO_PKG_VERSION"),
                "engine": adm_core::version(),
                "active": engine.active_count(),
            }),
        ),
        method::DOWNLOAD_ADD => match req.params {
            Some(p) => match serde_json::from_value::<DownloadAddParams>(p) {
                Ok(params) if !params.url.is_empty() => {
                    let _ = tx.send(IpcCommand::Add(params));
                    ctx.request_repaint();
                    Response::ok(req.id, json!({ "accepted": true }))
                }
                Ok(_) => Response::err(req.id, adm_ipc::INVALID_PARAMS, "url kosong"),
                Err(e) => Response::err(req.id, adm_ipc::INVALID_PARAMS, format!("params: {e}")),
            },
            None => Response::err(req.id, adm_ipc::INVALID_PARAMS, "params wajib"),
        },
        method::APP_ACTIVATE => {
            let _ = tx.send(IpcCommand::Activate);
            ctx.request_repaint();
            Response::ok(req.id, json!({ "ok": true }))
        }
        other => Response::err(req.id, METHOD_NOT_FOUND, format!("metode tidak dikenal: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{mpsc, Arc};
    use std::time::Duration;

    async fn rpc(stream: &mut BufReader<UnixStream>, req: &Request) -> Response {
        adm_ipc::write_message(stream.get_mut(), req).await.unwrap();
        let bytes = adm_ipc::read_message(stream).await.unwrap().unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[test]
    fn ipc_roundtrip_ping_add_activate() {
        // Isolasi socket path lewat XDG_RUNTIME_DIR sementara.
        let tmp = std::env::temp_dir().join(format!("adm-ipc-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        // SAFETY: hanya satu test yang menyetuh var ini → tak ada balapan.
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", &tmp) };

        // Tak ada instance → activate harus gagal (tak ada socket).
        assert!(!try_activate_existing(), "tanpa socket, tak ada yang diaktifkan");

        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let (tx, rx) = mpsc::channel::<IpcCommand>();
        let ctx = egui::Context::default();
        let engine = EngineHandle::new(rt.handle().clone(), tmp.clone(), Arc::new(|_| {}));
        rt.spawn(serve(tx, ctx, engine));

        rt.block_on(async {
            // Tunggu socket siap.
            let path = adm_ipc::unix_socket_path();
            let mut conn = None;
            for _ in 0..50 {
                if let Ok(s) = UnixStream::connect(&path).await {
                    conn = Some(s);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            let mut stream = BufReader::new(conn.expect("socket harus siap"));

            // PING
            let r = rpc(&mut stream, &Request::new(1, method::PING, None)).await;
            assert_eq!(r.result.unwrap()["pong"], serde_json::json!(true));

            // download.add → IpcCommand::Add
            let params = serde_json::to_value(DownloadAddParams {
                url: "https://example.com/a.zip".into(),
                ..Default::default()
            })
            .unwrap();
            let r = rpc(&mut stream, &Request::new(2, method::DOWNLOAD_ADD, Some(params))).await;
            assert_eq!(r.result.unwrap()["accepted"], serde_json::json!(true));

            // app.activate → IpcCommand::Activate
            let r = rpc(&mut stream, &Request::new(3, method::APP_ACTIVATE, None)).await;
            assert_eq!(r.result.unwrap()["ok"], serde_json::json!(true));
        });

        // Verifikasi perintah sampai ke UI thread.
        let cmds: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(matches!(cmds.first(), Some(IpcCommand::Add(p)) if p.url == "https://example.com/a.zip"));
        assert!(matches!(cmds.get(1), Some(IpcCommand::Activate)));

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
