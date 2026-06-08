//! Named Pipe server — jalur bridge -> app (plan §6).
//!
//! WM0: dengarkan pipe, jawab `daemon.ping`, terima `download.add` (di-stub
//! karena engine belum ada). ACL per-user menyusul di WM2.

use adm_ipc::{method, Request, Response, METHOD_NOT_FOUND, PIPE_NAME};
use serde_json::json;
use tokio::io::BufReader;
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

pub async fn serve() -> std::io::Result<()> {
    eprintln!("[ipc] listen di {PIPE_NAME}");
    let mut server = ServerOptions::new()
        .first_pipe_instance(true)
        .create(PIPE_NAME)?;

    loop {
        // Tunggu client (bridge) konek.
        server.connect().await?;
        let connected = server;

        // Siapkan instance pipe berikutnya sebelum melayani yang sekarang.
        server = ServerOptions::new().create(PIPE_NAME)?;

        tokio::spawn(async move {
            if let Err(e) = handle_conn(connected).await {
                eprintln!("[ipc] koneksi berakhir: {e}");
            }
        });
    }
}

async fn handle_conn(conn: NamedPipeServer) -> std::io::Result<()> {
    let mut reader = BufReader::new(conn);
    while let Some(bytes) = adm_ipc::read_message(&mut reader).await? {
        let resp = match serde_json::from_slice::<Request>(&bytes) {
            Ok(req) => dispatch(req),
            Err(e) => Response::err(None, adm_ipc::INTERNAL_ERROR, format!("parse: {e}")),
        };
        adm_ipc::write_message(reader.get_mut(), &resp).await?;
    }
    Ok(())
}

fn dispatch(req: Request) -> Response {
    match req.method.as_str() {
        method::PING => Response::ok(
            req.id,
            json!({
                "pong": true,
                "app": "adm-app",
                "version": env!("CARGO_PKG_VERSION"),
                "engine": adm_core::version(),
            }),
        ),
        method::DOWNLOAD_ADD => {
            // WM0: engine belum ada — terima tapi tandai belum diproses.
            eprintln!("[ipc] download.add diterima (stub WM0): {:?}", req.params);
            Response::ok(
                req.id,
                json!({ "accepted": true, "id": null, "note": "engine belum ada (WM1)" }),
            )
        }
        method::APP_ACTIVATE => Response::ok(req.id, json!({ "ok": true })),
        other => Response::err(req.id, METHOD_NOT_FOUND, format!("metode tidak dikenal: {other}")),
    }
}
