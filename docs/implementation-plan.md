# nzbg Implementation Plan

## Current State

14 library crates with real implementations and tests. Binary entrypoints are stubs. No crate wires everything together yet.

## Phase A — MVP (end-to-end download works)

| # | Gap | Crates Affected |
|---|-----|-----------------|
| 1 | **Binary entrypoint + wiring** — config → tracing → spawn coordinator/server/scheduler → graceful shutdown | `nzbg`, `src/main.rs` |
| 2 | **Download worker loop** — coordinator selects article → NNTP `BODY` → yEnc decode → write to disk → report back | `nzbg-queue`, `nzbg-nntp`, `nzbg-yenc` |
| 3 | **Scheduler tick() real implementations** — at minimum "fill download slots" and periodic diskstate flush | `nzbg-scheduler` |
| 4 | **Disk state integration** — load on startup, persist on changes + periodic + shutdown | `nzbg-diskstate` |
| 5 | **RPC methods beyond version/status** — need at least `append`, `listgroups`, `editqueue`, `shutdown` | `nzbg-server` |
| 6 | **Logging setup** — tracing subscriber + ring buffer sink for web UI | `nzbg-logging` |
| 7 | **CLI parsing** — config path, log level, daemon mode (via `clap`) | `nzbg` |
| 8 | **Web UI static file serving** | `nzbg-server` |

## Phase B — Functional (daily-use quality)

| # | Gap |
|---|-----|
| 9 | **NNTP connection pool + multi-server failover** (levels, groups, per-server backoff) |
| 10 | **Full queue coordinator behaviors** — priority algorithm, PAR2 on-demand unpause, health calculation, dupe detection |
| 11 | **Post-processing pipeline** — unpack (rar/7z/zip), cleanup, move-to-destination, PAR2 rename |
| 12 | **Extension execution** — subprocess runner with `NZBOP_*`/`NZBPP_*` env vars, stdout protocol |
| 13 | **Feed coordinator** — RSS polling, HTTP fetch, filter matching, dedup |
| 14 | **Full RPC method surface** (~30 methods for Sonarr/Radarr/web UI compat) |
| 15 | **NZB acquisition paths** — directory scan, URL fetch with retries |

## Phase C — Feature-complete

| # | Gap |
|---|-----|
| 16 | Full history operations (return-to-queue, redownload, mark good/bad) |
| 17 | Speed limiting / quotas / bandwidth scheduling |
| 18 | Advanced postproc strategies (Balanced/Rocket/Aggressive, direct unpack) |
| 19 | Daemon mode / systemd / pidfile |
| 20 | Article cache + IO batching performance optimizations |

## Recommended Critical Path

1. Binary wiring (config + tracing + shutdown + spawn tasks)
2. Download worker loop (NNTP BODY → yEnc decode → write segment → report)
3. Diskstate integration (restore + periodic snapshot + shutdown flush)
4. Basic RPC (append, status, queue controls, shutdown)
5. Web UI static serving
6. Multi-server pools + retry/failover
7. Postproc pipeline (unpack/move/cleanup + extensions)
8. Feed + scan-dir acquisition
9. RPC compatibility (full method surface)

## Key Design Guardrails

- **State ownership**: Keep the coordinator as the single owner of mutable queue state (actor model). Don't leak `Arc<Mutex<QueueState>>` everywhere.
- **Cancellation safety**: Download tasks must handle shutdown without corrupting partially-written files; mark articles `Undefined` on cancellation so they retry after restart.
- **Backpressure**: Bounded channels between API/feed/scheduler and coordinator; avoid unbounded spawning on large queues.
- **Compatibility**: Limit RPC to the minimum viable set until end-to-end flow is solid; add methods only when a real client needs them.
- **Filesystem correctness**: Atomic renames, temp suffixes, pre-allocation strategy, cross-device moves.
