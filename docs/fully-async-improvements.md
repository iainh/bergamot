# Fully Async NNTP Implementation Improvements

This document catalogs every blocking, pseudo-blocking, and inefficient pattern in the NNTP download path, along with the specific changes required to make the implementation fully async.

## Current Architecture

The download path flows through these components:

```
QueueCoordinator → download_worker → ServerPool → NntpConnection → BodyReader
                                   → BoundedCache
                                   → YencDecoder
                                   → FileWriterPool
```

## 1. ServerPool: `try_acquire_owned()` Fails Instantly Instead of Waiting

**File:** `crates/nzbg-nntp/src/pool.rs` line 92  
**Type:** Design flaw (non-blocking where it should be async-waiting)  
**Severity:** Critical  

`fetch_article` uses `try_acquire_owned()` on each server's semaphore. When all permits are in use, it returns an error immediately (`"no servers available"`) instead of waiting for a connection to become available. At high connection counts, this creates a tight failure loop: tasks fail instantly, get reported as errors, and must be retried, wasting CPU and degrading throughput.

**Change:** Replace `try_acquire_owned()` with an async wait strategy. The simplest approach is to collect available servers and `tokio::select!` across their `acquire_owned().await` calls, so the task waits on whichever server has the next free permit. This transforms a busy-fail loop into productive async waiting.

**Effort:** Small  
**Impact:** High — eliminates the primary cause of speed drops at high connection counts

## 2. ServerPool: Backoff and Fail Count Use `tokio::Mutex` for Simple Scalars

**File:** `crates/nzbg-nntp/src/pool.rs` lines 213–231  
**Type:** Unnecessary async lock contention  
**Severity:** Medium  

`backoff_until: Mutex<Option<Instant>>` and `fail_count: Mutex<u32>` are locked on every `fetch_article` call (once per server to check backoff, plus on success/failure to update). These protect simple scalar values that could be atomic.

**Change:** Replace `fail_count` with `AtomicU32`. Replace `backoff_until` with an `AtomicU64` storing the backoff deadline as milliseconds since a reference `Instant` (or epoch). Use `Ordering::Relaxed` for loads and `Ordering::Release` for stores. This eliminates all locking in the hot path for backoff checks.

**Effort:** Small  
**Impact:** Medium — removes 2–4 lock acquisitions per server per fetch attempt

## 3. FileWriterPool: Blocking Sync Syscalls on Tokio Worker Threads

**File:** `crates/nzbg/src/writer.rs` lines 25–33  
**Type:** Blocking I/O in async context  
**Severity:** High  

Every `write_segment` call performs these blocking operations while holding the per-file mutex:

1. `try_clone().await` — creates a duplicate file handle (syscall)
2. `into_std().await` — converts the async file to a sync `std::fs::File`
3. `std_file.metadata()?.len()` — **blocking** `fstat` syscall on a Tokio worker thread
4. `std_file.set_len(target_len)?` — **blocking** `ftruncate` syscall on a Tokio worker thread

These block the Tokio runtime thread, preventing it from polling other sockets. At 100 concurrent tasks, segments complete in bursts and contend on these blocking calls, stalling network I/O progress.

**Change:** Track the allocated file length in memory per file (e.g., in a `HashMap<PathBuf, u64>` or alongside the file handle). Only call `set_len` when the file is first created, using the known total file size from the NZB metadata. Remove the per-segment `try_clone`/`into_std`/`metadata` chain entirely. If `set_len` is still needed at file creation time, wrap it in `tokio::task::spawn_blocking`.

**Effort:** Medium  
**Impact:** High — eliminates blocking syscalls that stall the Tokio runtime

## 4. FileWriterPool: Global Mutex for File Handle Registry

**File:** `crates/nzbg/src/writer.rs` lines 41–65  
**Type:** Async lock contention  
**Severity:** Medium  

Every `write_segment` call locks `Mutex<HashMap<PathBuf, ...>>` to look up the file handle, even though the vast majority of calls are cache hits (the file is already open).

**Change:** Replace `Mutex<HashMap<...>>` with a `DashMap<PathBuf, Arc<Mutex<tokio::fs::File>>>`. This allows concurrent reads without locking and only takes a shard-level lock on insert. Alternatively, use `tokio::sync::RwLock<HashMap<...>>` so lookups take a shared read lock.

**Effort:** Small  
**Impact:** Medium — removes the global serialization point for file lookups

## 5. FileWriterPool: Per-File Mutex Serializes All Writes to Same File

**File:** `crates/nzbg/src/writer.rs` lines 23, 36–37  
**Type:** Lock convoying  
**Severity:** Medium-High  

Each output file is protected by `Arc<Mutex<tokio::fs::File>>`. All segments for the same file (which is the common case — NZBs have many segments per file) serialize on this lock. A download task cannot report completion to the coordinator until the write finishes, meaning network scheduling is throttled by disk write speed.

**Change:** Replace the per-file `Mutex<File>` with a dedicated writer task per file. Each writer task owns the file handle exclusively and receives `(offset, data)` messages via an `mpsc` channel. Download tasks send write requests and can immediately report completion to the coordinator without waiting for the disk write. The writer task processes writes sequentially (which is correct for a single file) without blocking any download tasks.

**Effort:** Large  
**Impact:** High — decouples network download completion from disk write latency

## 6. BoundedCache: `std::sync::Mutex` in Async Context

**File:** `crates/nzbg/src/cache.rs` lines 43–53  
**Type:** Blocking mutex in async code  
**Severity:** Medium  

`BoundedCache` uses `std::sync::Mutex` for `get()` and `put()`. When a Tokio worker thread blocks on this mutex, it cannot poll other futures (sockets, timers). The `get()` method also clones the entire article buffer while holding the lock, extending the critical section.

**Change:** Option A: Switch to `tokio::sync::Mutex` — straightforward but still serializes access. Option B: Use `DashMap` for the entries map, with a separate `Mutex<VecDeque>` for the eviction order (only locked on `put`). Option C: Use a purpose-built concurrent LRU cache crate like `moka` which is designed for async workloads.

**Effort:** Small  
**Impact:** Medium — prevents Tokio worker thread blocking under cache contention

## 7. Data Pipeline: Excessive Buffer Copies

**Files:**
- `crates/nzbg-nntp/src/pool.rs` line 154
- `crates/nzbg/src/download.rs` lines 37, 141, 101

**Type:** Inefficient memory usage  
**Severity:** Medium  

Each article body is copied 4 times through the pipeline:

1. **Pool layer:** `body_lines.join(&b'\n')` — reads lines into `Vec<Vec<u8>>`, then joins into a single `Vec<u8>`
2. **Fetcher layer:** `data.split(|&b| b == b'\n').map(|s| s.to_vec()).collect()` — splits the joined buffer back into lines, allocating a new `Vec<u8>` per line
3. **Cache layer:** `fetched.join(&b'\n')` — joins lines back together again for caching
4. **Result layer:** `data.clone()` — clones the decoded segment data into the `DownloadResult`

At 100 concurrent tasks with ~500KB articles, this creates ~200MB of redundant allocation pressure per second.

**Change:**
- Return a single `Vec<u8>` (the raw body) from `ServerPool::try_fetch_from_server` instead of joining lines.
- Change `ArticleFetcher::fetch_body` to return `Vec<u8>` instead of `Vec<Vec<u8>>`.
- Feed lines directly from `BodyReader` into `YencDecoder` without materializing the full body (streaming decode).
- Move (not clone) the decoded data into the `DownloadResult`.

**Effort:** Medium  
**Impact:** Medium — halves memory bandwidth, reduces allocator contention

## 8. SpeedLimiter: Global `tokio::Mutex` Serializes Download Starts

**File:** `crates/nzbg/src/download.rs` lines 51, 80–87  
**Type:** Async lock contention  
**Severity:** Medium  

The `SpeedLimiter` is behind `Arc<tokio::Mutex<SpeedLimiter>>`. Every spawned download task locks this mutex twice: once for `reserve()` and once for `after_sleep()`. With 100 concurrent tasks, this serializes the start of all downloads through a single point.

**Change:** Option A: Run the SpeedLimiter as a dedicated actor task that receives `(bytes, oneshot::Sender<Duration>)` reservation requests via an mpsc channel. Download tasks send a request and await the response without holding any lock. Option B: Use a lock-free token bucket algorithm (e.g., based on `AtomicU64` for the token count and `AtomicU64` for the last-refill timestamp). Option C: If the critical section is truly tiny, a `std::sync::Mutex` might actually perform better here than `tokio::sync::Mutex` since there is no await inside the lock.

**Effort:** Medium  
**Impact:** Medium — removes global serialization of download task starts

## 9. Download Result: Unnecessary Data Clone

**File:** `crates/nzbg/src/download.rs` line 101  
**Type:** Unnecessary allocation  
**Severity:** Low  

The success path clones the decoded segment data: `data: data.clone()`. The `data` binding comes from `Ok((ref data, offset, _crc))` which borrows the tuple. The clone is avoidable.

**Change:** Destructure without `ref`: `Ok((data, offset, crc))` and move `data` directly into the `DownloadResult`. This eliminates one large buffer copy per successful segment.

**Effort:** Small  
**Impact:** Low-Medium — removes one ~500KB allocation per segment

## Summary

| # | Component | Issue | Fix | Effort | Impact |
|---|-----------|-------|-----|--------|--------|
| 1 | ServerPool | `try_acquire_owned()` fails instantly | Async wait with `acquire_owned().await` | S | Critical |
| 2 | ServerPool | Mutex on backoff/fail_count scalars | Replace with atomics | S | Medium |
| 3 | FileWriterPool | Blocking `metadata()`/`set_len()` syscalls | Track length in memory, pre-allocate | M | High |
| 4 | FileWriterPool | Global mutex for file handle registry | Use `DashMap` or `RwLock` | S | Medium |
| 5 | FileWriterPool | Per-file mutex serializes writes | Dedicated writer task per file | L | High |
| 6 | BoundedCache | `std::sync::Mutex` blocks Tokio threads | Switch to `tokio::sync::Mutex` or `DashMap` | S | Medium |
| 7 | Data pipeline | 4 redundant buffer copies per article | Streaming decode, return `Vec<u8>` | M | Medium |
| 8 | SpeedLimiter | Global mutex serializes all downloads | Actor pattern or lock-free token bucket | M | Medium |
| 9 | DownloadResult | Unnecessary `data.clone()` | Move instead of clone | S | Low-Medium |

### Recommended Implementation Order

**Phase 1 — Quick wins (items 1, 2, 4, 6, 9):** All small effort, can be done independently. Item 1 alone should resolve the speed drop at high connection counts.

**Phase 2 — File I/O overhaul (items 3, 5):** Remove blocking syscalls and decouple network from disk. These require coordinated changes to the writer subsystem.

**Phase 3 — Data path optimization (items 7, 8):** Streaming decode and SpeedLimiter redesign. These touch multiple crate boundaries and benefit from the earlier changes being in place.
