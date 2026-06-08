//! Manajer engine in-process (plan §4). Membungkus `adm-core` di runtime
//! tokio bersama; tiap unduhan = task dengan `CancelToken` (untuk pause/stop,
//! per-item maupun all). Event lifecycle dialirkan lewat `EventSink`.

use adm_core::{download, CancelToken, DownloadRequest, Outcome, Progress, ProgressCb};
use adm_ipc::DownloadAddParams;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::runtime::Handle;

#[derive(Debug, Clone)]
pub enum EngineEvent {
    Started { id: u64, url: String, output: PathBuf },
    Progress { id: u64, downloaded: u64, total: Option<u64>, speed_bps: u64 },
    Completed { id: u64, bytes: u64 },
    Paused { id: u64, downloaded: u64 },
    Failed { id: u64, error: String },
}

pub type EventSink = Arc<dyn Fn(EngineEvent) + Send + Sync>;

#[derive(Clone)]
pub struct EngineHandle {
    handle: Handle,
    download_dir: PathBuf,
    sink: EventSink,
    active: Arc<Mutex<HashMap<u64, CancelToken>>>,
    next_id: Arc<AtomicU64>,
}

impl EngineHandle {
    pub fn new(handle: Handle, download_dir: PathBuf, sink: EventSink) -> Self {
        Self {
            handle,
            download_dir,
            sink,
            active: Arc::new(Mutex::new(HashMap::new())),
            next_id: Arc::new(AtomicU64::new(1)),
        }
    }

    pub fn download_dir(&self) -> &std::path::Path {
        &self.download_dir
    }

    pub fn active_count(&self) -> usize {
        self.active.lock().unwrap().len()
    }

    /// Batalkan satu unduhan (sidecar tetap → resumable).
    pub fn cancel(&self, id: u64) {
        if let Some(t) = self.active.lock().unwrap().get(&id) {
            t.cancel();
        }
    }

    /// Batalkan semua (Pause/Stop All).
    pub fn cancel_all(&self) {
        for t in self.active.lock().unwrap().values() {
            t.cancel();
        }
    }

    /// Tambah unduhan baru; kembalikan id.
    pub fn add(&self, params: DownloadAddParams) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        self.start(id, params);
        id
    }

    /// Lanjutkan unduhan yang sudah ada (id dipakai ulang → baris tak duplikat).
    pub fn resume(&self, id: u64, url: String, filename: String) {
        self.start(
            id,
            DownloadAddParams {
                url,
                filename: Some(filename),
                ..Default::default()
            },
        );
    }

    fn start(&self, id: u64, params: DownloadAddParams) {
        let output = self.download_dir.join(pick_filename(&params, id));
        let cancel = CancelToken::new();
        self.active.lock().unwrap().insert(id, cancel.clone());

        let req = DownloadRequest {
            url: params.url.clone(),
            output: output.clone(),
            connections: 8,
            speed_limit_bps: None,
        };

        (self.sink)(EngineEvent::Started {
            id,
            url: params.url.clone(),
            output,
        });

        let sink = self.sink.clone();
        let active = self.active.clone();
        let prog = sink.clone();
        let on_progress: ProgressCb = Arc::new(move |p: Progress| {
            prog(EngineEvent::Progress {
                id,
                downloaded: p.downloaded,
                total: p.total,
                speed_bps: p.speed_bps,
            });
        });

        self.handle.spawn(async move {
            let res = download(req, cancel, Some(on_progress)).await;
            active.lock().unwrap().remove(&id);
            let ev = match res {
                Ok(Outcome::Completed { bytes }) => EngineEvent::Completed { id, bytes },
                Ok(Outcome::Paused { downloaded, .. }) => EngineEvent::Paused { id, downloaded },
                Err(e) => EngineEvent::Failed { id, error: e.to_string() },
            };
            sink(ev);
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
