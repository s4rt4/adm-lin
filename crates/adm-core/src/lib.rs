//! adm-core — engine unduhan portabel ADM (plan §7).
//!
//! WM1: probe Range, segmentasi multi-koneksi statis, positioned write
//! (`seek_write`) + pre-alokasi (`set_len`/`SetEndOfFile`), sidecar `.adm`
//! untuk resume tahan-crash, dan token-bucket limiter global.
//!
//! Segmentasi dinamis (work-stealing) & per-file limiter menyusul (lihat
//! TODO di plan §7 / WM6).

mod download;
mod error;
mod limiter;
mod platform;
mod probe;
mod sidecar;

pub use download::{
    download, CancelToken, DownloadRequest, Outcome, Progress, ProgressCb,
};
pub use error::{Error, Result};
pub use probe::{probe, Probe};

/// Versi crate, dipakai a.l. untuk balasan `daemon.ping`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
