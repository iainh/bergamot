use std::time::{Duration, Instant};

use tokio::time::sleep;

#[derive(Debug)]
pub struct SpeedLimiter {
    rate: u64,
    tokens: f64,
    last_refill: Instant,
}

impl SpeedLimiter {
    pub fn new(rate_bytes_per_sec: u64) -> Self {
        Self {
            rate: rate_bytes_per_sec,
            tokens: rate_bytes_per_sec as f64,
            last_refill: Instant::now(),
        }
    }

    pub async fn acquire(&mut self, bytes: u64) {
        if self.rate == 0 {
            return;
        }

        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.last_refill = now;
        self.tokens = (self.tokens + elapsed * self.rate as f64).min(self.rate as f64 * 2.0);

        self.tokens -= bytes as f64;
        if self.tokens < 0.0 {
            let delay = Duration::from_secs_f64(-self.tokens / self.rate as f64);
            sleep(delay).await;
            self.tokens = 0.0;
            self.last_refill = Instant::now();
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
        assert_eq!(limiter.rate, 0);
    }

    #[tokio::test]
    async fn acquire_consumes_tokens() {
        let mut limiter = SpeedLimiter::new(1000);
        let before = limiter.tokens;
        limiter.acquire(500).await;
        assert!(limiter.tokens < before);
    }
}
