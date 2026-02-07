use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tokio::time::sleep;

#[derive(Debug)]
pub struct SpeedLimiter {
    rate: u64,
    rate_atomic: Arc<AtomicU64>,
    tokens: f64,
    last_refill: Instant,
}

use std::sync::Arc;

impl SpeedLimiter {
    pub fn new(rate_bytes_per_sec: u64) -> Self {
        Self {
            rate: rate_bytes_per_sec,
            rate_atomic: Arc::new(AtomicU64::new(rate_bytes_per_sec)),
            tokens: rate_bytes_per_sec as f64,
            last_refill: Instant::now(),
        }
    }

    pub fn rate(&self) -> u64 {
        self.rate
    }

    pub fn rate_ref(&self) -> &Arc<AtomicU64> {
        &self.rate_atomic
    }

    pub fn set_rate(&mut self, rate_bytes_per_sec: u64) {
        self.rate = rate_bytes_per_sec;
        self.rate_atomic.store(rate_bytes_per_sec, Ordering::Relaxed);
    }

    pub fn is_unlimited(&self) -> bool {
        self.rate_atomic.load(Ordering::Relaxed) == 0
    }

    pub fn reserve(&mut self, bytes: u64) -> Option<Duration> {
        if self.rate == 0 {
            return None;
        }

        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.last_refill = now;
        self.tokens = (self.tokens + elapsed * self.rate as f64).min(self.rate as f64 * 2.0);

        self.tokens -= bytes as f64;
        if self.tokens < 0.0 {
            let delay = Duration::from_secs_f64(-self.tokens / self.rate as f64);
            Some(delay)
        } else {
            None
        }
    }

    pub fn after_sleep(&mut self) {
        self.tokens = 0.0;
        self.last_refill = Instant::now();
    }

    pub async fn acquire(&mut self, bytes: u64) {
        if let Some(delay) = self.reserve(bytes) {
            sleep(delay).await;
            self.after_sleep();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn acquire_allows_unlimited_rate() {
        let mut limiter = SpeedLimiter::new(0);
        limiter.acquire(1024).await;
        assert_eq!(limiter.rate(), 0);
    }

    #[tokio::test]
    async fn acquire_consumes_tokens() {
        let mut limiter = SpeedLimiter::new(1000);
        let before = limiter.tokens;
        limiter.acquire(500).await;
        assert!(limiter.tokens < before);
    }

    #[tokio::test]
    async fn set_rate_changes_rate() {
        let mut limiter = SpeedLimiter::new(1000);
        assert_eq!(limiter.rate(), 1000);
        limiter.set_rate(5000);
        assert_eq!(limiter.rate(), 5000);
    }

    #[tokio::test]
    async fn set_rate_to_zero_disables_limiting() {
        let mut limiter = SpeedLimiter::new(1000);
        limiter.set_rate(0);
        limiter.acquire(1_000_000).await;
        assert_eq!(limiter.rate(), 0);
    }
}
