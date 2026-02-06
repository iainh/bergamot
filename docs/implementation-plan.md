# nzbg Implementation Plan

## Current State (Post-TDD Audit)

14 workspace crates with real implementations and comprehensive tests. The binary
entrypoint, config loading, tracing, CLI parsing, daemon mode, web server,
scheduler services, download worker, and graceful shutdown are all wired together
in `app.rs`. However, an end-to-end audit reveals that **many components only
work to the extent needed to pass their tests** — the actual runtime cannot
download anything yet.

### What Works

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

### Critical Gap: Nothing Actually Downloads

The **#1 blocker** is that `QueueCommand::AddNzb` in `coordinator.rs` creates an
`NzbInfo` with `files: Vec::new()`. The NZB file content is never read or parsed.
Since `next_assignment()` iterates `nzb.files` to find segments with
`ArticleStatus::Undefined`, an empty files list means zero assignments are ever
produced and the download worker sits idle.

---

## Phase 1 — Make Downloads Work (Critical Path)

These items must be completed in order for the application to function as a
downloader at all.

| # | Gap | Detail | Crates |
|---|-----|--------|--------|
| 1 | **NZB ingestion on enqueue** | Read + parse NZB file content using `nzbg-nzb` parser inside `AddNzb` handler. Map parsed `NzbFile`/`Segment` structs into `FileInfo`/`ArticleInfo` on the queue's `NzbInfo`. Populate sizes, article counts, groups. | `nzbg-queue` |
| 2 | **File/NZB size tracking on download** | `handle_download_complete()` must update `remaining_size`, `success_size`, `failed_size` on both the file and NZB when a segment succeeds/fails. | `nzbg-queue` |
| 3 | **Completion detection** | Detect when all articles in a file are finished (mark file completed). Detect when all files in an NZB are finished (move NZB to history). | `nzbg-queue` |
| 4 | **Disk state restore on startup** | Call `disk.load_queue()` on startup and seed the in-memory `QueueCoordinator` with restored state, so partially-downloaded NZBs resume after restart. | `nzbg`, `nzbg-queue` |
| 5 | **Output filename from NZB metadata** | Use `filename` from parsed NZB (subject extraction) instead of `file-{idx}` when writing segments to disk. | `nzbg` (download.rs) |

---

## Phase 2 — Post-Processing Integration ✅

| # | Gap | Status |
|---|-----|--------|
| 6 | **Start PostProcessor in app.rs** | ✅ PostProcessor spawned with Par2CommandLine/CommandLineUnpacker, wired into shutdown |
| 7 | **Emit PostProcessRequest on NZB completion** | ✅ NzbCompletionNotice emitted via completion_tx, forwarded to PostProcessRequest |
| 8 | **PAR2 file discovery** | ✅ find_par2_file() discovers .par2 files in working directory |
| 9 | **Update queue/history statuses from postproc** | ✅ QueueCommand::UpdatePostStatus updates par/unpack/move status on queue or history |
| 10 | **Extension execution during postproc** | ✅ ExtensionExecutor trait called after move stage with par/unpack context |

---

## Phase 3 — RPC Completeness ✅

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

## Phase 4 — Feed & Scheduler Integration ✅

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

## Phase 5 — Server & Auth Hardening

| # | Gap | Detail | Crates |
|---|-----|--------|--------|
| 33 | **HTTPS/TLS control server** | `secure_control` config is mapped but `secure_cert`/`secure_key` are set to `None`. Server always binds plain TCP. | `nzbg`, `nzbg-server` |
| 34 | **Authorized IP enforcement** | `authorized_ips` config is read but `auth_middleware` never checks client IP. | `nzbg-server` |
| 35 | **XML-RPC endpoint** | `/xmlrpc` returns "not implemented" error. Many NZBGet clients use XML-RPC. | `nzbg-server` |
| 36 | **AppState live updates** | `download_rate` and `remaining_bytes` in `AppState` are always 0. Need periodic update from queue status. | `nzbg-server`, `nzbg` |
| 37 | **Form-based auth** | `form_auth` config exists but no form login endpoint. | `nzbg-server` |

---

## Phase 6 — Operational Completeness

| # | Gap | Detail | Crates |
|---|-----|--------|--------|
| 38 | **NNTP connection lifecycle** | Pool holds idle connections forever. Need idle timeout, max pool size, periodic QUIT, broken connection detection. | `nzbg-nntp` |
| 39 | **Server volume stats** | `StatsTracker` exists but `record_bytes()` is never called from the NNTP fetch path. | `nzbg-scheduler`, `nzbg-nntp` |
| 40 | **DiskSpaceMonitor pause/resume** | Detects low space and logs, but does not call queue API to pause/resume downloads. | `nzbg-scheduler` |
| 41 | **HistoryCleanup** | Computes cutoff but does not actually delete old history entries or disk files. | `nzbg-scheduler` |
| 42 | **ConnectionCleanup** | `tick()` is empty no-op. | `nzbg-scheduler` |
| 43 | **HealthChecker** | `check_certificate_expiry()` and `check_queue_consistency()` are empty. | `nzbg-scheduler` |
| 44 | **DiskStateFlush snapshot fidelity** | `snapshot_to_queue_state()` uses hardcoded defaults for `dupe_key`, `url`, `added_time`, `post_process_parameters`. | `nzbg-scheduler`, `nzbg-diskstate` |
| 45 | **File article state persistence** | `save_file_state/load_file_state` exist but no runtime code uses them for per-file segment bitmaps. | `nzbg-diskstate` |
| 46 | **Speed limiter placement** | Rate limiting is applied after decode, not around network I/O. Doesn't bound actual bandwidth during fetch. | `nzbg` (download.rs) |

---

## Previously Completed ✅

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

## Key Design Guardrails

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

## Recommended Priority

1. **Phase 1** (NZB ingestion + download lifecycle) — without this, the app does nothing useful
2. **Phase 2** (Post-processing) — without this, downloaded files are raw segments
3. **Phase 4** (Feeds + scheduler) — enables automated downloading
4. **Phase 3** (RPC completeness) — enables Sonarr/Radarr/web UI compatibility
5. **Phase 5** (Server hardening) — needed for production deployment
6. **Phase 6** (Operational) — polish for reliability and performance
