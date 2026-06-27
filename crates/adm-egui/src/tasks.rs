//! Pendukung menu Tasks: parsing batch download (multi-URL + ekspansi pola
//! `[a-b]`) dan pembacaan clipboard. Logika parsing diport apa adanya dari
//! versi Windows (`adm-app/src/tasks.rs`) — murni & teruji, bebas-Win32.

use std::collections::HashSet;

fn is_url(s: &str) -> bool {
    (s.starts_with("http://") || s.starts_with("https://")) && s.len() > 10
}

/// Ambil semua URL http(s) dari teks bebas (per token), urut & tanpa duplikat.
pub fn extract_urls(text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for tok in text.split(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '(' | ')')) {
        let t = tok.trim().trim_end_matches(['.', ',', ';']);
        if is_url(t) && seen.insert(t.to_string()) {
            out.push(t.to_string());
        }
    }
    out
}

/// Ekspansi pola `[start-end]` numerik (mendukung zero-pad: `[01-12]`).
/// Beberapa pola dalam satu baris diekspansi kartesian. Dibatasi agar aman.
pub fn expand_pattern(line: &str) -> Vec<String> {
    if let Some(open) = line.find('[') {
        if let Some(close_rel) = line[open..].find(']') {
            let close = open + close_rel;
            let inner = &line[open + 1..close];
            if let Some(dash) = inner.find('-') {
                let a = &inner[..dash];
                let b = &inner[dash + 1..];
                if let (Ok(start), Ok(end)) = (a.parse::<u64>(), b.parse::<u64>()) {
                    if start <= end && end - start < 100_000 {
                        let width = a.len();
                        let pad = a.starts_with('0') && width > 1;
                        let (pre, post) = (&line[..open], &line[close + 1..]);
                        let mut out = Vec::new();
                        for n in start..=end {
                            let num = if pad {
                                format!("{n:0width$}")
                            } else {
                                n.to_string()
                            };
                            out.extend(expand_pattern(&format!("{pre}{num}{post}")));
                        }
                        return out;
                    }
                }
            }
        }
    }
    vec![line.to_string()]
}

/// Pecah teks batch (per baris) → daftar URL final (wildcard diekspansi).
pub fn parse_batch(text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for line in text.lines() {
        let l = line.trim();
        if l.is_empty() {
            continue;
        }
        for u in expand_pattern(l) {
            let u = u.trim().to_string();
            if is_url(&u) && seen.insert(u.clone()) {
                out.push(u);
            }
        }
    }
    out
}

/// Baca teks dari clipboard sistem (X11/Wayland via arboard), bila ada.
pub fn read_clipboard() -> Option<String> {
    arboard::Clipboard::new().ok()?.get_text().ok()
}

/// Untuk clipboard monitor: kembalikan URL bila `text` adalah SATU URL http(s)
/// yang tampak sebagai berkas unduhan (ekstensi dikenali), supaya tak memicu
/// dialog pada URL halaman biasa. `None` bila bukan kandidat unduhan.
pub fn clipboard_download_url(text: &str) -> Option<String> {
    let t = text.trim();
    if !is_url(t) || t.split_whitespace().count() != 1 {
        return None;
    }
    let path = t.split(['?', '#']).next().unwrap_or("");
    let name = path.rsplit(['/', '\\']).next().unwrap_or("");
    if !name.contains('.') {
        return None;
    }
    use crate::category::Category;
    if Category::from_filename(name) != Category::General {
        return Some(t.to_string());
    }
    // Ekstensi unduhan umum di luar kategori (image disk, paket, dll).
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    const EXTRA: &[&str] = &[
        "iso", "dmg", "apk", "deb", "rpm", "appimage", "img", "torrent", "msi",
    ];
    EXTRA.contains(&ext.as_str()).then(|| t.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_urls_from_text() {
        let t = "lihat http://a.com/x.zip dan \"https://b.com/y.rar\", juga ftp://no.com";
        assert_eq!(
            extract_urls(t),
            vec!["http://a.com/x.zip".to_string(), "https://b.com/y.rar".to_string()]
        );
    }

    #[test]
    fn extract_urls_dedup() {
        let t = "http://a.com/x http://a.com/x";
        assert_eq!(extract_urls(t).len(), 1);
    }

    #[test]
    fn expand_simple_range() {
        let v = expand_pattern("http://s/f[1-3].zip");
        assert_eq!(v, vec!["http://s/f1.zip", "http://s/f2.zip", "http://s/f3.zip"]);
    }

    #[test]
    fn expand_zero_padded() {
        let v = expand_pattern("http://s/f[08-10].bin");
        assert_eq!(v, vec!["http://s/f08.bin", "http://s/f09.bin", "http://s/f10.bin"]);
    }

    #[test]
    fn expand_two_ranges_cartesian() {
        let v = expand_pattern("http://s/[1-2]/p[1-2].dat");
        assert_eq!(
            v,
            vec![
                "http://s/1/p1.dat",
                "http://s/1/p2.dat",
                "http://s/2/p1.dat",
                "http://s/2/p2.dat",
            ]
        );
    }

    #[test]
    fn expand_no_range_passthrough() {
        assert_eq!(expand_pattern("http://s/file.zip"), vec!["http://s/file.zip"]);
    }

    #[test]
    fn clipboard_url_matches_downloads_only() {
        // Berkas dikenali → terima.
        assert_eq!(
            clipboard_download_url("https://s.com/a.zip"),
            Some("https://s.com/a.zip".to_string())
        );
        assert_eq!(
            clipboard_download_url("  http://s.com/dir/setup.exe?x=1  "),
            Some("http://s.com/dir/setup.exe?x=1".to_string())
        );
        // Ekstensi ekstra (iso) → terima.
        assert!(clipboard_download_url("https://s.com/os.iso").is_some());
        // Halaman biasa / tanpa ekstensi / multi-token / non-URL → tolak.
        assert_eq!(clipboard_download_url("https://news.com/article"), None);
        assert_eq!(clipboard_download_url("https://s.com/page.html"), None);
        assert_eq!(clipboard_download_url("two https://s.com/a.zip"), None);
        assert_eq!(clipboard_download_url("just text"), None);
    }

    #[test]
    fn parse_batch_lines_and_patterns() {
        let t = "http://s/a.zip\n  \nhttp://s/f[1-2].bin\nbukan-url\n";
        assert_eq!(
            parse_batch(t),
            vec!["http://s/a.zip", "http://s/f1.bin", "http://s/f2.bin"]
        );
    }
}
