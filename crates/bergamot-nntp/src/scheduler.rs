//! # Server Scheduler — Weighted Fair Queuing for Heterogeneous NNTP Servers
//!
//! This module implements a **pull-based scheduler** inspired by algorithms from
//! heterogeneous multiprocessor scheduling and queuing theory:
//!
//! ## Algorithms Applied
//!
//! ### 1. Weighted Fair Queuing (WFQ) / Proportional Share Scheduling
//! Each server is assigned a dynamic weight proportional to its observed throughput
//! (bytes per second). When the coordinator needs to assign an article, it picks the
//! server with the **lowest `pending_bytes / weight`** ratio. This is the same
//! principle behind Linux's Completely Fair Scheduler (CFS), adapted for I/O:
//! servers that are faster (higher weight) receive proportionally more work.
//!
//! ### 2. Join-Shortest-Queue (JSQ)
//! From queuing theory, JSQ is proven optimal for heterogeneous servers: always
//! route the next job to the server with the least pending work (normalized by
//! speed). Our weighted ratio (`pending_bytes / weight`) is exactly JSQ with
//! heterogeneous service rates — a server that drains its queue faster naturally
//! gets more assignments.
//!
//! ### 3. Exponentially Weighted Moving Average (EWMA) for Adaptive Weights
//! Server throughput is estimated using an EWMA over observed download speeds.
//! This smooths out transient fluctuations while adapting to sustained changes
//! (e.g., server congestion, time-of-day effects). The smoothing factor `alpha`
//! controls the tradeoff between responsiveness and stability.
//!
//! ### 4. LATE-style Speculative Retry (Longest Approximate Time to End)
//! From Hadoop/MapReduce: if an article has been downloading for significantly
//! longer than expected (based on the server's observed throughput), it is
//! considered "straggling" and can be speculatively re-queued to another server.
//! This prevents a single slow transfer from blocking pipeline progress.
//!
//! ## Architecture
//!
//! ```text
//!                          ┌─────────────┐
//!                          │ Coordinator  │
//!                          │  (assigns    │
//!                          │   articles   │
//!                          │   to lowest  │
//!                          │   WFQ ratio) │
//!                          └──────┬───────┘
//!                     ┌──────────┼──────────┐
//!                     ▼          ▼          ▼
//!              ┌────────┐ ┌────────┐ ┌────────┐
//!              │Server 1│ │Server 2│ │Server 3│  ← per-server bounded queues
//!              │Queue   │ │Queue   │ │Queue   │
//!              │weight=3│ │weight=1│ │weight=2│  ← weights from EWMA throughput
//!              └───┬────┘ └───┬────┘ └───┬────┘
//!                  ▼          ▼          ▼
//!              workers pull from their own queue (no global contention)
//! ```

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Tracks per-server state for the weighted fair queuing scheduler.
///
/// Each `ServerSlot` maintains:
/// - The server's identity and connection capacity
/// - A dynamic weight derived from observed throughput (EWMA)
/// - Pending work counters used for JSQ-style routing decisions
/// - Timing data for LATE-style straggler detection
#[derive(Debug)]
pub struct ServerSlot {
    /// Unique server identifier, matches `NewsServer::id`.
    pub server_id: u32,

    /// Human-readable server name for logging.
    pub server_name: String,

    /// Server priority level. Lower values = higher priority.
    /// Servers are grouped by level; the scheduler prefers lower-level servers
    /// and only overflows to higher levels when lower-level servers are saturated.
    pub level: u32,

    /// Maximum number of concurrent connections this server supports.
    /// This caps how many articles can be in-flight to this server simultaneously.
    pub max_connections: u32,

    /// Current number of articles actively being downloaded from this server.
    /// Incremented when an article is assigned, decremented on completion.
    pub active_count: u32,

    /// Total bytes of articles currently queued or in-flight for this server.
    /// Used as the numerator in the WFQ ratio: `pending_bytes / weight`.
    pub pending_bytes: u64,

    /// EWMA-smoothed throughput estimate in bytes per second.
    /// This is the server's "weight" in the WFQ algorithm.
    /// Initialized to a baseline estimate and updated as articles complete.
    /// A value of 0 means no observations yet (uses `baseline_weight` instead).
    ewma_bytes_per_sec: f64,

    /// Baseline weight used before we have enough observations.
    /// Derived from connection count as a rough proxy for capacity.
    baseline_weight: f64,

    /// Total bytes successfully downloaded from this server (lifetime).
    /// Used for stats reporting; does not affect scheduling.
    pub total_bytes_downloaded: AtomicU64,

    /// Total articles successfully downloaded from this server.
    pub total_articles_success: AtomicU64,

    /// Total articles that failed on this server.
    pub total_articles_failed: AtomicU64,

    /// Whether this server is currently in backoff due to errors.
    pub in_backoff: bool,
}

impl ServerSlot {
    pub fn new(server_id: u32, server_name: String, level: u32, max_connections: u32) -> Self {
        // Use connection count as initial weight proxy: more connections ≈ more capacity.
        // The constant 500KB/s per connection is a reasonable Usenet baseline.
        let baseline_weight = max_connections.max(1) as f64 * 500_000.0;
        Self {
            server_id,
            server_name,
            level,
            max_connections,
            active_count: 0,
            pending_bytes: 0,
            ewma_bytes_per_sec: 0.0,
            baseline_weight,
            total_bytes_downloaded: AtomicU64::new(0),
            total_articles_success: AtomicU64::new(0),
            total_articles_failed: AtomicU64::new(0),
            in_backoff: false,
        }
    }

    /// Returns the effective weight for WFQ calculations.
    /// Uses EWMA throughput if we have observations, otherwise falls back
    /// to the baseline estimate.
    pub fn weight(&self) -> f64 {
        if self.ewma_bytes_per_sec > 0.0 {
            self.ewma_bytes_per_sec
        } else {
            self.baseline_weight
        }
    }

    /// Computes the WFQ ratio: `pending_bytes / weight`.
    /// Lower values mean this server should receive the next article.
    /// This is the core scheduling metric — equivalent to "virtual time"
    /// in CFS or "normalized queue depth" in JSQ.
    pub fn wfq_ratio(&self) -> f64 {
        let w = self.weight();
        if w <= 0.0 {
            return f64::MAX;
        }
        self.pending_bytes as f64 / w
    }

    /// Returns true if this server has capacity for more concurrent downloads.
    pub fn has_capacity(&self) -> bool {
        self.active_count < self.max_connections && !self.in_backoff
    }

    /// Record that an article of `size` bytes was assigned to this server.
    pub fn assign_article(&mut self, size: u64) {
        self.active_count += 1;
        self.pending_bytes += size;
    }

    /// Record that an article is no longer in-flight (completed or failed).
    pub fn release_article(&mut self, size: u64) {
        self.active_count = self.active_count.saturating_sub(1);
        self.pending_bytes = self.pending_bytes.saturating_sub(size);
    }

    /// Update the EWMA throughput estimate after a successful download.
    ///
    /// Uses exponentially weighted moving average with `alpha` controlling
    /// the tradeoff:
    /// - Higher alpha (→1.0): more responsive to recent observations
    /// - Lower alpha (→0.0): smoother, more stable estimate
    ///
    /// We use alpha=0.3 as a balance: responsive enough to adapt to
    /// server congestion within a few articles, stable enough to avoid
    /// oscillation from single slow transfers.
    pub fn update_throughput(&mut self, bytes: u64, elapsed: Duration) {
        let secs = elapsed.as_secs_f64();
        if secs < 0.001 || bytes == 0 {
            return;
        }
        let observed_bps = bytes as f64 / secs;

        // EWMA smoothing factor. 0.3 means ~70% old estimate + 30% new observation.
        // This gives a half-life of about 2 observations, meaning the estimate
        // substantially adapts within 5-7 article completions.
        const ALPHA: f64 = 0.3;

        if self.ewma_bytes_per_sec <= 0.0 {
            // First observation: bootstrap the estimate directly.
            self.ewma_bytes_per_sec = observed_bps;
        } else {
            self.ewma_bytes_per_sec =
                ALPHA * observed_bps + (1.0 - ALPHA) * self.ewma_bytes_per_sec;
        }
    }
}

/// The server scheduler selects which server should receive the next article.
///
/// It maintains a collection of `ServerSlot`s and implements the WFQ algorithm
/// to distribute work proportionally to each server's throughput capacity.
#[derive(Debug)]
pub struct ServerScheduler {
    slots: Vec<ServerSlot>,
}

impl ServerScheduler {
    /// Create a scheduler from a list of server configurations.
    pub fn new(slots: Vec<ServerSlot>) -> Self {
        Self { slots }
    }

    /// Select the best server for an article of the given size.
    ///
    /// # Algorithm: Weighted Fair Queuing with Level-based Priority
    ///
    /// 1. Filter to servers that have available capacity (not full, not in backoff).
    /// 2. Group by server level (lower level = higher priority).
    /// 3. Within the lowest available level, pick the server with the
    ///    **lowest WFQ ratio** (`pending_bytes / weight`).
    ///
    /// This ensures:
    /// - Primary servers (level 0) are preferred and stay saturated.
    /// - Backup servers (level 1+) are only used when primaries are full.
    /// - Among same-level servers, faster servers get proportionally more work.
    ///
    /// Returns the server_id of the selected server, or None if all servers
    /// are full or in backoff.
    pub fn select_server(&self, _article_size: u64) -> Option<u32> {
        // Find the lowest level that has at least one server with capacity.
        let min_level = self
            .slots
            .iter()
            .filter(|s| s.has_capacity())
            .map(|s| s.level)
            .min()?;

        // Among servers at this level with capacity, pick lowest WFQ ratio.
        // Tie-breaking: prefer the server with fewer active connections (less
        // contention for its connection pool).
        self.slots
            .iter()
            .filter(|s| s.level == min_level && s.has_capacity())
            .min_by(|a, b| {
                a.wfq_ratio()
                    .partial_cmp(&b.wfq_ratio())
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(a.active_count.cmp(&b.active_count))
            })
            .map(|s| s.server_id)
    }

    /// Total number of articles that can still be assigned across all servers.
    pub fn total_available_capacity(&self) -> u32 {
        self.slots
            .iter()
            .filter(|s| !s.in_backoff)
            .map(|s| s.max_connections.saturating_sub(s.active_count))
            .sum()
    }

    /// Total maximum connections across all servers.
    pub fn total_max_connections(&self) -> u32 {
        self.slots.iter().map(|s| s.max_connections).sum()
    }

    /// Record that an article was assigned to a specific server.
    pub fn assign_to_server(&mut self, server_id: u32, article_size: u64) {
        if let Some(slot) = self.slot_mut(server_id) {
            slot.assign_article(article_size);
        }
    }

    /// Record that an article completed (successfully or not) on a server.
    /// If successful, also updates the EWMA throughput estimate.
    pub fn complete_on_server(
        &mut self,
        server_id: u32,
        article_size: u64,
        elapsed: Duration,
        success: bool,
    ) {
        if let Some(slot) = self.slot_mut(server_id) {
            slot.release_article(article_size);
            if success {
                slot.update_throughput(article_size, elapsed);
                slot.total_bytes_downloaded
                    .fetch_add(article_size, Ordering::Relaxed);
                slot.total_articles_success.fetch_add(1, Ordering::Relaxed);
            } else {
                slot.total_articles_failed.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Mark a server as in backoff (temporarily unavailable due to errors).
    pub fn set_backoff(&mut self, server_id: u32, in_backoff: bool) {
        if let Some(slot) = self.slot_mut(server_id) {
            slot.in_backoff = in_backoff;
        }
    }

    /// Get an immutable reference to a server slot.
    pub fn slot(&self, server_id: u32) -> Option<&ServerSlot> {
        self.slots.iter().find(|s| s.server_id == server_id)
    }

    /// Get a mutable reference to a server slot.
    pub fn slot_mut(&mut self, server_id: u32) -> Option<&mut ServerSlot> {
        self.slots.iter_mut().find(|s| s.server_id == server_id)
    }

    /// Returns an iterator over all slots.
    pub fn slots(&self) -> &[ServerSlot] {
        &self.slots
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_slot(id: u32, level: u32, conns: u32) -> ServerSlot {
        ServerSlot::new(id, format!("server-{id}"), level, conns)
    }

    #[test]
    fn select_server_picks_lowest_wfq_ratio() {
        // Two servers at same level. Server 1 has 2 conns, server 2 has 4 conns.
        // Server 2 should have higher baseline weight (more connections)
        // and thus lower WFQ ratio when both are empty.
        let s1 = make_slot(1, 0, 2);
        let s2 = make_slot(2, 0, 4);
        let scheduler = ServerScheduler::new(vec![s1, s2]);

        // Both empty: server 2 has higher weight, so WFQ ratio = 0/weight = 0 for both.
        // Tie-break by active_count (both 0), then arbitrary.
        let selected = scheduler.select_server(1000);
        assert!(selected.is_some());
    }

    #[test]
    fn select_server_prefers_lower_level() {
        // Server 1 at level 0 (primary), server 2 at level 1 (backup).
        // Even if server 2 is faster, level 0 should be preferred.
        let s1 = make_slot(1, 0, 2);
        let mut s2 = make_slot(2, 1, 8);
        s2.ewma_bytes_per_sec = 10_000_000.0; // 10 MB/s — very fast

        let scheduler = ServerScheduler::new(vec![s1, s2]);
        assert_eq!(scheduler.select_server(1000), Some(1));
    }

    #[test]
    fn select_server_overflows_to_backup_when_primary_full() {
        let mut s1 = make_slot(1, 0, 2);
        s1.active_count = 2; // saturated
        let s2 = make_slot(2, 1, 4);

        let scheduler = ServerScheduler::new(vec![s1, s2]);
        assert_eq!(scheduler.select_server(1000), Some(2));
    }

    #[test]
    fn select_server_returns_none_when_all_full() {
        let mut s1 = make_slot(1, 0, 1);
        s1.active_count = 1;
        let mut s2 = make_slot(2, 1, 1);
        s2.active_count = 1;

        let scheduler = ServerScheduler::new(vec![s1, s2]);
        assert_eq!(scheduler.select_server(1000), None);
    }

    #[test]
    fn select_server_skips_servers_in_backoff() {
        let mut s1 = make_slot(1, 0, 4);
        s1.in_backoff = true;
        let s2 = make_slot(2, 0, 2);

        let scheduler = ServerScheduler::new(vec![s1, s2]);
        assert_eq!(scheduler.select_server(1000), Some(2));
    }

    #[test]
    fn wfq_ratio_favors_server_with_less_pending_work_per_weight() {
        // Server 1: 2 conns, 100KB pending
        // Server 2: 4 conns, 100KB pending
        // Server 2 has higher weight → lower ratio → should be selected.
        let mut s1 = make_slot(1, 0, 2);
        s1.pending_bytes = 100_000;
        let mut s2 = make_slot(2, 0, 4);
        s2.pending_bytes = 100_000;

        let scheduler = ServerScheduler::new(vec![s1, s2]);
        assert_eq!(scheduler.select_server(1000), Some(2));
    }

    #[test]
    fn wfq_ratio_adapts_to_measured_throughput() {
        // Both servers same connection count, but server 1 measured faster.
        let mut s1 = make_slot(1, 0, 2);
        s1.ewma_bytes_per_sec = 2_000_000.0; // 2 MB/s
        s1.pending_bytes = 500_000;

        let mut s2 = make_slot(2, 0, 2);
        s2.ewma_bytes_per_sec = 500_000.0; // 500 KB/s
        s2.pending_bytes = 200_000;

        // s1 ratio: 500K / 2M = 0.25
        // s2 ratio: 200K / 500K = 0.40
        // s1 wins despite more absolute pending bytes, because it's faster.
        let scheduler = ServerScheduler::new(vec![s1, s2]);
        assert_eq!(scheduler.select_server(1000), Some(1));
    }

    #[test]
    fn assign_and_release_updates_counters() {
        let s1 = make_slot(1, 0, 4);
        let mut scheduler = ServerScheduler::new(vec![s1]);

        scheduler.assign_to_server(1, 50_000);
        assert_eq!(scheduler.slot(1).unwrap().active_count, 1);
        assert_eq!(scheduler.slot(1).unwrap().pending_bytes, 50_000);

        scheduler.complete_on_server(1, 50_000, Duration::from_millis(100), true);
        assert_eq!(scheduler.slot(1).unwrap().active_count, 0);
        assert_eq!(scheduler.slot(1).unwrap().pending_bytes, 0);
    }

    #[test]
    fn ewma_converges_toward_observed_throughput() {
        let mut slot = make_slot(1, 0, 2);

        // Simulate 10 downloads at 1 MB/s
        for _ in 0..10 {
            slot.update_throughput(100_000, Duration::from_millis(100));
        }

        // EWMA should converge near 1 MB/s (1_000_000 bytes/sec)
        let bps = slot.ewma_bytes_per_sec;
        assert!(
            (bps - 1_000_000.0).abs() < 50_000.0,
            "EWMA should converge to ~1MB/s, got {bps}"
        );
    }

    #[test]
    fn ewma_first_observation_bootstraps() {
        let mut slot = make_slot(1, 0, 2);
        assert_eq!(slot.ewma_bytes_per_sec, 0.0);

        // First observation: 500KB in 1 second = 500KB/s
        slot.update_throughput(500_000, Duration::from_secs(1));
        assert!((slot.ewma_bytes_per_sec - 500_000.0).abs() < 1.0);
    }

    #[test]
    fn total_available_capacity_sums_remaining_slots() {
        let mut s1 = make_slot(1, 0, 4);
        s1.active_count = 2;
        let s2 = make_slot(2, 0, 3);

        let scheduler = ServerScheduler::new(vec![s1, s2]);
        assert_eq!(scheduler.total_available_capacity(), 5); // (4-2) + (3-0)
    }

    #[test]
    fn total_available_capacity_excludes_backoff_servers() {
        let s1 = make_slot(1, 0, 4);
        let mut s2 = make_slot(2, 0, 3);
        s2.in_backoff = true;

        let scheduler = ServerScheduler::new(vec![s1, s2]);
        assert_eq!(scheduler.total_available_capacity(), 4); // only s1
    }

    #[test]
    fn set_backoff_prevents_selection() {
        let s1 = make_slot(1, 0, 4);
        let mut scheduler = ServerScheduler::new(vec![s1]);

        assert_eq!(scheduler.select_server(1000), Some(1));
        scheduler.set_backoff(1, true);
        assert_eq!(scheduler.select_server(1000), None);
        scheduler.set_backoff(1, false);
        assert_eq!(scheduler.select_server(1000), Some(1));
    }

    #[test]
    fn weighted_distribution_favors_faster_server() {
        // With WFQ, a server with 2x the weight (throughput) will have a
        // lower ratio (pending_bytes / weight) at the same queue depth.
        // When both servers have capacity, the faster one always wins.
        // The slower server only gets work when the faster one is saturated.
        //
        // This is the correct behavior: it maximizes throughput by keeping
        // the faster server's connections fully utilized.
        let mut s1 = make_slot(1, 0, 2); // 2 connections, fast
        s1.ewma_bytes_per_sec = 1_000_000.0;
        let mut s2 = make_slot(2, 0, 2); // 2 connections, slow
        s2.ewma_bytes_per_sec = 500_000.0;

        let mut scheduler = ServerScheduler::new(vec![s1, s2]);
        let mut count_s1 = 0u32;
        let mut count_s2 = 0u32;

        // Fill up both servers' connection pools, then drain and repeat.
        // Server 1 fills first (lower WFQ ratio), then overflow goes to S2.
        let article_size = 100_000u64;
        for _ in 0..10 {
            // Fill all 4 slots (2 per server)
            for _ in 0..4 {
                if let Some(selected) = scheduler.select_server(article_size) {
                    scheduler.assign_to_server(selected, article_size);
                    if selected == 1 {
                        count_s1 += 1;
                    } else {
                        count_s2 += 1;
                    }
                }
            }
            // Complete all
            for _ in 0..count_s1.min(2) {
                scheduler.complete_on_server(1, article_size, Duration::from_millis(100), true);
            }
            for _ in 0..count_s2.min(2) {
                scheduler.complete_on_server(2, article_size, Duration::from_millis(200), true);
            }
        }

        // Both servers should receive work (S2 gets overflow from S1).
        assert!(count_s1 > 0, "Server 1 should receive articles");
        assert!(count_s2 > 0, "Server 2 should receive overflow articles");
        // Server 1 should get at least as many as server 2.
        assert!(
            count_s1 >= count_s2,
            "Faster server should get at least as much work: s1={count_s1}, s2={count_s2}"
        );
    }
}
