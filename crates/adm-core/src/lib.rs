//! adm-core — engine unduhan portabel (dipakai bersama plan Linux + Windows).
//!
//! WM0: placeholder. Logika engine (probe Range, segmentasi statis→dinamis,
//! resume tahan-crash, token-bucket limiter) menyusul di WM1 — lihat plan §7.

/// Versi crate, dipakai a.l. untuk balasan `daemon.ping`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
