//! adm-bridge — native messaging host (plan §11.2).
//!
//! WM0: hanya mode `ping` untuk verifikasi jalur bridge<->app (kriteria WM0).
//! Loop stdio native-messaging (4-byte length LE + JSON) menyusul di WM5.

use adm_ipc::{method, Request, Response, PIPE_NAME};
use tokio::io::BufReader;
use tokio::net::windows::named_pipe::ClientOptions;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().map(String::as_str).unwrap_or("ping");
    match mode {
        "ping" => match request(method::PING, None).await {
            Ok(resp) => println!("[bridge] ping OK: {}", serde_json::to_string(&resp).unwrap()),
            Err(e) => {
                eprintln!("[bridge] ping GAGAL: {e}");
                std::process::exit(1);
            }
        },
        "add" => {
            let Some(url) = args.get(1) else {
                eprintln!("usage: adm-bridge add <url>");
                std::process::exit(2);
            };
            let params = serde_json::json!({ "url": url });
            match request(method::DOWNLOAD_ADD, Some(params)).await {
                Ok(resp) => println!("[bridge] add OK: {}", serde_json::to_string(&resp).unwrap()),
                Err(e) => {
                    eprintln!("[bridge] add GAGAL: {e}");
                    std::process::exit(1);
                }
            }
        }
        other => {
            eprintln!("[bridge] mode '{other}' belum diimplementasikan (stdio host = WM5).");
            eprintln!("usage: adm-bridge [ping|add <url>]");
            std::process::exit(2);
        }
    }
}

async fn request(
    method: &str,
    params: Option<serde_json::Value>,
) -> std::io::Result<Response> {
    let client = ClientOptions::new().open(PIPE_NAME)?;
    let mut reader = BufReader::new(client);

    let req = Request::new(1, method, params);
    adm_ipc::write_message(reader.get_mut(), &req).await?;

    match adm_ipc::read_message(&mut reader).await? {
        Some(bytes) => Ok(serde_json::from_slice(&bytes)?),
        None => Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "pipe ditutup sebelum balasan",
        )),
    }
}
