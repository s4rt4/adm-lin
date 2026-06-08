//! Manajer engine in-process (plan §4, WM2).
//!
//! Membungkus `adm-core` di atas runtime tokio bersama. Setiap unduhan jadi
//! task tokio; token cancel disimpan agar Pause/Stop All dari UI bisa
//! menghentikannya. Event lifecycle dialirkan lewat `EventSink` (GUI mem-post
//! ke UI thread; test mengumpulkan ke channel).

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

    /// Jumlah unduhan yang sedang aktif.
    pub fn active_count(&self) -> usize {
        self.active.lock().unwrap().len()
    }

    /// Batalkan semua unduhan aktif (Pause/Stop All). Sidecar tetap → resumable.
    pub fn cancel_all(&self) {
        for token in self.active.lock().unwrap().values() {
            token.cancel();
        }
    }

    /// Tambah & mulai unduhan. Mengembalikan id.
    pub fn add(&self, params: DownloadAddParams) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
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

        id
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
