# Connection Scaling Bottlenecks

When increasing the connection count for both servers from 15 to 50 (100 total), overall download speed drops. This document captures the identified bottlenecks and recommended fixes.

## Root Causes

### 1. `try_acquire_owned()` fails instantly when busy (Critical)

**File:** `crates/nzbg-nntp/src/pool.rs` (line 92)

When both servers' semaphores have 0 permits available, `fetch_article` returns `"no servers available"` immediately instead of waiting. At 100 total connections, this happens frequently — tasks fail in tight loops, creating churn and wasted work instead of waiting for a permit.

The sequential server iteration with a fixed order also biases load toward the first server. Under pressure, many tasks hammer server #1 first and only spill to #2 if #1 is saturated and #2 has permits at that exact instant.

### 2. Coordinator polls at 100ms intervals

**File:** `crates/nzbg-queue/src/coordinator.rs` (line 333)

Slots refill only every 100ms. At high concurrency, segments complete faster than the scheduler reacts, leaving connections idle between polling ticks. This creates a sawtooth pattern where slots drain quickly but refill slowly.

### 3. Assignment channel capacity (64) < total connections (100)

**File:** `crates/nzbg-queue/src/coordinator.rs` (line 299)

`try_fill_download_slots` uses `try_send` and stops dispatching when the channel is full, underutilizing available connections. The coordinator also can't refill until the next 100ms tick, compounding the problem.

### 4. SpeedLimiter global Mutex

**File:** `crates/nzbg/src/download.rs` (lines 51–86)

Every spawned task locks `Arc<Mutex<SpeedLimiter>>` twice per segment (once for `reserve()`, once for `after_sleep()`). When rate limiting is enabled, this serializes the start of all 100 downloads through a single mutex, causing convoy effects and delaying NNTP commands while connections sit idle.

### 5. Blocking file I/O in write_segment

**File:** `crates/nzbg/src/writer.rs` (lines 25–33)

Every segment write performs:
- `try_clone().await` (extra syscall)
- `into_std().await` (conversion boundary)
- `std_file.metadata()?.len()` (blocking sync syscall on a Tokio worker thread)
- `std_file.set_len(target_len)?` (blocking sync syscall)

All of this happens while holding a per-file mutex. This blocks Tokio worker threads and serializes all writes to the same output file. Since tasks don't report completion until the write finishes, network download scheduling is throttled by disk write serialization.

### 6. Excessive memory copies

**Files:** `crates/nzbg-nntp/src/pool.rs` (lines 148–154), `crates/nzbg/src/download.rs` (lines 37, 101)

Each article body goes through multiple full-buffer copies:
1. Lines assembled and joined in `ServerPool` (`body_lines.join`)
2. Split and cloned again in `NntpPoolFetcher::fetch_body`
3. Joined again for the article cache
4. `data.clone()` in the `DownloadResult` success path

At 100 concurrent tasks this creates superlinear memory and CPU pressure, saturating allocator throughput and memory bandwidth.

### 7. BoundedCache uses std::sync::Mutex

**File:** `crates/nzbg/src/cache.rs`

`BoundedCache::get/put` uses a blocking `std::sync::Mutex`. Under Tokio, blocking a runtime worker thread on contention reduces the executor's ability to drive sockets. The `.cloned()` in `get()` also copies the entire cached article buffer, amplifying the cost at scale.

### 8. ServerPool per-server locks

**File:** `crates/nzbg-nntp/src/pool.rs`

Per server, every fetch touches:
- `idle_connections: Mutex<Vec<...>>` (lock on acquire and return)
- `backoff_until: Mutex<Option<Instant>>` (lock per server per request)
- `fail_count: Mutex<u32>` (lock on failure/success)

With 100 concurrent tasks these small locks become noticeably contended, especially the idle pool `Vec` lock.

## Recommended Fixes (by priority)

### 1. Fix ServerPool permit acquisition to wait instead of failing

Replace `try_acquire_owned()` with an awaitable strategy so tasks wait for a permit instead of returning an error. For example, `acquire_owned().await` on the best server, or a round-robin strategy with fallback.

### 2. Make slot-fill event-driven

Trigger `try_fill_download_slots` immediately on `DownloadComplete` instead of relying on a 100ms polling interval. Alternatively, reduce the interval to 5–10ms as a stopgap.

### 3. Increase assignment channel capacity

Set the channel capacity to at least `total_connections` (e.g., 256 or 512) to avoid artificial underfill.

### 4. Pre-allocate output files

Use NZB metadata to pre-allocate file sizes once per file instead of calling `metadata().len()` and `set_len()` on every segment. Avoid sync std I/O on Tokio worker threads; wrap in `spawn_blocking` if necessary.

### 5. Eliminate redundant buffer copies

- Return `Vec<u8>` from NNTP fetch and feed directly into decoding without intermediate split/join cycles.
- Move the buffer in the success `DownloadResult` instead of cloning it.

### 6. Improve file write architecture (advanced)

Instead of locking `Mutex<File>` from 100 tasks, send `(offset, bytes)` to a dedicated writer task per output file. This removes mutex convoying, allows batching, and keeps Tokio threads unblocked.

### 7. Switch BoundedCache to tokio::sync::Mutex or a concurrent map

Replace `std::sync::Mutex` with `tokio::sync::Mutex` or a lock-free concurrent map to avoid blocking Tokio worker threads.
