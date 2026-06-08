//! adm-bridge — native messaging host (plan §11.2).
//!
//! WM0: hanya mode `ping` untuk verifikasi jalur bridge<->app (kriteria WM0).
//! Loop stdio native-messaging (4-byte length LE + JSON) menyusul di WM5.

use adm_ipc::{method, Request, Response, PIPE_NAME};
use tokio::io::BufReader;
use tokio::net::windows::named_pipe::ClientOptions;

#[tokio::main]
async fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "ping".into());
    match mode.as_str() {
        "ping" => match ping().await {
            Ok(resp) => {
                println!("[bridge] ping OK: {}", serde_json::to_string(&resp).unwrap());
            }
            Err(e) => {
                eprintln!("[bridge] ping GAGAL: {e}");
                std::process::exit(1);
            }
        },
        other => {
            eprintln!("[bridge] mode '{other}' belum diimplementasikan (stdio host = WM5).");
            eprintln!("usage: adm-bridge ping");
            std::process::exit(2);
        }
    }
}

async fn ping() -> std::io::Result<Response> {
    let client = ClientOptions::new().open(PIPE_NAME)?;
    let mut reader = BufReader::new(client);

    let req = Request::new(1, method::PING, None);
    adm_ipc::write_message(reader.get_mut(), &req).await?;

    match adm_ipc::read_message(&mut reader).await? {
        Some(bytes) => Ok(serde_json::from_slice(&bytes)?),
        None => Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "pipe ditutup sebelum balasan",
        )),
    }
}
