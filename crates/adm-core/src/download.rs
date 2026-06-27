//! Orkestrator unduhan: segmentasi multi-koneksi, positioned write, resume,
//! limiter, progres/ETA (plan §7).

use crate::error::{Error, Result};
use crate::limiter::Limiter;
use crate::sidecar::{self, SegRecord, Sidecar};
use crate::{platform, probe};
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, COOKIE, RANGE, REFERER};
use reqwest::Client;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Token pembatalan (pause/stop). Shared antar task.
#[derive(Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Permintaan unduhan.
#[derive(Debug, Clone)]
pub struct DownloadRequest {
    pub url: String,
    pub output: PathBuf,
    /// jumlah koneksi yang diinginkan (di-clamp ke [1, 64]).
    pub connections: usize,
    /// Abaikan verifikasi sertifikat TLS (server bersertifikat invalid).
    pub insecure: bool,
    /// Header titipan browser (untuk unduhan ber-autentikasi seperti Gmail).
    pub referrer: Option<String>,
    pub user_agent: Option<String>,
    pub cookies: Option<String>,
}

/// Snapshot progres untuk callback.
/// Progres satu segmen/koneksi (untuk bar segmen di GUI, §9.11).
#[derive(Debug, Clone, Copy)]
pub struct SegmentProgress {
    pub start: u64,
    /// inklusif.
    pub end: u64,
    pub downloaded: u64,
}

#[derive(Debug, Clone)]
pub struct Progress {
    pub downloaded: u64,
    pub total: Option<u64>,
    /// kecepatan sesaat (byte/detik).
    pub speed_bps: u64,
    /// estimasi sisa waktu (detik); `None` bila tak terhitung.
    pub eta_secs: Option<u64>,
    pub connections: usize,
    /// snapshot progres per segmen (kosong untuk unduhan satu-koneksi non-resumable).
    pub segments: Vec<SegmentProgress>,
}

/// Hasil akhir.
#[derive(Debug, Clone)]
pub enum Outcome {
    Completed { bytes: u64 },
    Paused { downloaded: u64, total: Option<u64> },
}

pub type ProgressCb = Arc<dyn Fn(Progress) + Send + Sync>;

struct SegState {
    start: u64,
    end: u64, // inklusif
    downloaded: AtomicU64,
}

impl SegState {
    fn len(&self) -> u64 {
        self.end - self.start + 1
    }
    fn is_done(&self) -> bool {
        self.downloaded.load(Ordering::Relaxed) >= self.len()
    }
}

/// Header titipan browser untuk unduhan ber-autentikasi (referrer/UA/cookie).
#[derive(Default, Clone)]
pub struct ReqHeaders {
    pub referrer: Option<String>,
    pub user_agent: Option<String>,
    pub cookies: Option<String>,
}

pub(crate) fn build_client(insecure: bool, h: &ReqHeaders) -> Result<Client> {
    let ua = h
        .user_agent
        .clone()
        .unwrap_or_else(|| concat!("ADM/", env!("CARGO_PKG_VERSION")).to_string());
    let mut b = Client::builder()
        .user_agent(ua)
        // Jangan simpan koneksi idle (probe singkat tak meninggalkan keep-alive
        // yang menggantung; tiap segmen pakai koneksi sendiri).
        .pool_max_idle_per_host(0);
    let mut headers = HeaderMap::new();
    if let Some(r) = &h.referrer {
        if let Ok(v) = HeaderValue::from_str(r) {
            headers.insert(REFERER, v);
        }
    }
    if let Some(c) = &h.cookies {
        if let Ok(v) = HeaderValue::from_str(c) {
            headers.insert(COOKIE, v);
        }
    }
    if !headers.is_empty() {
        b = b.default_headers(headers);
    }
    if insecure {
        // User memilih "terima risiko": jangan verifikasi sertifikat TLS.
        b = b.danger_accept_invalid_certs(true);
    }
    Ok(b.build()?)
}

/// Probe ringan satu URL (client default tanpa header titipan).
pub async fn probe_url(url: &str) -> Result<probe::Probe> {
    probe_url_with(url, &ReqHeaders::default(), false).await
}

/// Probe dengan header titipan browser (referrer/UA/cookie) + opsi insecure —
/// agar unduhan ber-autentikasi (Gmail dll) memberi ukuran & Content-Disposition.
pub async fn probe_url_with(url: &str, h: &ReqHeaders, insecure: bool) -> Result<probe::Probe> {
    let client = build_client(insecure, h)?;
    probe::probe(&client, url).await
}

/// Unduh isi sebuah halaman sebagai teks (dipakai site grabber).
pub async fn fetch_text(url: &str) -> Result<String> {
    let client = build_client(false, &ReqHeaders::default())?;
    let resp = client.get(url).send().await?.error_for_status()?;
    Ok(resp.text().await?)
}

/// Jalankan unduhan (resume otomatis bila sidecar cocok). Blokir sampai
/// selesai, paused (cancel), atau error.
pub async fn download(
    req: DownloadRequest,
    cancel: CancelToken,
    on_progress: Option<ProgressCb>,
    per_limiter: Arc<Limiter>,
    global_limiter: Arc<Limiter>,
) -> Result<Outcome> {
    let headers = ReqHeaders {
        referrer: req.referrer.clone(),
        user_agent: req.user_agent.clone(),
        cookies: req.cookies.clone(),
    };
    let client = build_client(req.insecure, &headers)?;
    let pr = probe::probe(&client, &req.url).await?;
    let sidecar_path = sidecar::path_for(&req.output);

    // Jalur non-resumable: ukuran tak diketahui atau Range tak didukung.
    if !pr.resumable {
        return download_single(&client, &req, cancel, on_progress, pr.total, per_limiter, global_limiter).await;
    }

    let total = pr.total.ok_or(Error::UnknownSize)?;
    let conns = req.connections.clamp(1, 64);

    // Resume bila sidecar cocok; selain itu rencana segar.
    let segments: Vec<Arc<SegState>> = match sidecar::load(&sidecar_path) {
        Some(sc) if sc.is_compatible(&req.url, &pr) => sc
            .segments
            .into_iter()
            .map(|r| {
                Arc::new(SegState {
                    start: r.start,
                    end: r.end,
                    downloaded: AtomicU64::new(r.downloaded.min(r.end - r.start + 1)),
                })
            })
            .collect(),
        _ => plan_segments(total, conns),
    };

    platform::preallocate(&req.output, total)?;

    let downloaded0: u64 = segments
        .iter()
        .map(|s| s.downloaded.load(Ordering::Relaxed))
        .sum();
    let global = Arc::new(AtomicU64::new(downloaded0));

    // Tulis sidecar awal (agar crash sebelum flush pertama tetap resumable).
    write_sidecar(&sidecar_path, &req, &pr, total, &segments);

    // Reporter + flusher sidecar berkala.
    let reporter_stop = Arc::new(AtomicBool::new(false));
    let reporter = spawn_reporter(
        reporter_stop.clone(),
        global.clone(),
        Arc::new(segments.to_vec()),
        sidecar_path.clone(),
        req.clone(),
        pr.clone(),
        total,
        on_progress.clone(),
    );

    // Task per segmen.
    let mut handles = Vec::with_capacity(segments.len());
    for seg in &segments {
        let h = tokio::spawn(run_segment(
            client.clone(),
            req.url.clone(),
            seg.clone(),
            req.output.clone(),
            per_limiter.clone(),
            global_limiter.clone(),
            cancel.clone(),
            global.clone(),
        ));
        handles.push(h);
    }

    let mut first_err: Option<Error> = None;
    for h in handles {
        match h.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                first_err.get_or_insert(e);
            }
            Err(join) => {
                first_err.get_or_insert(Error::Other(format!("task panik: {join}")));
            }
        }
    }

    // Hentikan reporter & flush state terakhir.
    reporter_stop.store(true, Ordering::SeqCst);
    let _ = reporter.await;
    write_sidecar(&sidecar_path, &req, &pr, total, &segments);

    if cancel.is_cancelled() {
        let dl = global.load(Ordering::Relaxed);
        return Ok(Outcome::Paused {
            downloaded: dl,
            total: Some(total),
        });
    }
    if let Some(e) = first_err {
        return Err(e);
    }

    let complete = segments.iter().all(|s| s.is_done());
    if complete {
        sidecar::remove(&sidecar_path);
        Ok(Outcome::Completed { bytes: total })
    } else {
        // Tidak cancel, tak ada error, tapi belum penuh: anggap paused (resumable).
        Ok(Outcome::Paused {
            downloaded: global.load(Ordering::Relaxed),
            total: Some(total),
        })
    }
}

/// Segmentasi statis: bagi `total` ke `conns` rentang kontigu hampir sama.
fn plan_segments(total: u64, conns: usize) -> Vec<Arc<SegState>> {
    let conns = conns.max(1) as u64;
    let base = total / conns;
    let mut segs = Vec::new();
    let mut start = 0u64;
    for i in 0..conns {
        if start >= total {
            break;
        }
        let mut end = start + base - 1;
        if i == conns - 1 {
            end = total - 1; // segmen terakhir menyapu sisa
        }
        segs.push(Arc::new(SegState {
            start,
            end,
            downloaded: AtomicU64::new(0),
        }));
        start = end + 1;
    }
    segs
}

#[allow(clippy::too_many_arguments)]
async fn run_segment(
    client: Client,
    url: String,
    seg: Arc<SegState>,
    output: PathBuf,
    per_limiter: Arc<Limiter>,
    global_limiter: Arc<Limiter>,
    cancel: CancelToken,
    global: Arc<AtomicU64>,
) -> Result<()> {
    let begin = seg.start + seg.downloaded.load(Ordering::Relaxed);
    if begin > seg.end {
        return Ok(()); // sudah selesai
    }

    let range = format!("bytes={}-{}", begin, seg.end);
    let resp = client
        .get(&url)
        .header(RANGE, range)
        .send()
        .await?
        .error_for_status()?;

    let file = platform::open_writer(&output)?;
    let mut offset = begin;
    let mut stream = resp.bytes_stream();

    while let Some(item) = stream.next().await {
        if cancel.is_cancelled() {
            return Ok(());
        }
        let chunk = item?;
        // Jangan melampaui akhir segmen (server bisa abaikan batas Range).
        let allowed = (seg.end + 1 - offset) as usize;
        let data: &[u8] = if chunk.len() > allowed {
            &chunk[..allowed]
        } else {
            &chunk
        };
        if data.is_empty() {
            break;
        }
        per_limiter.acquire(data.len()).await;
        global_limiter.acquire(data.len()).await;
        platform::write_at(&file, data, offset)?;
        let n = data.len() as u64;
        offset += n;
        seg.downloaded.fetch_add(n, Ordering::Relaxed);
        global.fetch_add(n, Ordering::Relaxed);
        if offset > seg.end {
            break;
        }
    }
    Ok(())
}

/// Jalur satu-koneksi tanpa resume (server tanpa Range / ukuran tak diketahui).
#[allow(clippy::too_many_arguments)]
async fn download_single(
    client: &Client,
    req: &DownloadRequest,
    cancel: CancelToken,
    on_progress: Option<ProgressCb>,
    total: Option<u64>,
    per_limiter: Arc<Limiter>,
    global_limiter: Arc<Limiter>,
) -> Result<Outcome> {
    use std::io::Write;

    let resp = client.get(&req.url).send().await?.error_for_status()?;
    if let Some(parent) = req.output.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&req.output)?;

    let mut stream = resp.bytes_stream();
    let mut downloaded = 0u64;

    while let Some(item) = stream.next().await {
        if cancel.is_cancelled() {
            return Ok(Outcome::Paused { downloaded, total });
        }
        let chunk = item?;
        per_limiter.acquire(chunk.len()).await;
        global_limiter.acquire(chunk.len()).await;
        file.write_all(&chunk)?;
        downloaded += chunk.len() as u64;
        if let Some(cb) = &on_progress {
            cb(Progress {
                downloaded,
                total,
                speed_bps: 0,
                eta_secs: None,
                connections: 1,
                segments: Vec::new(),
            });
        }
    }
    file.flush()?;
    Ok(Outcome::Completed { bytes: downloaded })
}

#[allow(clippy::too_many_arguments)]
fn spawn_reporter(
    stop: Arc<AtomicBool>,
    global: Arc<AtomicU64>,
    segments: Arc<Vec<Arc<SegState>>>,
    sidecar_path: PathBuf,
    req: DownloadRequest,
    pr: probe::Probe,
    total: u64,
    on_progress: Option<ProgressCb>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut prev = global.load(Ordering::Relaxed);
        let interval = Duration::from_millis(500);
        loop {
            tokio::time::sleep(interval).await;
            let cur = global.load(Ordering::Relaxed);
            let speed = ((cur.saturating_sub(prev)) as f64 / interval.as_secs_f64()) as u64;
            prev = cur;

            if let Some(cb) = &on_progress {
                let eta = total.saturating_sub(cur).checked_div(speed);
                let segs: Vec<SegmentProgress> = segments
                    .iter()
                    .map(|s| SegmentProgress {
                        start: s.start,
                        end: s.end,
                        downloaded: s.downloaded.load(Ordering::Relaxed),
                    })
                    .collect();
                cb(Progress {
                    downloaded: cur,
                    total: Some(total),
                    speed_bps: speed,
                    eta_secs: eta,
                    connections: segments.len(),
                    segments: segs,
                });
            }

            // Flush sidecar berkala (tahan-crash).
            write_sidecar(&sidecar_path, &req, &pr, total, &segments);

            if stop.load(Ordering::SeqCst) {
                break;
            }
        }
    })
}

fn write_sidecar(
    path: &std::path::Path,
    req: &DownloadRequest,
    pr: &probe::Probe,
    total: u64,
    segments: &[Arc<SegState>],
) {
    let sc = Sidecar {
        url: req.url.clone(),
        total,
        etag: pr.etag.clone(),
        last_modified: pr.last_modified.clone(),
        segments: segments
            .iter()
            .map(|s| SegRecord {
                start: s.start,
                end: s.end,
                downloaded: s.downloaded.load(Ordering::Relaxed),
            })
            .collect(),
    };
    let _ = sidecar::save(path, &sc);
}
