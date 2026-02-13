# NNTP download engine

> **Crate module layout** (`crates/bergamot-nntp/src/`):
>
> | Module | Purpose |
> |--------|---------|
> | `lib.rs` | Public API re-exports |
> | `error.rs` | `NntpError` enum |
> | `model.rs` | `NewsServer`, `Encryption`, `IpVersion`, `NntpResponse` |
> | `protocol.rs` | `NntpConnection`, `NntpStream`, `BodyReader`, TLS setup |
> | `machine.rs` | Sans-I/O state machine driving command/response exchanges |
> | `pool.rs` | `ServerPool` connection pool and factory traits |
> | `scheduler.rs` | Weighted fair-queuing article scheduler (EWMA-based server selection) |
> | `speed.rs` | `SpeedLimiter` / `SpeedLimiterHandle` — token-bucket rate limiting |
>
> The illustrative code below is simplified for reference; see the source modules
> for the full implementation.

## NNTP protocol overview (RFC 3977)

NNTP (Network News Transfer Protocol) is a TCP-based text protocol for accessing
Usenet articles. A binary downloader only needs a small subset of the full protocol.

### Required commands

| Command | Purpose | Example |
|---------|---------|---------|
| `AUTHINFO USER <user>` | Send username for authentication | `AUTHINFO USER bob` |
| `AUTHINFO PASS <pass>` | Send password for authentication | `AUTHINFO PASS secret` |
| `GROUP <name>` | Select a newsgroup, get article range | `GROUP alt.binaries.linux` |
| `ARTICLE <message-id>` | Retrieve full article (headers + body) | `ARTICLE <abc@news.example.com>` |
| `BODY <message-id>` | Retrieve article body only | `BODY <abc@news.example.com>` |
| `STAT <message-id>` | Check if article exists (no transfer) | `STAT <abc@news.example.com>` |
| `QUIT` | Close connection gracefully | `QUIT` |

### Typical session

```
S: 200 news.example.com NNRP Service Ready (posting ok)
C: AUTHINFO USER bob
S: 381 Password required
C: AUTHINFO PASS secret
S: 281 Authentication accepted
C: GROUP alt.binaries.linux
S: 211 50000 1000 51000 alt.binaries.linux
C: BODY <part1of50.abc123@news.example.com>
S: 222 0 <part1of50.abc123@news.example.com>
S: [body lines...]
S: .
C: QUIT
S: 205 Connection closing
```

---

## Response codes

| Code | Meaning | Context |
|------|---------|---------|
| 200 | Service available, posting allowed | Connection greeting |
| 201 | Service available, posting prohibited | Connection greeting |
| 205 | Connection closing | Response to `QUIT` |
| 211 | Group selected (`count first last name`) | Response to `GROUP` |
| 220 | Article follows (headers + body) | Response to `ARTICLE` |
| 222 | Body follows | Response to `BODY` |
| 223 | Article exists | Response to `STAT` |
| 281 | Authentication accepted | Response to `AUTHINFO PASS` |
| 381 | Password required | Response to `AUTHINFO USER` |
| 411 | No such newsgroup | Response to `GROUP` |
| 420 | Current article number is invalid | No article selected |
| 423 | No article with that number | Article number not in range |
| 430 | No article with that message-ID | Article not found on server |
| 480 | Authentication required | Server demands credentials |
| 502 | Permission denied / service unavailable | Auth failed or access denied |

---

## Article transfer format

NNTP uses a dot-stuffed text format inherited from SMTP:

1. **Response line**: `222 0 <message-id>` (status code, article number, message-ID)
2. **Body lines**: each line terminated by `\r\n`
3. **Dot-stuffing**: any body line starting with `.` has an extra `.` prepended
4. **Termination**: a line containing only `.\r\n` signals end of body

```
222 0 <abc@news.example.com>\r\n
=ybegin part=1 line=128 size=739811 name=file.rar\r\n
=ypart begin=1 end=128000\r\n
[yEnc-encoded binary data lines]\r\n
=yend size=128000 part=1 pcrc32=abcd1234\r\n
.\r\n
```

The parser must:
- Strip the leading `.` from any line that starts with `..`
- Detect the lone `.` terminator
- Stream body lines to the decoder without buffering the entire article

---

## NewsServer configuration

```rust
#[derive(Debug, Clone)]
pub struct NewsServer {
    pub id: u32,
    pub name: String,
    pub active: bool,
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
    pub encryption: Encryption,
    pub cipher: Option<String>,
    pub connections: u32,
    pub retention: u32,          // days; 0 = unlimited
    pub level: u32,              // 0 = primary, 1+ = fill/backup
    pub optional: bool,          // don't fail download if this server can't provide
    pub group: u32,              // load-balancing group (0 = default)
    pub join_group: bool,        // send GROUP command before ARTICLE/BODY
    pub ip_version: IpVersion,
    pub cert_verification: bool, // verify TLS certificate
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Encryption {
    None,
    Tls,        // TLS on connect (typically port 563)
    StartTls,   // STARTTLS upgrade after connect
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum IpVersion {
    Auto,
    IPv4Only,
    IPv6Only,
}
```

---

## Connection pool architecture

```
┌─────────────────────────────────────────────────────────┐
│                      ServerPool                         │
│                                                         │
│  Level 0 (Primary)                                      │
│  ┌─────────────────────┐  ┌─────────────────────┐      │
│  │ Server A (Group 0)  │  │ Server B (Group 0)  │      │
│  │ ┌────┐┌────┐┌────┐  │  │ ┌────┐┌────┐       │      │
│  │ │Conn││Conn││Conn│  │  │ │Conn││Conn│       │      │
│  │ └────┘└────┘└────┘  │  │ └────┘└────┘       │      │
│  │   max_connections=3  │  │  max_connections=2  │      │
│  └─────────────────────┘  └─────────────────────┘      │
│                                                         │
│  Level 1 (Fill / Backup)                                │
│  ┌─────────────────────┐                                │
│  │ Server C (Group 1)  │  ← optional = true             │
│  │ ┌────┐              │                                │
│  │ │Conn│              │                                │
│  │ └────┘              │                                │
│  │   max_connections=1  │                                │
│  └─────────────────────┘                                │
│                                                         │
│  Level 2 (Tertiary Block Account)                       │
│  ┌─────────────────────┐                                │
│  │ Server D (Group 0)  │                                │
│  │ ┌────┐┌────┐        │                                │
│  │ │Conn││Conn│        │                                │
│  │ └────┘└────┘        │                                │
│  │   max_connections=2  │                                │
│  └─────────────────────┘                                │
└─────────────────────────────────────────────────────────┘
```

### Connection lifecycle

```
                  ┌─────────┐
       ┌─────────│  Create  │
       │         └────┬─────┘
       │              │ TCP connect + optional TLS + auth
       │              ▼
       │         ┌─────────┐
       │    ┌───▶│  Idle   │◀──────────────┐
       │    │    └────┬─────┘               │
       │    │         │ checkout()          │
       │    │         ▼                     │
       │    │    ┌─────────┐     success    │
       │    │    │ In Use  │───────────────▶│
       │    │    └────┬─────┘  return()     │
       │    │         │                     │
       │    │         │ error / timeout     │
       │    │         ▼                     │
       │    │    ┌─────────┐               │
       │    │    │ Closed  │               │
       │    │    └────┬─────┘               │
       │    │         │ reconnect?          │
       │    └─────────┘                     │
       │                                    │
       │         idle_timeout               │
       └────────────────────────────────────┘
```

---

## Connection selection algorithm

When a download task needs a connection, the pool selects one using this strategy:

```
fn select_connection(pool, article, want_server) -> Option<Connection>:
    for level in 0..max_level:
        for server in servers_at_level(level):
            if !server.active:
                continue
            if server is temporarily blocked (recent auth/connect failure):
                continue
            if server.connections are all in use:
                continue
            if server.retention > 0 and article.age_days > server.retention:
                continue
            if want_server is set and server.id != want_server:
                continue  // prefer specific server for retries
            if server.group != 0:
                // load-balancing: only use if fewest active downloads in group
                if not least_loaded_in_group(server):
                    continue
            return checkout_or_create_connection(server)
    return None  // all servers exhausted, article failed
```

### Server levels and groups

- **Level 0** — Primary servers, tried first for every article.
- **Level 1+** — Fill/backup servers, tried only after all lower levels fail.
- **Groups** — Servers in the same group are load-balanced (round-robin or
  least-connections). Servers in group `0` are independent.
- **Optional** — If `optional = true`, failure on this server does not count as
  a permanent failure. The article may still succeed via other servers.

---

## Article download flow

Each article download runs as an async task:

```
┌──────────────┐
│ ArticleTask  │
│              │
│  article_id  │
│  message_id  │
│  want_server │
└──────┬───────┘
       │
       ▼
  ┌─────────────────┐     No connection available
  │ Get Connection  │────────────────────────────▶ Queue / Wait
  │ from ServerPool │
  └────────┬────────┘
           │ got connection
           ▼
  ┌─────────────────┐     join_group == true
  │ GROUP command?  │────────────────────────────▶ Send GROUP
  └────────┬────────┘                              (if group changed)
           │
           ▼
  ┌─────────────────┐
  │ BODY <msg-id>   │──────▶ Send command, read response code
  └────────┬────────┘
           │ 222 Body follows
           ▼
  ┌─────────────────┐
  │ Stream body to  │──────▶ Lines streamed to yEnc/UU decoder
  │ decoder         │        (dot-unstuffing applied)
  └────────┬────────┘
           │ termination dot received
           ▼
  ┌─────────────────┐
  │ Validate CRC    │──────▶ Compare computed CRC32 with yEnc trailer
  └────────┬────────┘
           │
      ┌────┴────┐
      │ Success │──────▶ Return connection to pool (idle)
      └─────────┘       Emit decoded segment data

      ┌─────────┐
      │ Failure │──────▶ See retry strategy below
      └─────────┘
```

---

## Retry strategy

```
                    ┌─────────────┐
                    │   Error     │
                    └──────┬──────┘
                           │
              ┌────────────┼────────────────┐──────────────┐
              ▼            ▼                ▼              ▼
        ┌───────────┐ ┌──────────┐  ┌────────────┐  ┌──────────┐
        │ Connection│ │ 430 Not  │  │ CRC        │  │ Auth     │
        │ Error     │ │ Found    │  │ Mismatch   │  │ Error    │
        │ (timeout, │ │          │  │            │  │ (480,502)│
        │ reset)    │ │          │  │            │  │          │
        └─────┬─────┘ └────┬─────┘  └─────┬──────┘  └────┬─────┘
              │             │              │              │
              ▼             ▼              ▼              ▼
        Retry same    Try next        Retry once     Block server
        server        server/level    on same         for backoff
        (up to N      (article may    server; if      period; try
        retries)      exist on        still bad,      other servers
                      fill server)    try next
```

| Error Type | Action | Max Retries |
|------------|--------|-------------|
| Connection error (TCP reset, timeout) | Retry same server with new connection | `ArticleRetries` (default 3) |
| 430 Not Found | Mark article missing on this server; try next server, then next level | All servers |
| CRC mismatch | Retry same server once (corruption); then try next server | 1 + all servers |
| Auth error (480, 502) | Block server for exponential backoff; try other servers | N/A |

---

## Speed limiting

When `DownloadRate` is configured (bytes/sec), the engine throttles across all
connections using a token bucket:

```rust
use std::time::{Duration, Instant};
use tokio::time::sleep;

pub struct SpeedLimiter {
    rate: u64,             // bytes per second; 0 = unlimited
    tokens: f64,           // available bytes
    last_refill: Instant,
}

impl SpeedLimiter {
    pub fn new(rate_bytes_per_sec: u64) -> Self {
        Self {
            rate: rate_bytes_per_sec,
            tokens: rate_bytes_per_sec as f64,
            last_refill: Instant::now(),
        }
    }

    pub async fn acquire(&mut self, bytes: u64) {
        if self.rate == 0 {
            return;
        }

        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.last_refill = now;
        self.tokens = (self.tokens + elapsed * self.rate as f64)
            .min(self.rate as f64 * 2.0);

        self.tokens -= bytes as f64;
        if self.tokens < 0.0 {
            let delay = Duration::from_secs_f64(-self.tokens / self.rate as f64);
            sleep(delay).await;
            self.tokens = 0.0;
            self.last_refill = Instant::now();
        }
    }
}
```

---

## Quota management

Monthly and daily download quotas prevent exceeding ISP or provider limits:

```rust
use chrono::{Datelike, Local, NaiveDate};

pub struct QuotaManager {
    pub monthly_quota: u64,       // bytes; 0 = unlimited
    pub daily_quota: u64,         // bytes; 0 = unlimited
    pub first_day_of_month: u32,  // 1-28, when monthly quota resets
    month_used: u64,
    day_used: u64,
    current_day: NaiveDate,
    current_month_start: NaiveDate,
}

impl QuotaManager {
    pub fn add_usage(&mut self, bytes: u64) {
        self.roll_over_if_needed();
        self.month_used += bytes;
        self.day_used += bytes;
    }

    pub fn quota_reached(&self) -> bool {
        (self.monthly_quota > 0 && self.month_used >= self.monthly_quota)
            || (self.daily_quota > 0 && self.day_used >= self.daily_quota)
    }

    fn roll_over_if_needed(&mut self) {
        let today = Local::now().date_naive();
        if today != self.current_day {
            self.day_used = 0;
            self.current_day = today;
        }
        if today.day() == self.first_day_of_month && today != self.current_month_start {
            self.month_used = 0;
            self.current_month_start = today;
        }
    }
}
```

---

## NntpConnection

```rust
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;

pub enum NntpStream {
    Plain(BufReader<TcpStream>),
    Tls(BufReader<TlsStream<TcpStream>>),
}

pub struct NntpConnection {
    pub server_id: u32,
    stream: NntpStream,
    current_group: Option<String>,
    authenticated: bool,
}

pub struct NntpResponse {
    pub code: u16,
    pub message: String,
}

impl NntpConnection {
    /// Establish a new connection to the NNTP server.
    pub async fn connect(server: &NewsServer) -> Result<Self, NntpError> {
        let tcp = TcpStream::connect((server.host.as_str(), server.port)).await?;

        let stream = match server.encryption {
            Encryption::Tls => {
                let tls = tls_connect(tcp, &server.host, server.cert_verification).await?;
                NntpStream::Tls(BufReader::new(tls))
            }
            Encryption::StartTls => {
                let mut plain = BufReader::new(tcp);
                let greeting = read_response(&mut plain).await?;
                expect_code(&greeting, &[200, 201])?;
                // Send STARTTLS, upgrade connection
                send_command(&mut plain, "STARTTLS").await?;
                let resp = read_response(&mut plain).await?;
                expect_code(&resp, &[382])?;
                let tls = tls_connect(plain.into_inner(), &server.host, server.cert_verification).await?;
                NntpStream::Tls(BufReader::new(tls))
            }
            Encryption::None => {
                NntpStream::Plain(BufReader::new(tcp))
            }
        };

        let mut conn = NntpConnection {
            server_id: server.id,
            stream,
            current_group: None,
            authenticated: false,
        };

        // Read greeting if not already consumed by STARTTLS path
        if server.encryption != Encryption::StartTls {
            let greeting = conn.read_response().await?;
            expect_code(&greeting, &[200, 201])?;
        }

        Ok(conn)
    }

    /// Authenticate with AUTHINFO USER/PASS.
    pub async fn authenticate(
        &mut self,
        username: &str,
        password: &str,
    ) -> Result<(), NntpError> {
        self.send_command(&format!("AUTHINFO USER {username}")).await?;
        let resp = self.read_response().await?;

        match resp.code {
            281 => {
                self.authenticated = true;
                return Ok(());
            }
            381 => {} // password required, continue
            _ => return Err(NntpError::AuthFailed(resp.message)),
        }

        self.send_command(&format!("AUTHINFO PASS {password}")).await?;
        let resp = self.read_response().await?;
        match resp.code {
            281 => {
                self.authenticated = true;
                Ok(())
            }
            _ => Err(NntpError::AuthFailed(resp.message)),
        }
    }

    /// Select a newsgroup. Caches the current group to skip redundant commands.
    pub async fn join_group(&mut self, group: &str) -> Result<(), NntpError> {
        if self.current_group.as_deref() == Some(group) {
            return Ok(());
        }
        self.send_command(&format!("GROUP {group}")).await?;
        let resp = self.read_response().await?;
        expect_code(&resp, &[211])?;
        self.current_group = Some(group.to_string());
        Ok(())
    }

    /// Fetch the body of an article by message-ID.
    /// Returns a reader that streams dot-unstuffed body lines.
    pub async fn fetch_body(&mut self, message_id: &str) -> Result<BodyReader, NntpError> {
        self.send_command(&format!("BODY <{message_id}>")).await?;
        let resp = self.read_response().await?;

        match resp.code {
            222 => Ok(BodyReader::new(&mut self.stream)),
            430 => Err(NntpError::ArticleNotFound(message_id.to_string())),
            480 => Err(NntpError::AuthRequired),
            _ => Err(NntpError::UnexpectedResponse(resp.code, resp.message)),
        }
    }

    /// Check if an article exists without transferring it.
    pub async fn stat(&mut self, message_id: &str) -> Result<bool, NntpError> {
        self.send_command(&format!("STAT <{message_id}>")).await?;
        let resp = self.read_response().await?;
        match resp.code {
            223 => Ok(true),
            430 => Ok(false),
            _ => Err(NntpError::UnexpectedResponse(resp.code, resp.message)),
        }
    }

    /// Gracefully close the connection.
    pub async fn quit(&mut self) -> Result<(), NntpError> {
        self.send_command("QUIT").await?;
        let _ = self.read_response().await; // 205 expected, but don't fail on error
        Ok(())
    }

    async fn send_command(&mut self, cmd: &str) -> Result<(), NntpError> {
        let line = format!("{cmd}\r\n");
        match &mut self.stream {
            NntpStream::Plain(s) => s.get_mut().write_all(line.as_bytes()).await?,
            NntpStream::Tls(s) => s.get_mut().write_all(line.as_bytes()).await?,
        }
        Ok(())
    }

    async fn read_response(&mut self) -> Result<NntpResponse, NntpError> {
        let mut line = String::new();
        match &mut self.stream {
            NntpStream::Plain(s) => s.read_line(&mut line).await?,
            NntpStream::Tls(s) => s.read_line(&mut line).await?,
        };

        let code = line
            .get(..3)
            .and_then(|s| s.parse::<u16>().ok())
            .ok_or_else(|| NntpError::ProtocolError("invalid response line".into()))?;

        let message = line[3..].trim().to_string();
        Ok(NntpResponse { code, message })
    }
}
```

---

## TLS support (rustls)

TLS connections use `tokio-rustls` with `rustls` as the backend (no OpenSSL dependency):

```rust
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::{ClientConfig, RootCertStore, pki_types::ServerName};

async fn tls_connect(
    tcp: TcpStream,
    hostname: &str,
    verify_cert: bool,
) -> Result<tokio_rustls::client::TlsStream<TcpStream>, NntpError> {
    let mut root_store = RootCertStore::empty();
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    let config = if verify_cert {
        ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth()
    } else {
        ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerifier))
            .with_no_client_auth()
    };

    let connector = TlsConnector::from(Arc::new(config));
    let server_name = ServerName::try_from(hostname.to_string())
        .map_err(|_| NntpError::TlsError(format!("invalid hostname: {hostname}")))?;

    let tls_stream = connector.connect(server_name, tcp).await
        .map_err(|e| NntpError::TlsError(e.to_string()))?;

    Ok(tls_stream)
}
```

```toml
[dependencies]
tokio = { version = "1", features = ["net", "io-util", "time", "rt-multi-thread"] }
tokio-rustls = "0.26"
rustls = "0.23"
webpki-roots = "0.26"
```

---

## NntpError

```rust
#[derive(Debug, thiserror::Error)]
pub enum NntpError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("TLS error: {0}")]
    TlsError(String),

    #[error("Authentication failed: {0}")]
    AuthFailed(String),

    #[error("Authentication required")]
    AuthRequired,

    #[error("Article not found: {0}")]
    ArticleNotFound(String),

    #[error("Unexpected response {0}: {1}")]
    UnexpectedResponse(u16, String),

    #[error("Protocol error: {0}")]
    ProtocolError(String),

    #[error("CRC mismatch: expected {expected:#010x}, got {actual:#010x}")]
    CrcMismatch { expected: u32, actual: u32 },

    #[error("Connection timed out")]
    Timeout,

    #[error("Server blocked: {0}")]
    ServerBlocked(String),

    #[error("Quota exceeded")]
    QuotaExceeded,
}
```
