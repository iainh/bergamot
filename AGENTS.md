# Bergamot — Agent Guidelines

Bergamot is a ground-up Rust reimplementation of [NZBGet](https://nzbget.com), an efficient Usenet binary downloader. It downloads files described by NZB files from Usenet (NNTP) servers, reassembles multi-part articles, decodes yEnc payloads, and orchestrates post-processing (PAR2 repair, archive extraction, script execution).

## Build & Test Commands

```sh
cargo build                        # Debug build
cargo build --release              # Release build (binary at target/release/bergamot)
cargo test --workspace             # Run all tests (unit, integration, property-based)
cargo test -p bergamot-yenc        # Test a single crate
cargo fmt                          # Format code
cargo fmt -- --check               # Check formatting without modifying
cargo clippy -- -D warnings        # Lint (treat warnings as errors)
```

All three of `cargo fmt -- --check`, `cargo clippy -- -D warnings`, and `cargo test --workspace` must pass before any change is considered complete.

## Workspace Structure

This is a Cargo workspace (`resolver = "2"`) with 16 crates under `crates/`. The binary entry point is in `src/main.rs` but most logic lives in the workspace crates. Key crates:

| Crate | Role |
|---|---|
| `bergamot` | Binary wiring, CLI (clap), download workers, daemon mode |
| `bergamot-core` | Shared data models (`NzbInfo`, `FileInfo`, etc.) |
| `bergamot-config` | INI-style config parser (NZBGet-compatible) |
| `bergamot-nntp` | NNTP protocol state machine, connection pool |
| `bergamot-yenc` | SIMD-accelerated yEnc decoder (NEON + SSE2) |
| `bergamot-par2` | PAR2 parser, verifier, repairer (SIMD GF math) |
| `bergamot-queue` | Queue coordinator actor (mpsc command pattern) |
| `bergamot-server` | HTTP server (axum), JSON-RPC & XML-RPC API |
| `bergamot-postproc` | Post-processing pipeline (PAR2, unpack, scripts) |
| `bergamot-nntp-stub` | Stub NNTP server for integration testing |

See `docs/design/02-rust-architecture.md` for the full dependency graph and architecture details.

## Code Conventions

### Rust Edition & Toolchain
- **Edition**: 2024
- **Minimum Rust**: 1.85+
- **Async runtime**: Tokio (multi-threaded)

### Error Handling
- **Library crates**: Use `thiserror` with domain-specific error enums (e.g., `ConfigError`, `NntpError`). Keep `#[from]` conversions sparse and unambiguous.
- **Application/orchestration crates**: Use `anyhow::Result` with `.context()` / `.with_context()` for propagation. Use `anyhow::bail!` for early returns.
- Never use `.unwrap()` in non-test code unless the invariant is provably safe and commented.

### Sans-I/O Pattern
- Protocol and decoding logic is implemented as **sans-I/O state machines** — pure functions that consume typed inputs and produce typed outputs, with no direct network or file I/O. A separate driver layer handles the actual I/O.
- `NntpMachine` (`bergamot-nntp/src/machine.rs`) takes `Input` (response lines, body chunks) and produces `Output` (send command, upgrade TLS, protocol events). `NntpConnection` in `protocol.rs` drives it.
- `YencDecoder` (`bergamot-yenc/src/decode.rs`) follows the same pattern for binary decoding.
- This makes protocol logic unit-testable without sockets or async runtimes. Prefer this pattern when adding new protocol or parser logic.

### Concurrency
- The `QueueCoordinator` uses an **actor pattern** — it owns mutable state and receives `QueueCommand` messages via `mpsc`. Do not add locks around queue state; send commands through the channel instead.
- Shutdown is coordinated via `broadcast` channel. All subsystems subscribe to the shutdown signal.
- Use `tokio::sync` primitives (`mpsc`, `broadcast`, `RwLock`, `Mutex`, `oneshot`), not `std::sync` equivalents, in async code.

### API Compatibility
- bergamot must remain compatible with NZBGet's JSON-RPC and XML-RPC API so that Sonarr, Radarr, and other tools work without modification. Do not rename RPC methods, change response field names/types, or alter authentication behaviour without explicit discussion.

### Naming
- Crate names: `bergamot-{subsystem}` (kebab-case)
- Modules: snake_case
- Types/traits: PascalCase
- Follow existing patterns in neighboring files before introducing new conventions.

## Testing

- **Unit tests**: `#[cfg(test)] mod tests` blocks co-located in source files.
- **Integration tests**: `tests/` directories at crate roots. The main E2E suite is `crates/bergamot/tests/e2e_flow.rs`.
- **Property-based tests**: `proptest` for parsers and math (yEnc roundtrip, NZB fuzzing, Galois field properties).
- **Mocks**: Prefer hand-written mock structs implementing traits over mocking frameworks. The `bergamot-nntp-stub` crate provides a full stub NNTP server using JSON fixtures from `fixtures/nntp/`.
- **Async tests**: Use `#[tokio::test]` with timeouts.

When adding new functionality, include tests. When fixing a bug, add a regression test.

## Documentation

- Architecture docs are in `docs/design/`.
- Planning and gap analysis are in `docs/planning/`.
- Configuration reference is in `docs/implementation/06-configuration.md`.

Consult these before making architectural decisions — they document deliberate design choices.

## Configuration

bergamot uses an INI-style config file compatible with `nzbget.conf`. The parser is custom (not the `config` crate) to maintain full NZBGet compatibility, including variable interpolation (`${MainDir}`). See `bergamot.conf.sample` for all options.

## Nix

The project includes a `flake.nix` for reproducible builds, Docker images, and a NixOS module. Use `nix develop` for a dev shell with all dependencies.
