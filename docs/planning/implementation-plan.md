# bergamot Implementation Plan

## Current state (post-TDD audit)

14 workspace crates with real implementations and comprehensive tests. The binary
entrypoint, config loading, tracing, CLI parsing, daemon mode, web server,
scheduler services, download worker, and graceful shutdown are all wired together
in `app.rs`. However, an end-to-end audit reveals that **many components only
work to the extent needed to pass their tests** — the actual runtime cannot
download anything yet.

### What works

- **Binary entrypoint + wiring** — config → tracing → spawn coordinator/server/scheduler → graceful shutdown ✅
- **CLI parsing** — config path, log level, daemon mode, pidfile ✅
- **Config parsing** — key=value format, variable interpolation, server/category extraction ✅
- **NZB parsing** — full XML parser with gzip support, filename extraction, par2 classification, dedup ✅
- **NNTP protocol** — connect, authenticate, TLS/STARTTLS, GROUP, BODY, dot-unstuffing ✅
- **NNTP connection pool** — multi-server with level/group sorting, semaphore limits, backoff ✅
- **yEnc decoding** — single-part and multi-part segments, CRC verification ✅
- **Queue coordinator** — actor model, command dispatch, article assignment, health/critical-health ✅
- **Download worker** — fetch → decode → write segment to disk, speed limiting, article cache ✅
- **Web server** — JSON-RPC, JSONP, URL-credential auth, Basic auth, static file serving ✅
- **RPC dispatch** — append, listgroups, editqueue, status, version, shutdown, history, rate, pause/resume ✅
- **Scheduler** — task parsing, weekday/time matching, service runner with shutdown ✅
- **NZB directory scanner** — watches nzb_dir, renames to .queued, adds to queue ✅
- **Disk state** — JSON serialization, atomic writes, state lock, recovery, consistency checks ✅
- **Post-processing** — PAR2 verify/repair, unpack (rar/7z/zip), cleanup, move-to-destination ✅
- **Extension system** — script runner, env construction, output parsing, config injection ✅
- **Feed system** — RSS/Atom/Newznab parsing, filter evaluation, history dedup, coordinator ✅
- **Logging** — tracing layer → ring buffer, log levels, history coordinator ✅
- **History operations** — return-to-queue, redownload (reset state), mark good/bad, delete ✅
- **Speed limiting** — token bucket limiter, rate watch channel from coordinator ✅
- **Article cache** — bounded LRU cache with eviction ✅
- **Daemon mode / pidfile** — double-fork daemonize, pidfile create/cleanup ✅

### Critical gap: nothing actually downloads

The **#1 blocker** is that `QueueCommand::AddNzb` in `coordinator.rs` creates an
`NzbInfo` with `files: Vec::new()`. The NZB file content is never read or parsed.
Since `next_assignment()` iterates `nzb.files` to find segments with
`ArticleStatus::Undefined`, an empty files list means zero assignments are ever
produced and the download worker sits idle.

---

## Phase 1 — Make downloads work (critical path)

These items must be completed in order for the application to function as a
downloader at all.

| # | Gap | Detail | Crates |
|---|-----|--------|--------|
| 1 | **NZB ingestion on enqueue** | Read + parse NZB file content using `bergamot-nzb` parser inside `AddNzb` handler. Map parsed `NzbFile`/`Segment` structs into `FileInfo`/`ArticleInfo` on the queue's `NzbInfo`. Populate sizes, article counts, groups. | `bergamot-queue` |
| 2 | **File/NZB size tracking on download** | `handle_download_complete()` must update `remaining_size`, `success_size`, `failed_size` on both the file and NZB when a segment succeeds/fails. | `bergamot-queue` |
| 3 | **Completion detection** | Detect when all articles in a file are finished (mark file completed). Detect when all files in an NZB are finished (move NZB to history). | `bergamot-queue` |
| 4 | **Disk state restore on startup** | Call `disk.load_queue()` on startup and seed the in-memory `QueueCoordinator` with restored state, so partially-downloaded NZBs resume after restart. | `bergamot`, `bergamot-queue` |
| 5 | **Output filename from NZB metadata** | Use `filename` from parsed NZB (subject extraction) instead of `file-{idx}` when writing segments to disk. | `bergamot` (download.rs) |

---

## Phase 2 — Post-processing integration ✅

| # | Gap | Status |
|---|-----|--------|
| 6 | **Start PostProcessor in app.rs** | ✅ PostProcessor spawned with Par2CommandLine/CommandLineUnpacker, wired into shutdown |
| 7 | **Emit PostProcessRequest on NZB completion** | ✅ NzbCompletionNotice emitted via completion_tx, forwarded to PostProcessRequest |
| 8 | **PAR2 file discovery** | ✅ find_par2_file() discovers .par2 files in working directory |
| 9 | **Update queue/history statuses from postproc** | ✅ QueueCommand::UpdatePostStatus updates par/unpack/move status on queue or history |
| 10 | **Extension execution during postproc** | ✅ ExtensionExecutor trait called after move stage with par/unpack context |

---

## Phase 3 — RPC completeness ✅

| # | RPC Method | Status |
|---|------------|--------|
| 11 | `listfiles` | ✅ Returns file details (ID, filename, subject, size, articles, paused) for a given NZBID via GetFileList queue command |
| 12 | `postqueue` | ✅ Returns paused status with jobs list; paused state via AtomicBool |
| 13 | `loadlog` | ✅ Reads from LogBuffer with since_id filtering |
| 14 | `writelog` | ✅ Writes log entry to LogBuffer with level mapping |
| 15 | `servervolumes` | Deferred — needs NNTP StatsTracker wiring (Phase 6) |
| 16 | `config` / `loadconfig` | ✅ Returns current config key-value pairs from shared RwLock<Config> |
| 17 | `saveconfig` | ✅ Updates options via Config::set_option() and persists to disk |
| 18 | `configtemplates` | ✅ Returns metadata for known config options |
| 19 | `feeds` | Deferred — needs FeedCoordinator (Phase 4) |
| 20 | `sysinfo` | ✅ Returns version, OS, architecture, uptime |
| 21 | `systemhealth` | ✅ Returns queue availability and health status |
| 22 | `log` | ✅ Alias to loadlog |
| 23 | `pausepost`/`resumepost` | ✅ Toggles AtomicBool pause state, reflected in postqueue |
| 24 | `pausescan`/`resumescan` | ✅ Toggles AtomicBool scan pause state |
| 25 | `scan` | ✅ Sends trigger on mpsc channel for immediate directory scan |

---

## Phase 4 — Feed & scheduler integration ✅

| # | Gap | Status |
|---|-----|--------|
| 26 | **Wire FeedCoordinator into app** | ✅ FeedCoordinator actor spawned in app.rs with FeedHandle, config extraction, HttpFeedFetcher |
| 27 | **Implement scheduler FetchFeed command** | ✅ CommandExecutor calls FeedHandle::process_feed() via CommandDeps |
| 28 | **Implement ActivateServer/DeactivateServer** | ✅ ServerPoolManager with RwLock-based pool rebuild, wired into CommandDeps |
| 29 | **Implement remaining scheduler commands** | ✅ PausePostProcess/UnpausePostProcess via AtomicBool, PauseScan/UnpauseScan via AtomicBool, Process via scan trigger, Extensions as info-log |
| 30 | **Feed filter Age/Rating/Genre/Tag** | ✅ Age (days comparison), Rating (numeric), Genre/Tag (wildcard match against lists) |
| 31 | **Feed persistence** | ✅ to_serializable()/load_entries() conversion helpers, DiskState roundtrip test, feed history wiring ready |
| 32 | **URL-based NZB downloads** | ✅ RPC append detects http/https URLs, fetches via reqwest, writes temp file, enqueues normally |

---

## Phase 5 — Server & auth hardening ✅

| # | Gap | Status |
|---|-----|--------|
| 33 | **HTTPS/TLS control server** | ✅ axum-server with rustls, config mapping for SecureCert/SecureKey, validation on startup |
| 34 | **Authorized IP enforcement** | ✅ is_ip_allowed with exact/wildcard/CIDR matching, ConnectInfo plumbing, localhost always allowed |
| 35 | **XML-RPC endpoint** | ✅ Full XML-RPC request parsing and response generation, maps to existing dispatch_rpc |
| 36 | **AppState live updates** | ✅ AtomicU64 for download_rate/remaining_bytes, periodic 1s updater from queue status |
| 37 | **Form-based auth** | ✅ HMAC-signed session cookies, login/logout endpoints, cookie auth integrated into middleware |

---

## Phase 6 — Operational completeness ✅

| # | Gap | Status |
|---|-----|--------|
| 38 | **NNTP connection lifecycle** | ✅ Idle timeout (60s), max pool size cap, broken connection drop, periodic QUIT via cleanup_idle_connections() |
| 39 | **Server volume stats** | ✅ StatsRecorder trait injected into ServerPool, record_bytes() called on successful fetch, SharedStatsTracker in scheduler |
| 40 | **DiskSpaceMonitor pause/resume** | ✅ Calls queue.pause_all() on low space, queue.resume_all() on recovery, injectable space checker for tests |
| 41 | **HistoryCleanup** | ✅ Fetches history, compares against keep_days cutoff, deletes old entries via queue.history_delete() |
| 42 | **ConnectionCleanup** | ✅ tick() calls pool.cleanup_idle_connections() with 60s timeout, logs closed connections |
| 43 | **HealthChecker** | ✅ check_certificate_expiry() validates cert file accessibility, check_queue_consistency() runs disk validate_consistency() |
| 44 | **DiskStateFlush snapshot fidelity** | ✅ NzbSnapshotEntry carries url, dupe_key, dupe_score, dupe_mode, added_time, parameters, final_dir; snapshot_to_queue_state uses real values |
| 45 | **File article state persistence** | ✅ FileArticleSnapshot from coordinator, DiskStateFlush saves/cleans file states, restore_queue loads and applies completed article bitmaps |
| 46 | **Speed limiter placement** | ✅ limiter.acquire(expected_size) called BEFORE fetch_and_decode, expected_size field added to ArticleAssignment |

---

## Previously completed ✅

| Item | Status |
|------|--------|
| Full history operations (return-to-queue, redownload, mark good/bad) | ✅ |
| Speed limiting / quotas / bandwidth scheduling | ✅ |
| Advanced postproc strategies (Balanced/Rocket/Aggressive) | ✅ |
| Daemon mode / systemd / pidfile | ✅ |
| Article cache + IO batching | ✅ |
| Binary wiring (config + tracing + shutdown + spawn tasks) | ✅ |
| Download worker loop (NNTP BODY → yEnc → write → report) | ✅ |
| Basic RPC (append, status, queue controls, shutdown, history) | ✅ |
| Web UI static file serving | ✅ |
| Multi-server pools + retry/failover | ✅ |
| NZB directory scanner | ✅ |
| Logging with tracing layer + ring buffer | ✅ |
| CLI parsing with clap | ✅ |
| Disk state serialization + atomic writes | ✅ |

---

## Key design guardrails

- **State ownership**: Keep the coordinator as the single owner of mutable queue
  state (actor model). Don't leak `Arc<Mutex<QueueState>>` everywhere.
- **Cancellation safety**: Download tasks must handle shutdown without corrupting
  partially-written files; mark articles `Undefined` on cancellation so they retry
  after restart.
- **Backpressure**: Bounded channels between API/feed/scheduler and coordinator;
  avoid unbounded spawning on large queues.
- **Compatibility**: Limit RPC to the minimum viable set until end-to-end flow is
  solid; add methods only when a real client needs them.
- **Filesystem correctness**: Atomic renames, temp suffixes, pre-allocation
  strategy, cross-device moves.

## Recommended priority

1. **Phase 1** (NZB ingestion + download lifecycle) — without this, the app does nothing useful
2. **Phase 2** (Post-processing) — without this, downloaded files are raw segments
3. **Phase 4** (Feeds + scheduler) — enables automated downloading
4. **Phase 3** (RPC completeness) — enables Sonarr/Radarr/web UI compatibility
5. **Phase 5** (Server hardening) — needed for production deployment
6. **Phase 6** (Operational) — polish for reliability and performance
