//! Operasi file spesifik-OS di balik abstraksi `cfg` (plan §7).
//!
//! Windows: positioned write via `seek_write` (mirip `pwrite`), pre-alokasi
//! via `set_len` (semantik `SetEndOfFile`). Impl Unix disertakan agar engine
//! tetap portabel bila dikompilasi di luar Windows (mis. test CI Linux).

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

/// Buat/pastikan file ada dan berukuran `size` byte (pre-alokasi).
///
/// Windows: `File::set_len` memanggil `SetEndOfFile`; ruang terisi nol secara
/// lazy (valid-data length). `SetFileValidData` (opsional, butuh privilege)
/// sengaja tidak dipakai — lihat plan §7.
pub fn preallocate(path: &Path, size: u64) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    // truncate(false): JANGAN kosongkan isi — resume bergantung pada data lama.
    let f = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    if f.metadata()?.len() != size {
        f.set_len(size)?;
    }
    Ok(())
}

/// Buka handle tulis untuk satu koneksi (handle terpisah per segmen agar
/// `seek_write` tidak balapan pada cursor handle yang sama).
pub fn open_writer(path: &Path) -> io::Result<File> {
    OpenOptions::new().write(true).open(path)
}

/// Tulis seluruh `buf` pada `offset` absolut (positioned write).
#[cfg(windows)]
pub fn write_at(file: &File, buf: &[u8], offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    let mut written = 0usize;
    while written < buf.len() {
        let n = file.seek_write(&buf[written..], offset + written as u64)?;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::WriteZero, "seek_write 0 byte"));
        }
        written += n;
    }
    Ok(())
}

/// Varian Unix (untuk portabilitas/test non-Windows).
#[cfg(unix)]
pub fn write_at(file: &File, buf: &[u8], offset: u64) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.write_all_at(buf, offset)
}
