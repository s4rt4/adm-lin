//! Manajer engine in-process (plan §4) + antrian (§9.4/§10, WM6).
//!
//! Tiap unduhan = task tokio dengan `CancelToken`. Antrian menahan unduhan
//! "Download Later" dan menjalankannya hingga `max` konkuren; saat satu selesai,
//! slot terisi item berikutnya (`pump`).

use crate::category::Category;
use adm_core::{download, CancelToken, DownloadRequest, Limiter, Outcome, Progress, ProgressCb};
use adm_ipc::DownloadAddParams;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::runtime::Handle;

#[derive(Debug, Clone)]
pub enum EngineEvent {
    Queued { id: u64, url: String, output: PathBuf },
    Started { id: u64, url: String, output: PathBuf },
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

    pub fn set_download_dir(&self, dir: PathBuf) {
        *self.download_dir.lock().unwrap() = dir;
    }

    pub fn active_count(&self) -> usize {
        self.active.lock().unwrap().len()
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

    /// Lanjutkan unduhan yang sudah ada (segera).
    pub fn resume(&self, id: u64, url: String, filename: String) {
        self.start(
            id,
            DownloadAddParams { url, filename: Some(filename), ..Default::default() },
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
        let output = self.output_for(&params, id);
        let cancel = CancelToken::new();
        let per_limiter = Arc::new(Limiter::unlimited());
        self.active
            .lock()
            .unwrap()
            .insert(id, (cancel.clone(), per_limiter.clone()));

        let req = DownloadRequest {
            url: params.url.clone(),
            output: output.clone(),
            connections: 8,
        };

        (self.sink)(EngineEvent::Started { id, url: params.url.clone(), output });

        let sink = self.sink.clone();
        let active = self.active.clone();
        let prog = sink.clone();
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
            let res = download(req, cancel, Some(on_progress), per_limiter, global_limiter).await;
            active.lock().unwrap().remove(&id);
            // Emit event terminal DULU (slot dianggap selesai) sebelum memulai
            // item antrian berikutnya — agar pengamat tak melihat konkuren palsu.
            let ev = match res {
                Ok(Outcome::Completed { bytes }) => EngineEvent::Completed { id, bytes },
                Ok(Outcome::Paused { downloaded, .. }) => EngineEvent::Paused { id, downloaded },
                Err(e) => EngineEvent::Failed { id, error: e.to_string() },
            };
            sink(ev);
            if queued {
                this.queue.lock().unwrap().running_ids.remove(&id);
                this.pump();
            }
        });
    }
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

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| if "\\/:*?\"<>|".contains(c) { '_' } else { c })
        .collect()
}
