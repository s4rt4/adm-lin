//! Pengunduh stream HLS (HTTP Live Streaming, `.m3u8`) — fitur "stream grabber"
//! gaya IDM. Mengambil playlist, memilih varian bandwidth tertinggi (master
//! playlist), mengunduh seluruh segmen `.ts` (mendekripsi AES-128-CBC bila
//! dilindungi `EXT-X-KEY`), lalu menggabungkannya menjadi satu berkas `.ts`.
//!
//! Parser playlist (`parse_master`/`parse_media`) murni & teruji. DASH (`.mpd`)
//! belum didukung (lihat catatan di `is_stream_url`).

use crate::download::{build_client, Outcome, Progress, ProgressCb, ReqHeaders};
use crate::error::{Error, Result};
use crate::limiter::Limiter;
use crate::CancelToken;
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;
use url::Url;

/// Apakah URL menunjuk ke playlist HLS (`.m3u8`). Query/fragment diabaikan.
pub fn is_hls_url(url: &str) -> bool {
    let path = url.split(['?', '#']).next().unwrap_or("");
    path.to_ascii_lowercase().ends_with(".m3u8")
}

/// Varian dalam master playlist (resolusi/bandwidth berbeda).
#[derive(Debug, Clone, PartialEq)]
pub struct Variant {
    pub bandwidth: u64,
    pub url: String,
}

/// Info kunci AES-128 yang berlaku untuk segmen-segmen berikutnya.
#[derive(Debug, Clone, PartialEq)]
struct KeyInfo {
    uri: String,
    iv: Option<[u8; 16]>,
}

/// Satu segmen media + kunci yang berlaku (bila terenkripsi) dan nomor urutnya.
#[derive(Debug, Clone, PartialEq)]
struct SegmentInfo {
    url: String,
    key: Option<KeyInfo>,
    seq: u64,
}

/// Gabungkan URL relatif segmen/varian terhadap URL playlist induknya.
fn resolve(base: &str, rel: &str) -> String {
    match Url::parse(base).and_then(|b| b.join(rel)) {
        Ok(u) => u.to_string(),
        Err(_) => rel.to_string(),
    }
}

/// Ambil nilai atribut `name=...` dari baris tag (mendukung nilai ber-quote).
fn attr<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let pos = line.find(&format!("{name}="))? + name.len() + 1;
    let rest = &line[pos..];
    if let Some(stripped) = rest.strip_prefix('"') {
        stripped.split('"').next()
    } else {
        rest.split(',').next()
    }
}

fn parse_iv(s: &str) -> Option<[u8; 16]> {
    let hex = s.trim().strip_prefix("0x").or_else(|| s.trim().strip_prefix("0X"))?;
    if hex.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// Parse master playlist → daftar varian (`EXT-X-STREAM-INF`). Kosong bila ini
/// bukan master playlist (mis. media playlist langsung).
pub fn parse_master(text: &str, base: &str) -> Vec<Variant> {
    let mut out = Vec::new();
    let mut pending: Option<u64> = None;
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with("#EXT-X-STREAM-INF") {
            pending = Some(attr(line, "BANDWIDTH").and_then(|b| b.parse().ok()).unwrap_or(0));
        } else if let Some(bw) = pending.take() {
            if !line.is_empty() && !line.starts_with('#') {
                out.push(Variant { bandwidth: bw, url: resolve(base, line) });
            }
        }
    }
    out
}

/// Parse media playlist → daftar segmen (URL diselesaikan, kunci & IV dilekatkan).
fn parse_media(text: &str, base: &str) -> Vec<SegmentInfo> {
    let mut segs = Vec::new();
    let mut seq: u64 = 0;
    let mut cur_key: Option<KeyInfo> = None;
    for raw in text.lines() {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            seq = rest.trim().parse().unwrap_or(0);
        } else if line.starts_with("#EXT-X-KEY") {
            let method = attr(line, "METHOD").unwrap_or("NONE");
            if method.eq_ignore_ascii_case("AES-128") {
                if let Some(uri) = attr(line, "URI") {
                    cur_key = Some(KeyInfo {
                        uri: resolve(base, uri),
                        iv: attr(line, "IV").and_then(parse_iv),
                    });
                }
            } else {
                cur_key = None; // METHOD=NONE → segmen berikutnya tak terenkripsi.
            }
        } else if !line.is_empty() && !line.starts_with('#') {
            segs.push(SegmentInfo { url: resolve(base, line), key: cur_key.clone(), seq });
            seq += 1;
        }
    }
    segs
}

/// Nomor urut → IV 16-byte big-endian (default bila `EXT-X-KEY` tanpa atribut IV).
fn seq_iv(seq: u64) -> [u8; 16] {
    let mut iv = [0u8; 16];
    iv[8..].copy_from_slice(&seq.to_be_bytes());
    iv
}

fn aes128_cbc_decrypt(key: &[u8; 16], iv: &[u8; 16], data: &[u8]) -> Result<Vec<u8>> {
    use cbc::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
    type Dec = cbc::Decryptor<aes::Aes128>;
    Dec::new(key.into(), iv.into())
        .decrypt_padded_vec_mut::<Pkcs7>(data)
        .map_err(|e| Error::Other(format!("dekripsi AES-128 gagal: {e}")))
}

/// Unduh stream HLS dari `url` ke `output` (digabung jadi satu berkas `.ts`).
/// Memilih varian bandwidth tertinggi pada master playlist, mengunduh segmen
/// berurutan, mendekripsi AES-128 bila perlu. `cancel` menghentikan di sela
/// segmen (mengembalikan `Paused`).
pub async fn download_hls(
    url: &str,
    output: &Path,
    headers: ReqHeaders,
    insecure: bool,
    cancel: CancelToken,
    on_progress: Option<ProgressCb>,
    per_limiter: Arc<Limiter>,
    global_limiter: Arc<Limiter>,
) -> Result<Outcome> {
    let client = build_client(insecure, &headers)?;

    // Master playlist → pilih varian bandwidth tertinggi → ambil media playlist.
    let first = client.get(url).send().await?.error_for_status()?.text().await?;
    let variants = parse_master(&first, url);
    let (media_url, media_text) = if let Some(best) = variants.iter().max_by_key(|v| v.bandwidth) {
        let text = client.get(&best.url).send().await?.error_for_status()?.text().await?;
        (best.url.clone(), text)
    } else {
        (url.to_string(), first)
    };

    let segments = parse_media(&media_text, &media_url);
    if segments.is_empty() {
        return Err(Error::Other("playlist HLS tanpa segmen".into()));
    }

    if let Some(parent) = output.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut file = std::fs::File::create(output)?;
    let mut key_cache: HashMap<String, [u8; 16]> = HashMap::new();
    let total_segs = segments.len();
    let mut downloaded: u64 = 0;
    let started = Instant::now();

    for (i, seg) in segments.iter().enumerate() {
        if cancel.is_cancelled() {
            file.flush()?;
            return Ok(Outcome::Paused { downloaded, total: None });
        }
        let raw = client.get(&seg.url).send().await?.error_for_status()?.bytes().await?;
        let data = match &seg.key {
            Some(k) => {
                let key = match key_cache.get(&k.uri) {
                    Some(kb) => *kb,
                    None => {
                        let kb = client.get(&k.uri).send().await?.error_for_status()?.bytes().await?;
                        if kb.len() != 16 {
                            return Err(Error::Other("kunci AES-128 bukan 16 byte".into()));
                        }
                        let mut arr = [0u8; 16];
                        arr.copy_from_slice(&kb);
                        key_cache.insert(k.uri.clone(), arr);
                        arr
                    }
                };
                let iv = k.iv.unwrap_or_else(|| seq_iv(seg.seq));
                aes128_cbc_decrypt(&key, &iv, &raw)?
            }
            None => raw.to_vec(),
        };
        // Hormati limiter (global & per-unduhan) berdasar byte yang ditulis.
        global_limiter.acquire(data.len()).await;
        per_limiter.acquire(data.len()).await;
        file.write_all(&data)?;
        downloaded += data.len() as u64;

        if let Some(cb) = &on_progress {
            let secs = started.elapsed().as_secs_f64().max(0.001);
            cb(Progress {
                downloaded,
                total: None, // total byte stream tak diketahui di muka.
                speed_bps: (downloaded as f64 / secs) as u64,
                eta_secs: None,
                connections: 1,
                // Satu "segmen sintetis" agar dialog progres menampilkan i/N.
                segments: vec![crate::SegmentProgress {
                    start: 0,
                    end: total_segs as u64,
                    downloaded: (i + 1) as u64,
                }],
            });
        }
    }
    file.flush()?;
    Ok(Outcome::Completed { bytes: downloaded })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_hls_url() {
        assert!(is_hls_url("https://x/stream.m3u8"));
        assert!(is_hls_url("https://x/a/b.M3U8?token=1"));
        assert!(!is_hls_url("https://x/video.mp4"));
        assert!(!is_hls_url("https://x/manifest.mpd"));
    }

    #[test]
    fn parses_master_variants_resolved_and_sorted() {
        let m = "#EXTM3U\n\
            #EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=640x360\n\
            low/index.m3u8\n\
            #EXT-X-STREAM-INF:BANDWIDTH=2400000,RESOLUTION=1280x720\n\
            https://cdn.example.com/hi/index.m3u8\n";
        let v = parse_master(m, "https://host.com/path/master.m3u8");
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].bandwidth, 800000);
        assert_eq!(v[0].url, "https://host.com/path/low/index.m3u8");
        assert_eq!(v[1].url, "https://cdn.example.com/hi/index.m3u8");
        assert_eq!(v.iter().max_by_key(|x| x.bandwidth).unwrap().bandwidth, 2400000);
    }

    #[test]
    fn parses_media_segments_with_key_and_seq() {
        let m = "#EXTM3U\n\
            #EXT-X-MEDIA-SEQUENCE:10\n\
            #EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\",IV=0x000102030405060708090a0b0c0d0e0f\n\
            #EXTINF:4.0,\n\
            seg0.ts\n\
            #EXTINF:4.0,\n\
            seg1.ts\n\
            #EXT-X-KEY:METHOD=NONE\n\
            #EXTINF:4.0,\n\
            seg2.ts\n\
            #EXT-X-ENDLIST\n";
        let s = parse_media(m, "https://h.com/v/index.m3u8");
        assert_eq!(s.len(), 3);
        assert_eq!(s[0].url, "https://h.com/v/seg0.ts");
        assert_eq!(s[0].seq, 10);
        assert_eq!(s[0].key.as_ref().unwrap().uri, "https://h.com/v/key.bin");
        assert!(s[0].key.as_ref().unwrap().iv.is_some());
        assert_eq!(s[1].seq, 11);
        assert!(s[2].key.is_none(), "METHOD=NONE menonaktifkan enkripsi");
    }

    #[test]
    fn aes_roundtrip_matches_plaintext() {
        use cbc::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};
        type Enc = cbc::Encryptor<aes::Aes128>;
        let key = [7u8; 16];
        let iv = [3u8; 16];
        let plain = b"the quick brown fox jumps over the lazy dog!!".to_vec();
        let ct = Enc::new(&key.into(), &iv.into()).encrypt_padded_vec_mut::<Pkcs7>(&plain);
        let got = aes128_cbc_decrypt(&key, &iv, &ct).unwrap();
        assert_eq!(got, plain);
    }

    /// E2e: server HLS lokal (master → media → 2 segmen polos + 1 segmen AES-128)
    /// → `download_hls` harus menghasilkan berkas = gabungan ketiga plaintext.
    #[test]
    fn download_hls_concatenates_and_decrypts() {
        use cbc::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::Duration;
        use tiny_http::{Response, Server};

        let key = [9u8; 16];
        let iv = seq_iv(2); // segmen ke-3 (seq=2) tanpa atribut IV → pakai nomor urut.
        let seg0 = b"AAAA-segment-zero".to_vec();
        let seg1 = b"BBBB-segment-one".to_vec();
        let seg2 = b"CCCC-segment-two-encrypted".to_vec();
        type Enc = cbc::Encryptor<aes::Aes128>;
        let seg2_ct = Enc::new(&key.into(), &iv.into()).encrypt_padded_vec_mut::<Pkcs7>(&seg2);

        let server = Arc::new(Server::http("127.0.0.1:0").unwrap());
        let base = format!("http://{}", server.server_addr().to_ip().unwrap());
        let master = format!(
            "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=500000\nlo.m3u8\n\
             #EXT-X-STREAM-INF:BANDWIDTH=1500000\nhi.m3u8\n"
        );
        let media = "#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:0\n\
            #EXTINF:1.0,\nseg0.ts\n#EXTINF:1.0,\nseg1.ts\n\
            #EXT-X-KEY:METHOD=AES-128,URI=\"enc.key\"\n#EXTINF:1.0,\nseg2.ts\n#EXT-X-ENDLIST\n";

        let stop = Arc::new(AtomicBool::new(false));
        let (srv, st) = (server.clone(), stop.clone());
        let (m, md) = (master.clone(), media.to_string());
        let (s0, s1, s2, k) = (seg0.clone(), seg1.clone(), seg2_ct.clone(), key.to_vec());
        let th = std::thread::spawn(move || loop {
            if st.load(Ordering::SeqCst) {
                break;
            }
            if let Ok(Some(req)) = srv.recv_timeout(Duration::from_millis(100)) {
                let url = req.url().to_string();
                let body: Vec<u8> = if url.ends_with("master.m3u8") {
                    m.clone().into_bytes()
                } else if url.ends_with("hi.m3u8") {
                    md.clone().into_bytes()
                } else if url.ends_with("seg0.ts") {
                    s0.clone()
                } else if url.ends_with("seg1.ts") {
                    s1.clone()
                } else if url.ends_with("seg2.ts") {
                    s2.clone()
                } else if url.ends_with("enc.key") {
                    k.clone()
                } else {
                    Vec::new()
                };
                let _ = req.respond(Response::from_data(body));
            }
        });

        let dir = std::env::temp_dir().join(format!("adm-hls-e2e-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let out = dir.join("stream.ts");
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let outcome = rt
            .block_on(download_hls(
                &format!("{base}/master.m3u8"),
                &out,
                ReqHeaders::default(),
                false,
                CancelToken::new(),
                None,
                Arc::new(Limiter::unlimited()),
                Arc::new(Limiter::unlimited()),
            ))
            .unwrap();

        stop.store(true, Ordering::SeqCst);
        let _ = th.join();

        let mut expect = seg0.clone();
        expect.extend_from_slice(&seg1);
        expect.extend_from_slice(&seg2);
        assert!(matches!(outcome, Outcome::Completed { .. }));
        assert_eq!(std::fs::read(&out).unwrap(), expect, "berkas = gabungan plaintext");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
