//! Verifikasi checksum berkas unduhan (gaya IDM "verify download"). Hitung
//! MD5 / SHA-1 / SHA-256 secara streaming (hemat memori) lalu bandingkan dengan
//! nilai harapan user. Dipanggil di thread latar setelah unduhan selesai.

use digest::Digest;
use std::io::Read;
use std::path::Path;

/// Algoritma checksum yang didukung dialog Add.
#[derive(Clone, Copy, PartialEq)]
pub enum Algo {
    Md5,
    Sha1,
    Sha256,
}

impl Algo {
    pub fn label(self) -> &'static str {
        match self {
            Algo::Md5 => "MD5",
            Algo::Sha1 => "SHA-1",
            Algo::Sha256 => "SHA-256",
        }
    }

    /// Kunci stabil untuk disisipkan pada `expected_sum` ("md5"/"sha1"/"sha256").
    pub fn key(self) -> &'static str {
        match self {
            Algo::Md5 => "md5",
            Algo::Sha1 => "sha1",
            Algo::Sha256 => "sha256",
        }
    }

    fn from_key(s: &str) -> Algo {
        match s {
            "md5" => Algo::Md5,
            "sha1" => Algo::Sha1,
            _ => Algo::Sha256,
        }
    }

    /// Tebak algoritma dari panjang hex (32=MD5, 40=SHA-1, selain itu SHA-256).
    fn from_hex_len(hex: &str) -> Algo {
        match hex.trim().len() {
            32 => Algo::Md5,
            40 => Algo::Sha1,
            _ => Algo::Sha256,
        }
    }
}

/// Format nilai harapan tersimpan: `"<algo>:<hex>"` (mis. `"sha256:ab…"`).
pub fn encode_expected(algo: Algo, hex: &str) -> String {
    format!("{}:{}", algo.key(), hex.trim().to_ascii_lowercase())
}

/// Pisah `expected` jadi (algo, hex). Tanpa prefix → algoritma ditebak dari
/// panjang hex (memudahkan tempel hash mentah).
fn split(expected: &str) -> (Algo, &str) {
    if let Some((a, h)) = expected.split_once(':') {
        (Algo::from_key(a), h.trim())
    } else {
        let h = expected.trim();
        (Algo::from_hex_len(h), h)
    }
}

/// Hitung hash `path` lalu cocokkan (case-insensitive) dgn `expected`.
/// `false` bila berkas tak terbaca atau hash tak cocok.
pub fn verify(path: &Path, expected: &str) -> bool {
    let (algo, want) = split(expected);
    match compute(path, algo) {
        Some(got) => got.eq_ignore_ascii_case(want),
        None => false,
    }
}

fn compute(path: &Path, algo: Algo) -> Option<String> {
    match algo {
        Algo::Md5 => digest_file::<md5::Md5>(path),
        Algo::Sha1 => digest_file::<sha1::Sha1>(path),
        Algo::Sha256 => digest_file::<sha2::Sha256>(path),
    }
}

fn digest_file<D: Digest>(path: &Path) -> Option<String> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut hasher = D::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Some(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn verifies_known_sha256_and_md5() {
        // Berkas berisi "abc".
        let dir = std::env::temp_dir().join("adm-checksum-test");
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("abc.txt");
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(b"abc").unwrap();
        drop(f);
        // Hash referensi "abc".
        let sha = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let md5 = "900150983cd24fb0d6963f7d28e17f72";
        assert!(verify(&p, &format!("sha256:{sha}")));
        assert!(verify(&p, &format!("md5:{md5}")));
        // Tanpa prefix → algoritma ditebak dari panjang.
        assert!(verify(&p, sha));
        assert!(verify(&p, md5));
        // Tidak cocok / berkas absen.
        assert!(!verify(&p, "sha256:deadbeef"));
        assert!(!verify(&dir.join("nope.txt"), md5));
    }
}
