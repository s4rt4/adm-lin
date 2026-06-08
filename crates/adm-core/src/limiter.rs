//! Token-bucket speed limiter (plan §7). Global; per-file menyusul di WM6.

use tokio::sync::Mutex;
use tokio::time::{Duration, Instant};

pub struct Limiter {
    inner: Option<Mutex<Bucket>>,
}

struct Bucket {
    /// byte per detik.
    rate: f64,
    /// kapasitas burst (byte).
    capacity: f64,
    tokens: f64,
    last: Instant,
}

impl Limiter {
    /// Tanpa batas.
    pub fn unlimited() -> Self {
        Self { inner: None }
    }

    /// Batasi ke `rate_bps` byte/detik. `0` atau `None` => tanpa batas.
    pub fn new(rate_bps: Option<u64>) -> Self {
        match rate_bps {
            Some(r) if r > 0 => {
                let rate = r as f64;
                Self {
                    inner: Some(Mutex::new(Bucket {
                        rate,
                        // burst = 1 detik trafik (min 64 KiB).
                        capacity: rate.max(64.0 * 1024.0),
                        tokens: rate,
                        last: Instant::now(),
                    })),
                }
            }
            _ => Self::unlimited(),
        }
    }

    /// Tahan sampai `n` byte boleh ditransfer.
    pub async fn acquire(&self, n: usize) {
        let Some(m) = &self.inner else { return };
        let need = n as f64;
        loop {
            let wait = {
                let mut b = m.lock().await;
                let now = Instant::now();
                let elapsed = now.duration_since(b.last).as_secs_f64();
                b.last = now;
                b.tokens = (b.tokens + elapsed * b.rate).min(b.capacity);
                if b.tokens >= need {
                    b.tokens -= need;
                    return;
                }
                let deficit = need - b.tokens;
                Duration::from_secs_f64((deficit / b.rate).min(1.0))
            };
            tokio::time::sleep(wait).await;
        }
    }
}
