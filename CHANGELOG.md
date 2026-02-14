# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/),
and this project adheres to [Semantic Versioning](https://semver.org/).

## [0.1.0] - 2026-02-14

### Added

- Async NNTP downloading with Tokio, multi-server failover, and connection pooling
- Weighted Fair Queuing (WFQ) download scheduler with EWMA-based throughput tracking to distribute articles across servers proportional to observed speed
- SIMD-accelerated yEnc decoding with CRC-32 verification
- NZB XML parsing
- Queue management with priority, pause/resume, and duplicate detection
- Native Rust PAR2 verification and repair with SIMD-accelerated GF multiplication
- Automatic archive unpacking (rar, 7z, zip)
- Post-processing pipeline with extension script support (Python, Bash, etc.)
- JSON-RPC and XML-RPC API compatible with NZBGet clients
- Bundled web UI (via rust_embed)
- RSS/Atom feed polling with filter rules
- Cron-like task scheduler with commands for pause/resume downloads, pause/resume post-processing, pause/resume scan, speed limiting, server activation, feed fetching, and extension execution
- On-disk persistence and crash recovery
- Drop-in compatibility with `nzbget.conf` configuration files
- HTTPS with auto-generated TLS certificates
- Three-tier authentication (Control, Restricted, Add-only)
- Download quotas (daily/monthly) with auto-pause/resume
- PAR2-based file deobfuscation
- Daemon mode with PID file support
- NZB directory scanner and disk space monitoring
- Docker image and NixOS module

[0.1.0]: https://github.com/iainh/bergamot/releases/tag/v0.1.0
