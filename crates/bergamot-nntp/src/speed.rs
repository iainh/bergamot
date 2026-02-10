use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, oneshot};
use tokio::time::sleep;

#[derive(Debug)]
pub struct SpeedLimiter {
    rate: u64,
    rate_atomic: Arc<AtomicU64>,
    tokens: f64,
    last_refill: Instant,
}

enum LimiterCommand {
    Acquire {
        bytes: u64,
        reply: oneshot::Sender<Option<Duration>>,
    },
    SetRate {
        rate: u64,
        reply: oneshot::Sender<()>,
    },
}

#[derive(Clone)]
pub struct SpeedLimiterHandle {
    tx: mpsc::Sender<LimiterCommand>,
    rate_atomic: Arc<AtomicU64>,
}

impl SpeedLimiterHandle {
    pub fn new(rate_bytes_per_sec: u64) -> Self {
        let rate_atomic = Arc::new(AtomicU64::new(rate_bytes_per_sec));
        let (tx, rx) = mpsc::channel(256);
        let atomic_clone = rate_atomic.clone();
        tokio::spawn(Self::run(rx, rate_bytes_per_sec, atomic_clone));
        Self { tx, rate_atomic }
    }

    async fn run(
        mut rx: mpsc::Receiver<LimiterCommand>,
        initial_rate: u64,
        rate_atomic: Arc<AtomicU64>,
    ) {
        let mut limiter = SpeedLimiter::new_internal(initial_rate);
        while let Some(cmd) = rx.recv().await {
            match cmd {
                LimiterCommand::Acquire { bytes, reply } => {
                    let delay = limiter.reserve(bytes);
                    let _ = reply.send(delay);
                }
                LimiterCommand::SetRate { rate, reply } => {
                    limiter.rate = rate;
                    rate_atomic.store(rate, Ordering::Relaxed);
                    let _ = reply.send(());
                }
            }
        }
    }

    pub async fn acquire(&self, bytes: u64) {
        if self.is_unlimited() {
            return;
        }
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .tx
            .send(LimiterCommand::Acquire {
                bytes,
                reply: reply_tx,
            })
            .await
            .is_err()
        {
            return;
        }
        if let Ok(Some(delay)) = reply_rx.await {
            sleep(delay).await;
        }
    }

    pub async fn set_rate(&self, rate: u64) {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .tx
            .send(LimiterCommand::SetRate {
                rate,
                reply: reply_tx,
            })
            .await
            .is_err()
        {
            return;
        }
        let _ = reply_rx.await;
    }

    pub fn is_unlimited(&self) -> bool {
        self.rate_atomic.load(Ordering::Relaxed) == 0
    }
}

impl SpeedLimiter {
    fn new_internal(rate_bytes_per_sec: u64) -> Self {
        Self {
            rate: rate_bytes_per_sec,
            rate_atomic: Arc::new(AtomicU64::new(rate_bytes_per_sec)),
            tokens: rate_bytes_per_sec as f64,
            last_refill: Instant::now(),
        }
    }

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
        self.rate_atomic
            .store(rate_bytes_per_sec, Ordering::Relaxed);
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

    #[tokio::test]
    async fn handle_acquire_unlimited_returns_immediately() {
        let handle = SpeedLimiterHandle::new(0);
        handle.acquire(1024).await;
    }

    #[tokio::test]
    async fn handle_acquire_within_budget_returns_immediately() {
        let handle = SpeedLimiterHandle::new(1_000_000);
        let start = Instant::now();
        handle.acquire(100).await;
        assert!(start.elapsed() < Duration::from_millis(50));
    }

    #[tokio::test]
    async fn handle_set_rate_to_zero_disables_limiting() {
        let handle = SpeedLimiterHandle::new(100);
        handle.set_rate(0).await;
        let start = Instant::now();
        handle.acquire(1_000_000).await;
        assert!(start.elapsed() < Duration::from_millis(50));
    }

    #[tokio::test]
    async fn handle_is_unlimited_reflects_rate() {
        let handle = SpeedLimiterHandle::new(1000);
        assert!(!handle.is_unlimited());
        let handle_zero = SpeedLimiterHandle::new(0);
        assert!(handle_zero.is_unlimited());
    }

    #[tokio::test]
    async fn handle_concurrent_acquires_do_not_panic() {
        let handle = SpeedLimiterHandle::new(1_000_000);
        let mut tasks = Vec::new();
        for _ in 0..50 {
            let h = handle.clone();
            tasks.push(tokio::spawn(async move {
                h.acquire(100).await;
            }));
        }
        for t in tasks {
            t.await.unwrap();
        }
    }
}
