//! Manajer engine in-process (plan §4) + antrian (§9.4/§10, WM6).
//!
//! Tiap unduhan = task tokio dengan `CancelToken`. Antrian menahan unduhan
//! "Download Later" dan menjalankannya hingga `max` konkuren; saat satu selesai,
//! slot terisi item berikutnya (`pump`).

use crate::category::Category;
use adm_core::{
    download, download_hls, is_hls_url, CancelToken, DownloadRequest, Limiter, Outcome, Progress,
    ProgressCb, ReqHeaders,
};
use adm_ipc::DownloadAddParams;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::runtime::Handle;

/// Gabungkan pesan error + seluruh rantai `source()` (mis. agar detail TLS
/// "invalid peer certificate: UnknownIssuer" ikut, bukan hanya pesan dangkal
/// reqwest "error sending request") — dipakai GUI untuk mendeteksi jenis error.
fn error_chain<E: std::error::Error>(e: &E) -> String {
    let mut msg = e.to_string();
    let mut src = e.source();
    while let Some(s) = src {
        let part = s.to_string();
        if !msg.contains(&part) {
            msg.push_str(": ");
            msg.push_str(&part);
        }
        src = s.source();
    }
    msg
}

#[derive(Debug, Clone)]
pub enum EngineEvent {
    Queued { id: u64, url: String, output: PathBuf },
    Started { id: u64, url: String, output: PathBuf },
    /// Nama berkas dikoreksi (mis. dari Content-Disposition) setelah mulai.
    Renamed { id: u64, output: PathBuf },
    Progress {
        id: u64,
        downloaded: u64,
        total: Option<u64>,
        speed_bps: u64,
        segments: Vec<(u64, u64, u64)>,
    },
    Completed { id: u64, bytes: u64 },
    Paused { id: u64, downloaded: u64 },
    Failed { id: u64, error: String },
}

pub type EventSink = Arc<dyn Fn(EngineEvent) + Send + Sync>;

/// Unduhan aktif: id → (token cancel, limiter per-unduhan).
type ActiveMap = HashMap<u64, (CancelToken, Arc<Limiter>)>;

struct QueueState {
    running: bool,
    max: usize,
    pending: VecDeque<(u64, DownloadAddParams)>,
    running_ids: HashSet<u64>,
}

#[derive(Clone)]
pub struct EngineHandle {
    handle: Handle,
    download_dir: Arc<Mutex<PathBuf>>,
    sink: EventSink,
    active: Arc<Mutex<ActiveMap>>,
    next_id: Arc<AtomicU64>,
    queue: Arc<Mutex<QueueState>>,
    /// Limiter global (dibagi semua unduhan); live-adjustable.
    global_limiter: Arc<Limiter>,
    /// Metadata titipan browser per-id (referrer/UA/cookie) untuk resume; sesi
    /// saja (tak dipersist — cookie sensitif & URL/ sesi cepat kedaluwarsa).
    meta: Arc<Mutex<HashMap<u64, DownloadAddParams>>>,
}

impl EngineHandle {
    pub fn new(handle: Handle, download_dir: PathBuf, sink: EventSink) -> Self {
        Self {
            handle,
            download_dir: Arc::new(Mutex::new(download_dir)),
            sink,
            active: Arc::new(Mutex::new(HashMap::new())),
            next_id: Arc::new(AtomicU64::new(1)),
            queue: Arc::new(Mutex::new(QueueState {
                running: false,
                max: 1,
                pending: VecDeque::new(),
                running_ids: HashSet::new(),
            })),
            global_limiter: Arc::new(Limiter::unlimited()),
            meta: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Batas kecepatan global (byte/detik; `0` = tanpa batas). Live.
    pub fn set_global_limit(&self, bps: u64) {
        self.global_limiter.set_rate(bps);
    }

    /// Batas kecepatan per-unduhan (byte/detik; `0` = tanpa batas). Live.
    pub fn set_limit(&self, id: u64, bps: u64) {
        if let Some((_, lim)) = self.active.lock().unwrap().get(&id) {
            lim.set_rate(bps);
        }
    }

    pub fn download_dir(&self) -> PathBuf {
        self.download_dir.lock().unwrap().clone()
    }

    /// Handle runtime tokio (untuk spawn probe dari UI thread, mis. ukuran file).
    pub fn runtime(&self) -> Handle {
        self.handle.clone()
    }

    pub fn set_download_dir(&self, dir: PathBuf) {
        *self.download_dir.lock().unwrap() = dir;
    }

    pub fn active_count(&self) -> usize {
        self.active.lock().unwrap().len()
    }

    /// Pastikan id berikutnya minimal `min_next` (dipakai setelah memulihkan
    /// daftar unduhan dari disk agar id baru tak bentrok dengan yang dipulihkan).
    pub fn reserve_ids(&self, min_next: u64) {
        self.next_id.fetch_max(min_next, Ordering::SeqCst);
    }

    /// Batas unduhan antrian yang berjalan bersamaan.
    pub fn set_queue_max(&self, max: usize) {
        self.queue.lock().unwrap().max = max.max(1);
    }

    pub fn cancel(&self, id: u64) {
        if let Some((t, _)) = self.active.lock().unwrap().get(&id) {
            t.cancel();
        }
    }

    pub fn cancel_all(&self) {
        for (t, _) in self.active.lock().unwrap().values() {
            t.cancel();
        }
    }

    /// Tambah & mulai segera; kembalikan id.
    pub fn add(&self, params: DownloadAddParams) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        self.start(id, params, false);
        id
    }

    /// Lanjutkan unduhan yang sudah ada (segera). `insecure` mengabaikan
    /// verifikasi sertifikat TLS. Header titipan (referrer/UA/cookie) diambil
    /// dari metadata sesi bila masih ada.
    pub fn resume(&self, id: u64, url: String, filename: String, insecure: bool) {
        let (referrer, user_agent, cookies) = self
            .meta
            .lock()
            .unwrap()
            .get(&id)
            .map(|m| (m.referrer.clone(), m.user_agent.clone(), m.cookies.clone()))
            .unwrap_or_default();
        self.start(
            id,
            DownloadAddParams {
                url,
                filename: Some(filename),
                insecure,
                referrer,
                user_agent,
                cookies,
            },
            false,
        );
    }

    /// Tambahkan ke antrian ("Download Later"); jalan saat queue running & ada slot.
    pub fn enqueue(&self, params: DownloadAddParams) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let output = self.output_for(&params, id);
        (self.sink)(EngineEvent::Queued { id, url: params.url.clone(), output });
        self.queue.lock().unwrap().pending.push_back((id, params));
        self.pump();
        id
    }

    /// Mulai antrian (Start Queue).
    pub fn start_queue(&self) {
        self.queue.lock().unwrap().running = true;
        self.pump();
    }

    /// Hentikan antrian (Stop Queue): batalkan item antrian yang sedang jalan.
    pub fn stop_queue(&self) {
        let ids: Vec<u64> = {
            let mut q = self.queue.lock().unwrap();
            q.running = false;
            q.running_ids.iter().copied().collect()
        };
        for id in ids {
            self.cancel(id);
        }
    }

    /// Jalankan item pending hingga batas konkuren tercapai.
    fn pump(&self) {
        loop {
            let next = {
                let mut q = self.queue.lock().unwrap();
                if !q.running || q.running_ids.len() >= q.max {
                    break;
                }
                match q.pending.pop_front() {
                    Some((id, params)) => {
                        q.running_ids.insert(id);
                        Some((id, params))
                    }
                    None => None,
                }
            };
            match next {
                Some((id, params)) => self.start(id, params, true),
                None => break,
            }
        }
    }

    fn output_for(&self, params: &DownloadAddParams, id: u64) -> PathBuf {
        let filename = pick_filename(params, id);
        let mut dir = self.download_dir.lock().unwrap().clone();
        if let Some(folder) = Category::from_filename(&filename).folder() {
            dir.push(folder);
        }
        dir.join(filename)
    }

    fn start(&self, id: u64, params: DownloadAddParams, queued: bool) {
        // Simpan metadata titipan (referrer/UA/cookie) agar resume bisa pakai lagi.
        self.meta.lock().unwrap().insert(id, params.clone());
        let cancel = CancelToken::new();
        let per_limiter = Arc::new(Limiter::unlimited());
        self.active
            .lock()
            .unwrap()
            .insert(id, (cancel.clone(), per_limiter.clone()));

        // Baris instan dengan tebakan nama (agar list & dialog progres langsung
        // ada); dikoreksi setelah resolusi nama (Content-Disposition).
        let guess_output = self.output_for(&params, id);
        (self.sink)(EngineEvent::Started {
            id,
            url: params.url.clone(),
            output: guess_output,
        });

        let prog = self.sink.clone();
        let on_progress: ProgressCb = Arc::new(move |p: Progress| {
            let segments = p.segments.iter().map(|s| (s.start, s.end, s.downloaded)).collect();
            prog(EngineEvent::Progress {
                id,
                downloaded: p.downloaded,
                total: p.total,
                speed_bps: p.speed_bps,
                segments,
            });
        });

        let this = self.clone();
        let global_limiter = self.global_limiter.clone();
        self.handle.spawn(async move {
            // Stream HLS (.m3u8): unduh & gabung segmen jadi satu berkas .ts
            // (kategori Video), bukan unduhan berkas-tunggal biasa.
            let hls = is_hls_url(&params.url);
            // Tentukan nama berkas (Content-Disposition bila nama generik/absen).
            let mut name = this.resolve_filename(&params, id).await;
            if hls {
                name = hls_output_name(&name);
            }
            let mut dir = this.download_dir.lock().unwrap().clone();
            if let Some(folder) = Category::from_filename(&name).folder() {
                dir.push(folder);
            }
            let output = dir.join(&name);

            // Koreksi nama baris (placeholder Started sudah diemit sinkron).
            (this.sink)(EngineEvent::Renamed { id, output: output.clone() });

            let res = if hls {
                let headers = ReqHeaders {
                    referrer: params.referrer.clone(),
                    user_agent: params.user_agent.clone(),
                    cookies: params.cookies.clone(),
                };
                download_hls(
                    &params.url,
                    &output,
                    headers,
                    params.insecure,
                    cancel,
                    Some(on_progress),
                    per_limiter,
                    global_limiter,
                )
                .await
            } else {
                let req = DownloadRequest {
                    url: params.url.clone(),
                    output,
                    connections: 8,
                    insecure: params.insecure,
                    referrer: params.referrer.clone(),
                    user_agent: params.user_agent.clone(),
                    cookies: params.cookies.clone(),
                };
                download(req, cancel, Some(on_progress), per_limiter, global_limiter).await
            };
            this.active.lock().unwrap().remove(&id);
            // Emit event terminal DULU sebelum memulai item antrian berikutnya.
            let ev = match res {
                Ok(Outcome::Completed { bytes }) => EngineEvent::Completed { id, bytes },
                Ok(Outcome::Paused { downloaded, .. }) => EngineEvent::Paused { id, downloaded },
                Err(e) => EngineEvent::Failed { id, error: error_chain(&e) },
            };
            (this.sink)(ev);
            if queued {
                this.queue.lock().unwrap().running_ids.remove(&id);
                this.pump();
            }
        });
    }

    /// Nama berkas akhir. Prioritas: nama eksplisit non-generik dari pemanggil
    /// (browser/dialog) → `Content-Disposition` server → basename URL → fallback.
    async fn resolve_filename(&self, params: &DownloadAddParams, id: u64) -> String {
        let provided = params
            .filename
            .as_deref()
            .map(sanitize)
            .filter(|s| !s.is_empty());

        if let Some(p) = &provided {
            if !looks_generic(p) {
                return p.clone();
            }
        }
        let headers = adm_core::ReqHeaders {
            referrer: params.referrer.clone(),
            user_agent: params.user_agent.clone(),
            cookies: params.cookies.clone(),
        };
        if let Ok(pr) = adm_core::probe_url_with(&params.url, &headers, params.insecure).await {
            if let Some(cd) = pr.suggested_filename {
                let cd = sanitize(&cd);
                if !cd.is_empty() && !looks_generic(&cd) {
                    return cd;
                }
            }
        }
        provided
            .or_else(|| url_basename(&params.url))
            .unwrap_or_else(|| format!("download-{id}.bin"))
    }
}

fn looks_generic(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n == "download.bin"
        || n == "download"
        || (n.starts_with("download-") && n.ends_with(".bin"))
        || !n.contains('.') // tanpa ekstensi
}

fn url_basename(url: &str) -> Option<String> {
    let path = url.split(['?', '#']).next().unwrap_or("");
    path.rsplit('/')
        .next()
        .map(sanitize)
        .filter(|s| !s.is_empty() && s.contains('.'))
}

fn pick_filename(params: &DownloadAddParams, id: u64) -> String {
    if let Some(f) = &params.filename {
        if !f.is_empty() {
            return sanitize(f);
        }
    }
    let path = params.url.split(['?', '#']).next().unwrap_or("");
    if let Some(seg) = path.rsplit('/').next() {
        if !seg.is_empty() {
            return sanitize(seg);
        }
    }
    format!("download-{id}.bin")
}

/// Nama keluaran untuk stream HLS: ganti ekstensi `.m3u8` (atau nama generik)
/// dengan `.ts` agar hasil gabungan segmen bernama wajar & masuk kategori Video.
fn hls_output_name(name: &str) -> String {
    let stem = name
        .strip_suffix(".m3u8")
        .or_else(|| name.strip_suffix(".M3U8"))
        .unwrap_or(name);
    let stem = stem.trim();
    if stem.is_empty() || stem == "index" || stem == "master" || stem == "playlist" {
        "video.ts".to_string()
    } else {
        format!("{stem}.ts")
    }
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| if "\\/:*?\"<>|".contains(c) { '_' } else { c })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::time::{Duration, Instant};
    use tiny_http::{Header, Response, Server, StatusCode};

    /// Server HTTP lokal dengan dukungan Range; mati saat guard di-drop.
    struct ServerGuard {
        stop: Arc<AtomicBool>,
        threads: Vec<std::thread::JoinHandle<()>>,
    }
    impl Drop for ServerGuard {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            for t in self.threads.drain(..) {
                let _ = t.join();
            }
        }
    }

    fn start_server(payload: Arc<Vec<u8>>) -> (String, ServerGuard) {
        let server = Arc::new(Server::http("127.0.0.1:0").unwrap());
        let base = format!("http://{}", server.server_addr().to_ip().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let mut threads = Vec::new();
        for _ in 0..4 {
            let (server, payload, stop) = (server.clone(), payload.clone(), stop.clone());
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
        let total = payload.len() as u64;
        let range = req
            .headers()
            .iter()
            .find(|h| h.field.equiv("Range"))
            .and_then(|h| h.value.as_str().strip_prefix("bytes=").map(str::to_owned));
        let accept = Header::from_bytes(&b"Accept-Ranges"[..], &b"bytes"[..]).unwrap();
        match range.as_deref().and_then(|r| {
            let (a, b) = r.split_once('-')?;
            let a: u64 = a.trim().parse().ok()?;
            let b = if b.trim().is_empty() { total - 1 } else { b.trim().parse().ok()? };
            Some((a.min(total - 1), b.min(total - 1)))
        }) {
            Some((a, b)) => {
                let slice = payload[a as usize..=b as usize].to_vec();
                let cr = format!("bytes {a}-{b}/{total}");
                let _ = req.respond(
                    Response::from_data(slice)
                        .with_status_code(StatusCode(206))
                        .with_header(Header::from_bytes(&b"Content-Range"[..], cr.as_bytes()).unwrap())
                        .with_header(accept),
                );
            }
            None => {
                let _ = req.respond(Response::from_data(payload.to_vec()).with_header(accept));
            }
        }
    }

    /// E2e lapisan EngineHandle: add → event Started/Progress/Completed, dan
    /// berkas tertulis ke subfolder kategori yang benar (.mkv → Video/).
    #[test]
    fn engine_download_emits_events_and_routes_category() {
        let payload = Arc::new((0..1_000_000u32).map(|i| (i & 0xff) as u8).collect::<Vec<u8>>());
        let (base, _srv) = start_server(payload.clone());

        let dir = std::env::temp_dir().join(format!("adm-egui-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let events: Arc<Mutex<Vec<EngineEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let ev = events.clone();
        let sink: EventSink = Arc::new(move |e| ev.lock().unwrap().push(e));
        let engine = EngineHandle::new(rt.handle().clone(), dir.clone(), sink);

        engine.add(DownloadAddParams {
            url: format!("{base}/movie.mkv"),
            filename: Some("movie.mkv".into()),
            ..Default::default()
        });

        // Tunggu event Completed (maks 15 dtk).
        let deadline = Instant::now() + Duration::from_secs(15);
        let completed = loop {
            if let Some(b) = events.lock().unwrap().iter().find_map(|e| match e {
                EngineEvent::Completed { bytes, .. } => Some(*bytes),
                _ => None,
            }) {
                break Some(b);
            }
            if Instant::now() > deadline {
                break None;
            }
            std::thread::sleep(Duration::from_millis(50));
        };

        let evs = events.lock().unwrap();
        assert!(
            matches!(evs.first(), Some(EngineEvent::Started { .. })),
            "event pertama harus Started, dapat {:?}",
            evs.first()
        );
        assert!(
            evs.iter().any(|e| matches!(e, EngineEvent::Progress { .. })),
            "harus ada minimal satu event Progress"
        );
        assert_eq!(completed, Some(payload.len() as u64), "Completed dgn ukuran penuh");

        // Kategori .mkv → subfolder Video.
        let out = dir.join("Video").join("movie.mkv");
        assert!(out.exists(), "berkas harus ada di {out:?}");
        assert_eq!(std::fs::read(&out).unwrap(), *payload, "isi berkas harus utuh");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
