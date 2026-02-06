# nzbg

A ground-up Rust reimplementation of [NZBGet](https://nzbget.com), the efficient Usenet binary downloader originally written in C++.

nzbg downloads files described by NZB files from Usenet (NNTP) servers, reassembles multi-part articles, decodes yEnc payloads, and orchestrates post-processing (PAR2 repair, archive extraction, script execution). It exposes a JSON-RPC / XML-RPC API consumed by its built-in web UI and by third-party tools such as Sonarr, Radarr, and SABnzbd integrations.

## Features

- Async I/O powered by Tokio for high-throughput downloading
- SIMD-accelerated yEnc decoding
- PAR2 verification and repair
- Automatic archive unpacking (rar, 7z, zip)
- RSS/Atom feed polling with filter rules
- JSON-RPC and XML-RPC API compatible with NZBGet clients
- Drop-in compatibility with existing `nzbget.conf` configuration files
- Extension script support (Python, Bash, etc.)
- Designed to run on low-resource hardware (NAS, Raspberry Pi)

## Building

### Prerequisites

- Rust 1.85+ (2024 edition)

With Nix:

```sh
nix develop
```

### Compile

```sh
cargo build --release
```

The binary is produced at `target/release/nzbg`.

### Run tests

```sh
cargo test --workspace
```

## Usage

```
nzbg [OPTIONS]

Options:
  -c, --config <FILE>          Path to configuration file
  -D, --foreground             Run in foreground (do not daemonize)
      --pidfile <FILE>         Write PID to file
  -l, --log-level <LEVEL>      Log level: debug, info, warning, error [default: info]
  -h, --help                   Print help
  -V, --version                Print version
```

### Quick start

1. Create a configuration file (INI-style, compatible with `nzbget.conf`):

    ```ini
    MainDir=~/downloads
    DestDir=${MainDir}/completed
    TempDir=${MainDir}/tmp
    QueueDir=${MainDir}/queue
    NzbDir=${MainDir}/nzb

    ControlIP=127.0.0.1
    ControlPort=6789
    ControlUsername=nzbget
    ControlPassword=tegbzn6789

    Server1.Active=yes
    Server1.Host=news.example.com
    Server1.Port=563
    Server1.Encryption=yes
    Server1.Connections=8
    Server1.Username=user
    Server1.Password=secret
    ```

2. Run nzbg:

    ```sh
    nzbg --foreground --config nzbg.conf
    ```

3. Open the web UI at `http://127.0.0.1:6789` or point your favourite Usenet tool (Sonarr, Radarr, etc.) at the JSON-RPC API.

See [docs/08-configuration.md](docs/08-configuration.md) for the full list of configuration options.

## Project structure

| Crate | Responsibility |
|---|---|
| `nzbg` | Binary entry point, CLI, top-level wiring |
| `nzbg-core` | Shared data structures and error types |
| `nzbg-config` | Configuration file parsing and validation |
| `nzbg-nntp` | NNTP protocol and connection pooling |
| `nzbg-yenc` | yEnc decoding and CRC-32 verification |
| `nzbg-nzb` | NZB XML parsing |
| `nzbg-queue` | Queue coordinator and download orchestration |
| `nzbg-postproc` | PAR2 verification, unpacking, script execution |
| `nzbg-server` | HTTP server, JSON-RPC / XML-RPC handlers |
| `nzbg-feed` | RSS/Atom feed polling and matching |
| `nzbg-scheduler` | Cron-like task scheduler and background services |
| `nzbg-diskstate` | On-disk persistence and crash recovery |
| `nzbg-extension` | Extension/script runner interface |
| `nzbg-logging` | Logging, message buffer, and history tracking |

## Acknowledgements

nzbg is inspired by and aims for compatibility with [NZBGet](https://nzbget.com), the battle-tested Usenet downloader created by the NZBGet team. Without their years of work building an excellent downloader and documenting its architecture, configuration format, and API, this project would not exist.

## License

TBD
