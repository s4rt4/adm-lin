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
mod grabber;
mod hls;
mod limiter;
mod platform;
mod probe;
mod sidecar;

pub use download::{
    download, fetch_text, probe_url, probe_url_with, CancelToken, DownloadRequest, Outcome,
    Progress, ProgressCb, ReqHeaders, SegmentProgress,
};
pub use error::{Error, Result};
pub use grabber::{extract_links, grab_links};
pub use hls::{download_hls, is_hls_url, parse_master, Variant};
pub use limiter::Limiter;
pub use probe::{probe, Probe};

/// Versi crate, dipakai a.l. untuk balasan `daemon.ping`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
