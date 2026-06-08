//! Probe Range: deteksi ukuran, dukungan resume, dan validator (plan §7).

use crate::error::Result;
use reqwest::header::{ACCEPT_RANGES, CONTENT_RANGE, ETAG, LAST_MODIFIED, RANGE};
use reqwest::Client;

#[derive(Debug, Clone)]
pub struct Probe {
    /// Total ukuran bila diketahui.
    pub total: Option<u64>,
    /// Server mendukung Range (resume + multi-koneksi).
    pub resumable: bool,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

/// Probe dengan `Range: bytes=0-0` — cara paling andal mendeteksi dukungan
/// Range sekaligus total (lewat `Content-Range: bytes 0-0/<total>`).
pub async fn probe(client: &Client, url: &str) -> Result<Probe> {
    let resp = client
        .get(url)
        .header(RANGE, "bytes=0-0")
        .send()
        .await?;

    let status = resp.status();
    let headers = resp.headers();

    let etag = header_str(headers, ETAG);
    let last_modified = header_str(headers, LAST_MODIFIED);

    let (total, resumable) = if status.as_u16() == 206 {
        // Content-Range: bytes 0-0/12345
        let total = headers
            .get(CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_content_range_total);
        (total, true)
    } else if status.is_success() {
        // 200: tanpa dukungan Range (kecuali Accept-Ranges: bytes).
        let total = resp.content_length();
        let accept_ranges = headers
            .get(ACCEPT_RANGES)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.eq_ignore_ascii_case("bytes"))
            .unwrap_or(false);
        (total, accept_ranges)
    } else {
        return Err(crate::error::Error::BadStatus(status.as_u16()));
    };

    Ok(Probe {
        total,
        // Resume aman hanya bila ukuran diketahui.
        resumable: resumable && total.is_some(),
        etag,
        last_modified,
    })
}

fn header_str(headers: &reqwest::header::HeaderMap, name: reqwest::header::HeaderName) -> Option<String> {
    headers.get(name).and_then(|v| v.to_str().ok()).map(|s| s.to_string())
}

fn parse_content_range_total(v: &str) -> Option<u64> {
    // "bytes 0-0/12345" -> 12345 ; "bytes 0-0/*" -> None
    let slash = v.rfind('/')?;
    let total = &v[slash + 1..];
    total.trim().parse().ok()
}
