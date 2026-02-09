# Rust Architecture

This document covers Rust-specific design decisions for bergamot: crate structure, async runtime, concurrency model, error handling, dependencies, testing, and deployment concerns.

## Crate / Module Structure

bergamot is organized as a Cargo workspace with focused crates:

```
bergamot/
├── Cargo.toml              # workspace root
├── bergamot/                   # binary crate (main entry point)
│   ├── Cargo.toml
│   └── src/
│       └── main.rs
├── bergamot-core/              # shared data structures, config, types
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── config.rs       # configuration parsing & defaults
│       ├── types.rs        # NzbInfo, FileInfo, ArticleInfo, etc.
│       └── error.rs        # shared error types
├── bergamot-nntp/              # NNTP protocol client, connection pool
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── connection.rs   # single NNTP connection
│       ├── pool.rs         # connection pool per server
│       └── response.rs     # response parsing
├── bergamot-nzb/               # NZB XML parser
│   ├── Cargo.toml
│   └── src/
│       └── lib.rs
├── bergamot-yenc/              # yEnc decoder
│   ├── Cargo.toml
│   └── src/
│       └── lib.rs
├── bergamot-queue/             # queue coordinator, scheduler
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── coordinator.rs  # QueueCoordinator actor
│       ├── scheduler.rs    # article scheduling
│       └── disk_state.rs   # persistence
├── bergamot-post/              # post-processing (PAR2, unpack, move)
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── par2.rs         # PAR2 verify/repair
│       ├── unpack.rs       # archive extraction
│       └── cleanup.rs      # temp file removal, renaming
├── bergamot-api/               # web server, JSON-RPC, XML-RPC
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── jsonrpc.rs
│       ├── xmlrpc.rs
│       └── routes.rs
└── bergamot-cli/               # command-line interface
    ├── Cargo.toml
    └── src/
        └── lib.rs
```

### Crate Responsibilities

| Crate | Responsibility |
|-------|----------------|
| `bergamot` | Binary entry point — wires subsystems, handles signals, runs `main()` |
| `bergamot-core` | Shared types (`NzbInfo`, `FileInfo`, `ArticleInfo`), configuration parsing, error types, constants |
| `bergamot-nntp` | NNTP protocol implementation, connection lifecycle, connection pool with server levels/groups |
| `bergamot-nzb` | Streaming NZB XML parsing via `quick-xml`, filename extraction, PAR2 classification, file reordering |
| `bergamot-yenc` | yEnc line-by-line decoder, CRC32 verification, header parsing (`=ybegin`/`=ypart`/`=yend`) |
| `bergamot-queue` | Queue coordinator actor, article scheduling, download slot management, health monitoring, disk-state persistence |
| `bergamot-post` | Post-processing pipeline: PAR2 verify/repair, archive extraction (RAR/7z/ZIP), cleanup, file move, extension scripts |
| `bergamot-api` | Axum-based HTTP server, JSON-RPC and XML-RPC endpoints for nzbget API compatibility, REST endpoints |
| `bergamot-cli` | CLI argument parsing with `clap`, remote server commands (`-L`, `-P`, `-U`), daemon mode |

### Dependency Graph

```
bergamot (binary)
 ├── bergamot-core
 ├── bergamot-nntp ──► bergamot-core
 ├── bergamot-nzb ──► bergamot-core
 ├── bergamot-yenc
 ├── bergamot-queue ──► bergamot-core, bergamot-nntp, bergamot-nzb, bergamot-yenc
 ├── bergamot-post ──► bergamot-core
 ├── bergamot-api ──► bergamot-core, bergamot-queue, bergamot-post
 └── bergamot-cli ──► bergamot-core, bergamot-api
```

Design rationale:

- **`bergamot-yenc` has no internal dependencies** — it is a pure data-transformation library, usable and testable in isolation.
- **`bergamot-queue` depends on `bergamot-nntp`, `bergamot-nzb`, `bergamot-yenc`** — it orchestrates the full download pipeline.
- **`bergamot-post` depends only on `bergamot-core`** — post-processing operates on files on disk and does not need NNTP or queue internals.
- **`bergamot-api` depends on `bergamot-queue` and `bergamot-post`** — it sends commands to the coordinator and queries post-processing status.

---

## Async Runtime

bergamot uses **tokio** with the multi-threaded runtime:

```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = bergamot_core::Config::load()?;
    bergamot::run(config).await
}
```

tokio is chosen because:

- **Mature ecosystem** — axum, hyper, reqwest, tokio-rustls all build on it.
- **Multi-threaded work-stealing scheduler** — suits the mixed I/O + CPU workload (NNTP connections, yEnc decoding, CRC computation).
- **`tokio::select!`** — enables clean shutdown coordination across subsystems.
- **Async-aware sync primitives** — `tokio::sync::{mpsc, broadcast, RwLock, Mutex, oneshot}` avoid blocking the runtime.
- **Timers and intervals** — `tokio::time` provides non-blocking scheduling for periodic tasks (feed polling, queue saves, slot filling).

### Runtime Configuration

The runtime is configured with the default multi-threaded settings. Worker thread count defaults to the number of CPU cores, which is appropriate for bergamot's workload — NNTP I/O is the bottleneck on most systems, not CPU.

For embedded/NAS deployments with limited cores, tokio's work-stealing scheduler still performs well by multiplexing many connections onto a small thread pool.

---

## Concurrency Model

### Actor Pattern for Queue Coordinator

The `QueueCoordinator` owns all mutable queue state and receives commands via an `mpsc` channel. This avoids locks on the hot path:

```
┌─────────┐  QueueCommand   ┌──────────────────┐
│ API     │ ──────────────► │                  │
│ handler │                 │ QueueCoordinator │
└─────────┘                 │  (actor task)    │
                            │                  │
┌─────────┐  QueueCommand   │  owns:           │
│ Feed    │ ──────────────► │  - queue state   │
│ coord.  │                 │  - download jobs │
└─────────┘                 │  - disk state    │
                            └────────┬─────────┘
                                     │
                            spawns download tasks
                                     │
                            ┌────────▼─────────┐
                            │ Download Workers  │
                            │ (tokio tasks)     │
                            └──────────────────┘
```

```rust
pub enum QueueCommand {
    AddNzb { nzb_data: Vec<u8>, params: AddNzbParams, reply: oneshot::Sender<Result<u32>> },
    AddUrl { url: String, params: AddNzbParams, reply: oneshot::Sender<Result<u32>> },
    Pause { id: u32 },
    Resume { id: u32 },
    Remove { id: u32 },
    MoveToTop { id: u32 },
    SetPriority { id: u32, priority: i32 },
    GetStatus { reply: oneshot::Sender<QueueStatus> },
    ArticleCompleted { file_id: u32, article_index: u32, data: Vec<u8>, crc: u32 },
    ArticleFailed { file_id: u32, article_index: u32, error: String },
    FileCompleted { file_id: u32 },
    NzbCompleted { nzb_id: u32 },
    Shutdown,
}

pub struct QueueCoordinator {
    rx: mpsc::Receiver<QueueCommand>,
    state: QueueState,
    disk_state: Arc<DiskState>,
}

impl QueueCoordinator {
    pub async fn run(mut self) {
        while let Some(cmd) = self.rx.recv().await {
            match cmd {
                QueueCommand::Shutdown => break,
                cmd => self.handle(cmd).await,
            }
        }
    }
}
```

Advantages of the actor pattern:

- **No locks** — the coordinator owns all mutable state exclusively.
- **No deadlocks** — impossible by construction.
- **Backpressure** — bounded channels prevent queue flooding.
- **Testable** — the coordinator can be driven by a synthetic channel in unit tests.

### Shared Read-Heavy State with `Arc<RwLock>`

Configuration and server stats are read frequently but written rarely:

```rust
pub type SharedConfig = Arc<RwLock<Config>>;
pub type SharedServerStats = Arc<RwLock<ServerStats>>;
```

Multiple readers (API handlers, download tasks) can hold read locks concurrently. Writers (config reload, stats update) acquire exclusive access — these are infrequent operations.

### Channel Pattern Summary

| Pattern | Use Case |
|---------|----------|
| `mpsc::channel` | Commands to QueueCoordinator, post-processing jobs |
| `oneshot::channel` | Request-reply (API → coordinator → API response) |
| `broadcast::channel` | Shutdown signal to all tasks |
| `Arc<RwLock<T>>` | Read-heavy shared state (config, server stats) |
| `Arc<Mutex<T>>` | Write-heavy shared state (log ring buffer) |

### Download Worker Concurrency

Each NNTP connection runs as an independent tokio task. Multiple NZBs, files, and articles are downloaded simultaneously:

```
NZB-1  ─── File A ─── Article 1 ──► Connection (Server 1)
       │           └── Article 2 ──► Connection (Server 1)
       └── File B ─── Article 1 ──► Connection (Server 2)

NZB-2  ─── File C ─── Article 1 ──► Connection (Server 1)
```

The coordinator limits total active downloads to the sum of configured connections across all servers. Within that limit, it prioritizes articles by NZB priority, file type, and file order.

---

## Error Handling

### Library Errors: `thiserror`

Each library crate defines its own error types using `thiserror`. This produces well-typed, descriptive errors that callers can match on:

```rust
// bergamot-nntp/src/error.rs
use thiserror::Error;

#[derive(Debug, Error)]
pub enum NntpError {
    #[error("connection failed: {0}")]
    Connect(#[from] std::io::Error),

    #[error("TLS handshake failed: {0}")]
    Tls(#[from] rustls::Error),

    #[error("authentication failed: {reason}")]
    Auth { reason: String },

    #[error("unexpected response {code}: {message}")]
    Protocol { code: u16, message: String },

    #[error("article not found: {message_id}")]
    ArticleNotFound { message_id: String },

    #[error("timeout after {0:?}")]
    Timeout(std::time::Duration),
}
```

Each crate's error enum covers that crate's failure modes. `#[from]` conversions are used sparingly — only where the source error unambiguously maps to one variant.

### Application Errors: `anyhow`

The binary crate and top-level orchestration use `anyhow` for ergonomic error propagation with context:

```rust
// bergamot/src/main.rs
use anyhow::{Context, Result};

async fn run(config: Config) -> Result<()> {
    let pool = NntpPool::new(&config.servers)
        .await
        .context("failed to initialize NNTP connection pool")?;
    // ...
    Ok(())
}
```

The boundary is clear: library crates return typed errors via `thiserror`, the binary crate wraps them with `anyhow` for context and propagation.

---

## Key Crate Dependencies

| Crate | Version | Purpose | Used By |
|-------|---------|---------|---------|
| `tokio` | 1.x | Async runtime, timers, sync primitives, signal handling | all crates |
| `axum` | 0.8 | HTTP server framework for web UI + API | bergamot-api |
| `hyper` | 1.x | HTTP/1.1 (used transitively by axum) | bergamot-api |
| `reqwest` | 0.12 | HTTP client for URL fetches, feed polling | bergamot-queue, bergamot-api |
| `quick-xml` | 0.37 | Streaming XML parser (NZB, RSS, XML-RPC) | bergamot-nzb, bergamot-api |
| `serde` | 1.x | Serialization framework | all crates |
| `serde_json` | 1.x | JSON serialization (config, state, JSON-RPC) | bergamot-core, bergamot-api, bergamot-queue |
| `rustls` | 0.23 | TLS implementation (no OpenSSL dependency) | bergamot-nntp |
| `tokio-rustls` | 0.26 | Tokio integration for rustls | bergamot-nntp |
| `webpki-roots` | 0.26 | Mozilla CA root certificates | bergamot-nntp |
| `crc32fast` | 1.x | Hardware-accelerated CRC32 (SSE4.2 / ARMv8) | bergamot-yenc, bergamot-post |
| `clap` | 4.x | CLI argument parsing with derive macros | bergamot-cli |
| `tracing` | 0.1 | Structured, async-aware logging facade | all crates |
| `tracing-subscriber` | 0.3 | Logging subscriber implementation, filtering | bergamot (binary) |
| `thiserror` | 2.x | Derive `Error` impls for library error types | bergamot-core, bergamot-nntp, bergamot-nzb, bergamot-yenc |
| `anyhow` | 1.x | Ergonomic error handling in binary crate | bergamot (binary) |
| `chrono` | 0.4 | Date/time handling, log rotation | bergamot-core, bergamot-post |
| `regex` | 1.x | Filename extraction, feed filter pattern matching | bergamot-nzb, bergamot-queue |
| `walkdir` | 2.x | Recursive directory scanning (extensions, incoming NZBs) | bergamot-post, bergamot-queue |
| `flate2` | 1.x | Gzip decompression for `.nzb.gz` files | bergamot-nzb |
| `proptest` | 1.x | Property-based testing (dev-dependency) | bergamot-yenc, bergamot-nzb |

### Why rustls over OpenSSL

- **No system dependency** — statically linked, no `libssl-dev` required at build or runtime.
- **Memory safety** — pure Rust implementation eliminates a class of vulnerabilities.
- **Cross-compilation** — works on any target without a C toolchain for OpenSSL.
- **Performance** — competitive with OpenSSL for the TLS workloads bergamot encounters (long-lived connections, bulk data transfer).

---

## Testing Strategy

### Unit Tests

Each module includes `#[cfg(test)]` tests co-located with the code:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_yenc_decode_single_part() {
        let input = include_bytes!("../testdata/single_part.yenc");
        let result = decode(input).unwrap();
        assert_eq!(result.crc, 0xDEADBEEF);
    }

    #[tokio::test]
    async fn test_nntp_auth() {
        let (mut server, addr) = mock_nntp_server().await;
        server.expect_authinfo("user", "pass", "281 Ok");
        let conn = NntpConnection::connect(addr).await.unwrap();
        conn.authenticate("user", "pass").await.unwrap();
    }
}
```

### Integration Tests with Mock NNTP Server

A mock NNTP server enables end-to-end testing without a real Usenet provider. The mock server speaks the NNTP protocol over TCP and serves pre-loaded articles:

```rust
// tests/integration/download.rs
#[tokio::test]
async fn test_full_download_flow() {
    let server = MockNntpServer::start().await;
    server.add_article("article-1@test", yenc_encode(b"hello"));

    let config = test_config(server.addr());
    let app = bergamot::App::new(config).await.unwrap();

    let nzb_id = app.add_nzb(test_nzb()).await.unwrap();
    app.wait_for_completion(nzb_id).await;

    let history = app.history().await;
    assert_eq!(history[0].total_status, TotalStatus::Success);
}
```

The mock server supports:

- Multiple connections (tests concurrent downloads)
- Article-not-found responses (tests server failover)
- Delayed responses (tests timeout handling)
- Connection drops (tests reconnection logic)

### Property-Based Testing

Parsers (yEnc, NZB XML, feed filters) use `proptest` for fuzzing:

```rust
use proptest::prelude::*;

proptest! {
    #[test]
    fn yenc_roundtrip(data: Vec<u8>) {
        let encoded = yenc_encode(&data);
        let decoded = yenc_decode(&encoded).unwrap();
        prop_assert_eq!(data, decoded.data);
    }

    #[test]
    fn nzb_parse_never_panics(data: Vec<u8>) {
        let _ = NzbParser::new().parse(std::io::Cursor::new(&data));
    }
}
```

### Test Organization

| Level | Location | Scope |
|-------|----------|-------|
| Unit | `src/*.rs` (`#[cfg(test)]`) | Single function or struct |
| Integration | `tests/integration/` | Full pipeline with mock NNTP |
| Property | Inline with unit tests | Parser robustness, roundtrip correctness |
| Benchmark | `benches/` | yEnc decode throughput, CRC32 performance |

---

## API Compatibility

bergamot maintains backward compatibility with nzbget's RPC API so that existing tools (Sonarr, Radarr, Lidarr, NZBHydra, etc.) work without modification:

```
┌─────────────┐     JSON-RPC       ┌───────────┐
│  Sonarr     │ ──────────────────► │  bergamot     │
│  Radarr     │     XML-RPC        │  /jsonrpc  │
│  NZBHydra   │ ──────────────────► │  /xmlrpc   │
└─────────────┘                     └───────────┘
```

Both JSON-RPC and XML-RPC endpoints implement the same method set. The API layer translates between RPC calls and `QueueCommand` messages sent to the coordinator.

```rust
// bergamot-api/src/jsonrpc.rs
pub async fn handle_jsonrpc(
    State(state): State<AppState>,
    Json(request): Json<JsonRpcRequest>,
) -> Json<JsonRpcResponse> {
    let result = match request.method.as_str() {
        "version" => json!({"Version": env!("CARGO_PKG_VERSION")}),
        "status" => {
            let (tx, rx) = oneshot::channel();
            state.queue_tx.send(QueueCommand::GetStatus { reply: tx }).await.unwrap();
            let status = rx.await.unwrap();
            serde_json::to_value(status).unwrap()
        }
        "append" => {
            // Parse NZB from base64 parameter, add to queue
            todo!()
        }
        _ => json!({"error": "unknown method"}),
    };
    Json(JsonRpcResponse { result, id: request.id })
}
```

### Compatibility Priorities

1. **Method names and parameters** — identical to nzbget's documented API.
2. **Response format** — field names, types, and value ranges match nzbget.
3. **Authentication** — HTTP Basic Auth with the same username/password scheme.
4. **Status values** — numeric status codes and string representations match nzbget.

---

## Graceful Shutdown

Shutdown is coordinated via a `broadcast` channel. All subsystems subscribe to the shutdown signal and drain their work before exiting:

```
                    SIGINT / SIGTERM / API command
                              │
                              ▼
                    ┌──────────────────┐
                    │ broadcast::send  │
                    │ (shutdown signal) │
                    └──────────────────┘
                              │
              ┌───────────────┼───────────────┐
              ▼               ▼               ▼
      ┌──────────────┐ ┌───────────┐  ┌────────────┐
      │ QueueCoord.  │ │ API Server│  │ Feed Coord. │
      │ drain active │ │ stop      │  │ stop        │
      │ downloads    │ │ accepting │  │ polling     │
      └──────┬───────┘ └───────────┘  └────────────┘
             │
             ▼
      ┌──────────────┐
      │ Save state   │
      │ to disk      │
      └──────────────┘
```

```rust
pub async fn run(config: Config) -> anyhow::Result<()> {
    let (shutdown_tx, _) = broadcast::channel::<()>(1);

    // Spawn subsystems
    let queue_handle = tokio::spawn({
        let rx = shutdown_tx.subscribe();
        async move { queue_coordinator.run(rx).await }
    });
    let api_handle = tokio::spawn({
        let rx = shutdown_tx.subscribe();
        async move { api_server.run(rx).await }
    });
    let feed_handle = tokio::spawn({
        let rx = shutdown_tx.subscribe();
        async move { feed_coordinator.run(rx).await }
    });

    // Wait for shutdown signal
    tokio::signal::ctrl_c().await?;
    tracing::info!("shutdown signal received, draining...");
    let _ = shutdown_tx.send(());

    // Wait for all subsystems to finish
    let _ = tokio::join!(queue_handle, api_handle, feed_handle);

    tracing::info!("shutdown complete");
    Ok(())
}
```

### Shutdown Sequence

1. Receive SIGINT/SIGTERM or API `shutdown` command.
2. Broadcast shutdown signal to all subsystems.
3. **QueueCoordinator**: stop scheduling new articles, wait for in-flight downloads to complete (with timeout), save queue state.
4. **API Server**: stop accepting new connections, finish in-flight requests.
5. **FeedCoordinator**: cancel pending polls.
6. **PostProcessor**: finish current job (or abort with state save).
7. **DiskState**: final flush of queue, history, stats.
8. Exit cleanly.

If in-flight downloads do not complete within a configurable timeout (default: 30 seconds), they are cancelled and their articles are marked as `Undefined` so they will be retried on next startup.

---

## Configuration

### CLI Arguments (clap)

```rust
use clap::Parser;

#[derive(Parser)]
#[command(name = "bergamot", about = "Usenet downloader")]
pub struct Cli {
    /// Path to configuration file
    #[arg(short, long, default_value = "/etc/bergamot/bergamot.conf")]
    pub config: PathBuf,

    /// Override data directory
    #[arg(long)]
    pub data_dir: Option<PathBuf>,

    /// Run in daemon mode
    #[arg(short, long)]
    pub daemon: bool,

    /// Remote server command (for nzbget compatibility)
    #[arg(subcommand)]
    pub command: Option<RemoteCommand>,
}

#[derive(clap::Subcommand)]
pub enum RemoteCommand {
    /// List queue
    #[command(name = "-L")]
    List,
    /// Pause downloads
    #[command(name = "-P")]
    Pause,
    /// Resume downloads
    #[command(name = "-U")]
    Unpause,
}
```

### Configuration File

nzbget-compatible INI-style configuration, parsed with a custom parser (not the `config` crate) for full compatibility:

```rust
pub struct Config {
    pub main_dir: PathBuf,
    pub dest_dir: PathBuf,
    pub temp_dir: PathBuf,
    pub queue_dir: PathBuf,
    pub servers: Vec<ServerConfig>,
    pub categories: Vec<CategoryConfig>,
    pub feeds: Vec<FeedConfig>,
    pub extensions: ExtensionConfig,
    pub log: LogConfig,
    pub download: DownloadConfig,
    pub post_process: PostProcessConfig,
}
```

### Environment Variable Overrides

Environment variables override config file values using the `NZBG_` prefix:

```
NZBG_DESTDIR=/downloads bergamot
```

---

## Binary Size & Performance

### Release Profile

```toml
# Cargo.toml (workspace root)
[profile.release]
opt-level = 3
lto = "fat"
codegen-units = 1
strip = true
panic = "abort"
```

| Setting | Effect |
|---------|--------|
| `lto = "fat"` | Full link-time optimization across all crates |
| `codegen-units = 1` | Better optimization at cost of compile time |
| `strip = true` | Remove debug symbols from binary |
| `panic = "abort"` | Smaller binary (no unwinding tables) |

### Performance Considerations

- **yEnc decoding**: inner loop uses SIMD-friendly byte operations; consider `packed_simd` or manual NEON/SSE intrinsics if profiling shows it as a bottleneck.
- **CRC32**: `crc32fast` uses hardware acceleration (SSE4.2 / ARMv8) automatically.
- **Connection pool**: pre-established connections avoid handshake latency; configurable pool size per server.
- **Article cache**: optional in-memory write cache reduces small random writes to disk.
- **Async I/O**: tokio's multi-threaded scheduler multiplexes many connections onto a small thread pool, reducing context-switch overhead vs. thread-per-connection.
