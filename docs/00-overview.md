# 00 — Architecture Overview

## What Is bergamot?

**bergamot** is a ground-up Rust reimplementation of
[NZBGet](https://nzbget.com), the efficient Usenet binary downloader
originally written in C++. NZBGet downloads files described by NZB
files from Usenet (NNTP) servers, reassembles multi-part articles,
decodes yEnc payloads, and orchestrates post-processing (PAR2 repair,
archive extraction, script execution). It exposes a JSON-RPC / XML-RPC
API consumed by its built-in web UI and by third-party tools such as
Sonarr, Radarr, and SABnzbd integrations.

bergamot aims to preserve NZBGet's battle-tested architecture while
leveraging Rust's type system, ownership model, and async runtime to
deliver the same (or better) performance with stronger safety
guarantees.

---

## High-Level Architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│                        Clients                                      │
│   ┌───────────┐   ┌──────────┐   ┌──────────────────┐               │
│   │  Web UI   │   │   CLI    │   │  Third-Party Apps │              │
│   │ (browser) │   │ (bergamot)   │   │ (Sonarr, etc.)   │               │
│   └─────┬─────┘   └─────┬────┘   └─────────┬─────────┘              │
│         │               │                  │                        │
└─────────┼───────────────┼──────────────────┼────────────────────────┘
          │               │                  │
          ▼               ▼                  ▼
┌─────────────────────────────────────────────────────────────────────┐
│                    RPC Layer                                        │
│         JSON-RPC  ·  XML-RPC  ·  REST/HTTP                          │
│                  (hyper / axum)                                     │
└──────────────────────────┬──────────────────────────────────────────┘
                           │
          ┌────────────────┼────────────────┐
          ▼                ▼                ▼
┌─────────────────┐ ┌──────────────┐ ┌───────────────────┐
│ Queue           │ │ Feed         │ │ Scheduler         │
│ Coordinator     │ │ Coordinator  │ │ (cron-like tasks) │
│                 │ │ (RSS → NZB)  │ │                   │
│  ┌───────────┐  │ └──────────────┘ └───────────────────┘
│  │  NZB      │  │
│  │  Parser   │  │
│  └───────────┘  │
└───────┬─────────┘
        │
        ▼
┌─────────────────────────────────────────────────────────────────────┐
│                     NNTP Engine                                     │
│  ┌───────────────┐  ┌──────────────┐  ┌──────────────────────────┐  │
│  │  Connection   │  │  Article     │  │  yEnc Decoder            │  │
│  │  Pool         │  │  Cache       │  │  (SIMD-accelerated)      │  │
│  │  (tokio TCP / │  │  (mem-mapped │  │                          │  │
│  │   rustls TLS) │  │   or heap)   │  │                          │  │
│  └───────────────┘  └──────────────┘  └──────────────────────────┘  │
└───────────────────────────┬─────────────────────────────────────────┘
                            │
                            ▼
┌─────────────────────────────────────────────────────────────────────┐
│                   Post-Processing Pipeline                          │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌───────────────────┐  │
│  │ PAR2     │  │ Unpack   │  │ Move /   │  │ Extension Scripts │  │
│  │ Verify / │  │ (rar,7z, │  │ Cleanup  │  │ (process runner)  │  │
│  │ Repair   │  │  zip)    │  │          │  │                   │  │
│  └──────────┘  └──────────┘  └──────────┘  └───────────────────┘  │
└─────────────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌─────────────────────────────────────────────────────────────────────┐
│                       Foundation                                    │
│  ┌──────────┐  ┌──────────┐  ┌───────────┐  ┌──────────────────┐  │
│  │  Config  │  │ Logging  │  │ DiskState │  │   FileSystem     │  │
│  │          │  │ & History│  │ (persist) │  │   Utilities      │  │
│  └──────────┘  └──────────┘  └───────────┘  └──────────────────┘  │
└─────────────────────────────────────────────────────────────────────┘
```

---

## Document Index

| #  | File                          | Topic                                         |
|----|-------------------------------|-----------------------------------------------|
| 00 | `00-overview.md`              | Architecture overview (this document)          |
| 01 | `01-core-data-structures.md`  | Core data structures & ownership model         |
| 02 | `02-nzb-parsing.md`           | NZB file parsing                               |
| 03 | `03-nntp-engine.md`           | NNTP connection engine & article downloading   |
| 04 | `04-yenc-decoding.md`         | yEnc decoding & CRC verification               |
| 05 | `05-queue-coordinator.md`     | Queue coordinator & download orchestration     |
| 06 | `06-post-processing.md`       | Post-processing: PAR2, unpack, scripts         |
| 07 | `07-web-server-api.md`        | Web server, JSON-RPC / XML-RPC API             |
| 08 | `08-configuration.md`         | Configuration system                           |
| 09 | `09-scheduler-services.md`    | Scheduler & background services                |
| 10 | `10-feed-system.md`           | RSS / Atom feed system                         |
| 11 | `11-extension-system.md`      | Extension / script system                      |
| 12 | `12-disk-state.md`            | Disk state persistence & recovery              |
| 13 | `13-logging-history.md`       | Logging, message buffer, history tracking      |
| 14 | `14-rust-architecture.md`     | Rust crate structure & cross-cutting concerns  |
| 15 | `15-download-flow.md`         | End-to-end download flow walkthrough           |

---

## Key Design Principles

These principles are inherited from NZBGet and carry forward into bergamot.

### Performance First

NZBGet was designed to saturate gigabit (and faster) links on hardware
as modest as a Raspberry Pi. bergamot must match that bar. Hot paths —
yEnc decoding, CRC computation, NNTP I/O — are benchmarked and
optimised aggressively. Where NZBGet uses hand-tuned C++, bergamot
leverages Rust's zero-cost abstractions, SIMD intrinsics, and
`tokio`'s work-stealing scheduler.

### Resilience

Downloads can take hours or days. The system must survive:

- process crashes (disk-state persistence, atomic writes),
- network interruptions (retry with back-off, server failover),
- corrupted articles (PAR2 repair, per-article CRC checks),
- partial downloads (resume from exact byte position).

### Low Resource Usage

Memory and CPU budgets must remain predictable. The article cache is
bounded. Decoded segments are flushed to disk promptly. Connection
counts are configurable per server. bergamot should be comfortable running
under systemd on a NAS with 512 MB of RAM.

### Extensibility

Users customise behaviour with post-processing and scan scripts
(Python, Bash, etc.). bergamot preserves the NZBGet extension interface
for backward compatibility and may introduce a native Rust plugin API
in future.

### Compatibility

bergamot aims for drop-in compatibility with NZBGet's:

- **Configuration file format** — existing `nzbget.conf` files should
  load with minimal changes.
- **JSON-RPC / XML-RPC API** — third-party tools (Sonarr, Radarr,
  NZB360, nzb-notify) must work unmodified.
- **Extension interface** — existing scripts receive the same
  environment variables and exit-code semantics.

---

## Rust-Specific Goals

### Memory Safety Without GC

Rust's ownership and borrowing system eliminates use-after-free, double
free, and data races at compile time. This replaces NZBGet's manual
memory management and mutex discipline with compiler-enforced
invariants.

### Async I/O with Tokio

All network I/O (NNTP connections, HTTP server, feed fetches) runs on
the Tokio async runtime. This replaces NZBGet's thread-per-connection
model with a small pool of OS threads multiplexing thousands of
futures, reducing context-switch overhead and memory footprint.

### Fearless Concurrency

Shared mutable state is managed through message-passing (tokio
channels, actor-style coordinators) rather than bare mutexes wherever
practical. Where shared state is unavoidable, `Arc<RwLock<_>>` or
lock-free structures are used with clear ownership boundaries
documented per module.

### Modern API Surface

While maintaining backward-compatible JSON-RPC, bergamot also exposes a
clean REST/JSON API suitable for modern front-end frameworks and
OpenAPI documentation.

### Modular Crate Structure

The project is organised as a Cargo workspace with focused crates:

| Crate            | Responsibility                              |
|------------------|---------------------------------------------|
| `bergamot`           | Binary entry point, CLI, top-level wiring   |
| `bergamot-core`      | Data structures, config, error types        |
| `bergamot-nntp`      | NNTP protocol, connection pooling           |
| `bergamot-yenc`      | yEnc decoding, CRC-32                       |
| `bergamot-nzb`       | NZB XML parsing                             |
| `bergamot-queue`     | Queue coordinator, download orchestration   |
| `bergamot-postproc`  | PAR2 verification, unpacking, scripts       |
| `bergamot-server`    | HTTP server, JSON-RPC / XML-RPC handlers    |
| `bergamot-feed`      | RSS/Atom feed polling & matching            |
| `bergamot-scheduler` | Cron-like task scheduler                    |
| `bergamot-diskstate` | On-disk persistence & crash recovery        |

Each crate has an independent test suite and can be developed, tested,
and documented in isolation.
