//! Test integrasi engine (kriteria WM1): multi-koneksi + checksum, resume
//! setelah cancel (mensimulasikan stop/crash), dan fallback non-Range.

use adm_core::{download, CancelToken, DownloadRequest, Limiter, Outcome};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;
use tiny_http::{Header, Response, Server, StatusCode};

const ETAG: &str = "\"adm-test-v1\"";

fn unlimited() -> Arc<Limiter> {
    Arc::new(Limiter::unlimited())
}

fn make_payload(n: usize) -> Vec<u8> {
    (0..n)
        .map(|i| {
            let x = (i as u64).wrapping_mul(2_654_435_761) ^ ((i as u64) >> 3);
            (x & 0xff) as u8
        })
        .collect()
}

fn sha256(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    format!("{:x}", h.finalize())
}

struct ServerGuard {
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

/// Server HTTP lokal yang melayani `payload` dengan dukungan Range + ETag.
/// Path mengandung "norange" => server abaikan Range & tak umumkan Accept-Ranges.
fn start_server(payload: Arc<Vec<u8>>) -> (String, ServerGuard) {
    let server = Arc::new(Server::http("127.0.0.1:0").unwrap());
    let addr = server.server_addr().to_ip().unwrap();
    let base = format!("http://{}", addr);
    let stop = Arc::new(AtomicBool::new(false));

    let mut threads = Vec::new();
    for _ in 0..8 {
        let server = server.clone();
        let payload = payload.clone();
        let stop = stop.clone();
        threads.push(std::thread::spawn(move || loop {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            match server.recv_timeout(Duration::from_millis(100)) {
                Ok(Some(req)) => handle(req, &payload),
                Ok(None) => continue,
                Err(_) => break,
            }
        }));
    }

    (base, ServerGuard { stop, threads })
}

fn handle(req: tiny_http::Request, payload: &[u8]) {
    let total = payload.len();
    let no_range = req.url().contains("norange");

    let range = req
        .headers()
        .iter()
        .find(|h| h.field.equiv("Range"))
        .map(|h| h.value.as_str().to_string());

    let etag_header = Header::from_bytes(&b"ETag"[..], ETAG.as_bytes()).unwrap();

    if no_range {
        let resp = Response::from_data(payload.to_vec()).with_header(etag_header);
        let _ = req.respond(resp);
        return;
    }

    match range.as_deref().and_then(parse_range) {
        Some((a, b_opt)) => {
            let b = b_opt.unwrap_or(total as u64 - 1).min(total as u64 - 1);
            let a = a.min(total as u64 - 1);
            let slice = payload[a as usize..=b as usize].to_vec();
            let cr = format!("bytes {}-{}/{}", a, b, total);
            let resp = Response::from_data(slice)
                .with_status_code(StatusCode(206))
                .with_header(Header::from_bytes(&b"Content-Range"[..], cr.as_bytes()).unwrap())
                .with_header(Header::from_bytes(&b"Accept-Ranges"[..], &b"bytes"[..]).unwrap())
                .with_header(etag_header);
            let _ = req.respond(resp);
        }
        None => {
            let resp = Response::from_data(payload.to_vec())
                .with_header(Header::from_bytes(&b"Accept-Ranges"[..], &b"bytes"[..]).unwrap())
                .with_header(etag_header);
            let _ = req.respond(resp);
        }
    }
}

fn parse_range(v: &str) -> Option<(u64, Option<u64>)> {
    let rest = v.trim().strip_prefix("bytes=")?;
    let (a, b) = rest.split_once('-')?;
    let a: u64 = a.trim().parse().ok()?;
    let b = if b.trim().is_empty() {
        None
    } else {
        Some(b.trim().parse().ok()?)
    };
    Some((a, b))
}

fn read_file(path: &std::path::Path) -> Vec<u8> {
    let mut f = std::fs::File::open(path).unwrap();
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).unwrap();
    buf
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn multi_connection_checksum() {
    let payload = Arc::new(make_payload(2 * 1024 * 1024)); // 2 MiB
    let (base, _srv) = start_server(payload.clone());
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("file.bin");

    let req = DownloadRequest {
        url: format!("{base}/file.bin"),
        output: out.clone(),
        connections: 8,
        insecure: false,
    };
    let outcome = download(req, CancelToken::new(), None, unlimited(), unlimited())
        .await
        .unwrap();
    assert!(matches!(outcome, Outcome::Completed { bytes } if bytes == payload.len() as u64));

    let got = read_file(&out);
    assert_eq!(got.len(), payload.len());
    assert_eq!(sha256(&got), sha256(&payload), "checksum harus cocok");

    // Sidecar harus terhapus setelah selesai.
    assert!(!adm_core_sidecar_exists(&out), "sidecar .adm harus hilang");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resume_after_cancel() {
    let payload = Arc::new(make_payload(2 * 1024 * 1024)); // 2 MiB
    let (base, _srv) = start_server(payload.clone());
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("file.bin");
    let url = format!("{base}/file.bin");

    // Percobaan 1: batasi 512 KiB/s lalu cancel di tengah jalan.
    let cancel = CancelToken::new();
    {
        let c = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(600)).await;
            c.cancel();
        });
    }
    let req1 = DownloadRequest {
        url: url.clone(),
        output: out.clone(),
        connections: 4,
        insecure: false,
    };
    // batasi 512 KiB/s lewat per-limiter agar cancel sempat di tengah.
    let per = Arc::new(Limiter::new(512 * 1024));
    let o1 = download(req1, cancel, None, per, unlimited()).await.unwrap();
    let mid = match o1 {
        Outcome::Paused { downloaded, .. } => downloaded,
        Outcome::Completed { .. } => panic!("seharusnya ter-pause, bukan selesai"),
    };
    assert!(mid > 0 && (mid as usize) < payload.len(), "harus parsial: {mid}");
    assert!(adm_core_sidecar_exists(&out), "sidecar harus ada setelah pause");

    // Percobaan 2: lanjutkan tanpa batas/cancel — harus selesai & utuh.
    let req2 = DownloadRequest {
        url,
        output: out.clone(),
        connections: 4,
        insecure: false,
    };
    let o2 = download(req2, CancelToken::new(), None, unlimited(), unlimited())
        .await
        .unwrap();
    assert!(matches!(o2, Outcome::Completed { .. }));

    let got = read_file(&out);
    assert_eq!(sha256(&got), sha256(&payload), "checksum setelah resume harus cocok");
    assert!(!adm_core_sidecar_exists(&out), "sidecar harus hilang setelah selesai");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fallback_no_range() {
    let payload = Arc::new(make_payload(512 * 1024)); // 512 KiB
    let (base, _srv) = start_server(payload.clone());
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("norange.bin");

    let req = DownloadRequest {
        url: format!("{base}/norange.bin"), // server abaikan Range
        output: out.clone(),
        connections: 8, // diminta 8, tapi engine harus fallback ke 1
        insecure: false,
    };
    let outcome = download(req, CancelToken::new(), None, unlimited(), unlimited())
        .await
        .unwrap();
    assert!(matches!(outcome, Outcome::Completed { .. }));
    let got = read_file(&out);
    assert_eq!(sha256(&got), sha256(&payload));
}

fn adm_core_sidecar_exists(output: &std::path::Path) -> bool {
    let mut s = output.as_os_str().to_os_string();
    s.push(".adm");
    std::path::Path::new(&s).exists()
}
