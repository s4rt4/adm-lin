//! Token-bucket speed limiter (plan §7). Rate live-adjustable (atomik) agar
//! bisa diubah saat unduhan berjalan; `0` = tanpa batas. Dipakai sebagai
//! limiter per-unduhan maupun global (shared via `Arc`).

use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::Mutex;
use tokio::time::{Duration, Instant};

pub struct Limiter {
    /// byte per detik; `0` = tanpa batas.
    rate: AtomicU64,
    bucket: Mutex<Bucket>,
}

struct Bucket {
    tokens: f64,
    last: Instant,
}

impl Limiter {
    pub fn new(rate_bps: u64) -> Self {
        Self {
            rate: AtomicU64::new(rate_bps),
            bucket: Mutex::new(Bucket {
                tokens: 0.0,
                last: Instant::now(),
            }),
        }
    }

    pub fn unlimited() -> Self {
        Self::new(0)
    }

    /// Ubah batas (byte/detik) saat berjalan; `0` = tanpa batas.
    pub fn set_rate(&self, bps: u64) {
        self.rate.store(bps, Ordering::Relaxed);
    }

    pub fn rate(&self) -> u64 {
        self.rate.load(Ordering::Relaxed)
    }

    /// Tahan sampai `n` byte boleh ditransfer pada rate saat ini.
    pub async fn acquire(&self, n: usize) {
        loop {
            let rate = self.rate.load(Ordering::Relaxed);
            if rate == 0 {
                return; // tanpa batas
            }
            let rate_f = rate as f64;
            let capacity = rate_f.max(64.0 * 1024.0);
            let need = n as f64;
            let wait = {
                let mut b = self.bucket.lock().await;
                let now = Instant::now();
                let elapsed = now.duration_since(b.last).as_secs_f64();
                b.last = now;
                b.tokens = (b.tokens + elapsed * rate_f).min(capacity);
                if b.tokens >= need {
                    b.tokens -= need;
                    return;
                }
                Duration::from_secs_f64(((need - b.tokens) / rate_f).min(1.0))
            };
            tokio::time::sleep(wait).await;
        }
    }
}
