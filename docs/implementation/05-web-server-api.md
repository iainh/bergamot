# Web server & RPC API

## Architecture overview

```
                    HTTP Request
                         │
                         ▼
                  ┌─────────────┐
                  │  HTTP Server │  (axum)
                  │  (TLS opt.)  │
                  └──────┬──────┘
                         │
                  ┌──────┴──────┐
                  │   Auth &    │
                  │  Routing    │
                  └──────┬──────┘
                         │
          ┌──────────────┼──────────────┐
          │              │              │
          ▼              ▼              ▼
    ┌───────────┐  ┌───────────┐  ┌───────────┐
    │  Static   │  │    RPC    │  │    API     │
    │  Files    │  │ Endpoint  │  │ Shortcuts  │
    │ (Web UI)  │  │           │  │            │
    └───────────┘  └─────┬─────┘  └───────────┘
                         │
              ┌──────────┼──────────┐
              │          │          │
              ▼          ▼          ▼
          JSON-RPC   XML-RPC   JSON-P-RPC
```

## Recommended framework: axum

axum provides async, tower-based middleware, built-in extractors, and good performance:

```rust
use axum::{
    Router,
    routing::{get, post},
    middleware,
};
use std::sync::Arc;
use tokio::net::TcpListener;

pub struct WebServer {
    config: Arc<Config>,
    app_state: Arc<AppState>,
}

pub struct AppState {
    pub queue: Arc<Mutex<DownloadQueue>>,
    pub history: Arc<Mutex<HistoryManager>>,
    pub config: Arc<Config>,
    pub stats: Arc<Stats>,
}

impl WebServer {
    pub async fn run(&self) -> Result<(), Box<dyn std::error::Error>> {
        let app = Router::new()
            .route("/xmlrpc", post(handle_xmlrpc))
            .route("/jsonrpc", post(handle_jsonrpc))
            .route("/jsonprpc", get(handle_jsonprpc))
            // API shortcuts: /{username}:{password}/jsonrpc/{method}
            .route("/{credentials}/jsonrpc/{method}", get(handle_api_shortcut))
            .fallback_service(tower_http::services::ServeDir::new(&self.config.web_dir))
            .layer(middleware::from_fn_with_state(
                self.app_state.clone(),
                auth_middleware,
            ))
            .with_state(self.app_state.clone());

        let bind_addr = format!("{}:{}", self.config.control_ip, self.config.control_port);
        let listener = TcpListener::bind(&bind_addr).await?;
        axum::serve(listener, app).await?;

        Ok(())
    }
}
```

## Server configuration

```rust
pub struct ServerConfig {
    /// IP to bind to. "0.0.0.0" for all interfaces, "127.0.0.1" for localhost only.
    pub control_ip: String,
    /// Port to listen on (default: 6789).
    pub control_port: u16,
    /// Enable HTTPS.
    pub secure_control: bool,
    /// Path to TLS certificate file.
    pub secure_cert: Option<PathBuf>,
    /// Path to TLS private key file.
    pub secure_key: Option<PathBuf>,
    /// Path to the web UI static files directory.
    pub web_dir: PathBuf,
    /// Enable form-based authentication (vs. HTTP Basic only).
    pub form_auth: bool,
    /// IP addresses that bypass authentication.
    pub authorized_ips: Vec<String>,
}
```

## Authentication

### Three access levels

nzbget defines three credential pairs, each granting a different level of access:

| Level | Config Keys | Allowed Operations |
|-------|------------|--------------------|
| **Control** | `ControlUsername` / `ControlPassword` | Full access to all API methods |
| **Restricted** | `RestrictedUsername` / `RestrictedPassword` | Read-only: status, listgroups, history, log. No config changes. |
| **Add** | `AddUsername` / `AddPassword` | Only `append` (add downloads) and `version` |

### Auth flow

```rust
use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::Response,
};
use base64::Engine;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessLevel {
    Control,
    Restricted,
    Add,
    Denied,
}

async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let config = &state.config;

    // Check authorized IPs (bypass auth entirely)
    let remote_ip = extract_remote_ip(&request);
    if config.authorized_ips.iter().any(|ip| ip == &remote_ip) {
        return Ok(next.run(request).await);
    }

    // Check URL-embedded credentials: /{username}:{password}/...
    let access = extract_url_credentials(&request)
        .and_then(|(user, pass)| authenticate(&user, &pass, config))
        // Fall back to HTTP Basic Auth
        .or_else(|| {
            extract_basic_auth(&request)
                .and_then(|(user, pass)| authenticate(&user, &pass, config))
        })
        .unwrap_or(AccessLevel::Denied);

    if access == AccessLevel::Denied {
        return Err(StatusCode::UNAUTHORIZED);
    }

    // Store access level in request extensions for method-level checks
    let mut request = request;
    request.extensions_mut().insert(access);

    Ok(next.run(request).await)
}

fn authenticate(username: &str, password: &str, config: &ServerConfig) -> Option<AccessLevel> {
    if username == config.control_username && password == config.control_password {
        Some(AccessLevel::Control)
    } else if username == config.restricted_username && password == config.restricted_password {
        Some(AccessLevel::Restricted)
    } else if username == config.add_username && password == config.add_password {
        Some(AccessLevel::Add)
    } else {
        None
    }
}

fn extract_basic_auth(request: &Request) -> Option<(String, String)> {
    let header = request.headers().get("Authorization")?.to_str().ok()?;
    let encoded = header.strip_prefix("Basic ")?;
    let decoded = base64::engine::general_purpose::STANDARD.decode(encoded).ok()?;
    let decoded = String::from_utf8(decoded).ok()?;
    let (user, pass) = decoded.split_once(':')?;
    Some((user.to_string(), pass.to_string()))
}
```

### Access control per method

```rust
fn required_access(method: &str) -> AccessLevel {
    match method {
        "version" => AccessLevel::Add,
        "append" | "scan" => AccessLevel::Add,

        "status" | "sysinfo" | "systemhealth"
        | "listgroups" | "listfiles" | "history"
        | "log" | "loadlog" | "servervolumes"
        | "config" | "configtemplates" | "loadconfig" => AccessLevel::Restricted,

        _ => AccessLevel::Control,
    }
}
```

## RPC protocols

bergamot supports three RPC protocol variants on the same endpoint:

### JSON-RPC 2.0

**Request** (`POST /jsonrpc`):
```json
{
    "jsonrpc": "2.0",
    "method": "status",
    "params": [],
    "id": 1
}
```

**Response**:
```json
{
    "jsonrpc": "2.0",
    "result": { "RemainingSizeLo": 1234, "RemainingSizeHi": 0, "DownloadRate": 5000000 },
    "id": 1
}
```

### XML-RPC

**Request** (`POST /xmlrpc`):
```xml
<?xml version="1.0"?>
<methodCall>
  <methodName>status</methodName>
  <params/>
</methodCall>
```

**Response**:
```xml
<?xml version="1.0"?>
<methodResponse>
  <params>
    <param>
      <value>
        <struct>
          <member>
            <name>RemainingSizeLo</name>
            <value><i4>1234</i4></value>
          </member>
          <member>
            <name>RemainingSizeHi</name>
            <value><i4>0</i4></value>
          </member>
        </struct>
      </value>
    </param>
  </params>
</methodResponse>
```

### JSON-P-RPC

For cross-origin browser requests. Wraps JSON-RPC in a JSONP callback:

**Request** (`GET /jsonprpc?method=status&callback=cb123`):

**Response**:
```javascript
cb123({"jsonrpc":"2.0","result":{"RemainingSizeLo":1234},"id":0});
```

### RPC dispatcher

```rust
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: Option<String>,
    pub method: String,
    pub params: serde_json::Value,
    pub id: Option<serde_json::Value>,
}

#[derive(Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub result: Option<serde_json::Value>,
    pub error: Option<JsonRpcError>,
    pub id: serde_json::Value,
}

#[derive(Serialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
}

async fn dispatch_rpc(
    method: &str,
    params: &serde_json::Value,
    state: &AppState,
) -> Result<serde_json::Value, JsonRpcError> {
    match method {
        "version" => Ok(serde_json::json!(state.version())),
        "status" => Ok(serde_json::to_value(state.status().await)?),
        "listgroups" => Ok(serde_json::to_value(state.list_groups().await)?),
        "listfiles" => {
            let nzb_id = params.get(0).and_then(|v| v.as_i64()).unwrap_or(0);
            Ok(serde_json::to_value(state.list_files(nzb_id).await)?)
        }
        "history" => {
            let hidden = params.get(0).and_then(|v| v.as_bool()).unwrap_or(false);
            Ok(serde_json::to_value(state.history(hidden).await)?)
        }
        "append" => handle_append(params, state).await,
        "editqueue" => handle_editqueue(params, state).await,
        "scan" => handle_scan(params, state).await,
        "shutdown" => handle_shutdown(state).await,
        "reload" => handle_reload(state).await,
        "log" => handle_log(params, state).await,
        "writelog" => handle_writelog(params, state).await,
        "loadlog" => handle_loadlog(params, state).await,
        "config" => Ok(serde_json::to_value(state.config_values())?),
        "loadconfig" => Ok(serde_json::to_value(state.load_config()?)?),
        "saveconfig" => handle_saveconfig(params, state).await,
        "configtemplates" => Ok(serde_json::to_value(state.config_templates()?)?),
        "servervolumes" => Ok(serde_json::to_value(state.server_volumes().await)?),
        "resetservervolume" => handle_reset_server_volume(params, state).await,
        "sysinfo" => Ok(serde_json::to_value(state.sysinfo())?),
        "systemhealth" => Ok(serde_json::to_value(state.system_health().await)?),
        "loadextensions" => Ok(serde_json::to_value(state.load_extensions()?)?),
        "downloadextension" => handle_download_extension(params, state).await,
        "updateextension" => handle_update_extension(params, state).await,
        "deleteextension" => handle_delete_extension(params, state).await,
        "testextension" => handle_test_extension(params, state).await,
        "testserver" => handle_testserver(params, state).await,
        "testserverspeed" => handle_testserverspeed(params, state).await,
        "testdiskspeed" => handle_testdiskspeed(params, state).await,
        "testnetworkspeed" => handle_testnetworkspeed(params, state).await,
        _ => Err(JsonRpcError {
            code: -32601,
            message: format!("Method not found: {method}"),
        }),
    }
}
```

## API methods reference

### Program control

| Method | Parameters | Returns | Description |
|--------|-----------|---------|-------------|
| `version` | — | `String` | Server version string |
| `shutdown` | — | `bool` | Initiate graceful shutdown |
| `reload` | — | `bool` | Reload config from disk |

### Queue management

| Method | Parameters | Returns | Description |
|--------|-----------|---------|-------------|
| `listgroups` | `NumberOfLogEntries: i32` | `Vec<GroupResponse>` | List NZBs in download queue |
| `listfiles` | `NzbID: i32, ...` | `Vec<FileResponse>` | List files within an NZB |
| `history` | `Hidden: bool` | `Vec<HistoryResponse>` | List completed/failed downloads |
| `append` | `Filename, Content, Category, ...` | `i32` (NZB ID) | Add NZB to download queue |
| `editqueue` | `Command, Param, IDs[]` | `bool` | Edit queue entries (pause, delete, move, etc.) |
| `scan` | `ScanNzbDir: bool` | `bool` | Scan incoming NZB directory |

### Status & logging

| Method | Parameters | Returns | Description |
|--------|-----------|---------|-------------|
| `status` | — | `StatusResponse` | Server status and statistics |
| `sysinfo` | — | `SysInfoResponse` | System information |
| `systemhealth` | — | `Vec<HealthEntry>` | System health checks |
| `log` | `IdFrom: i32, NumberOfEntries: i32` | `Vec<LogEntry>` | Retrieve log messages |
| `writelog` | `Kind: String, Text: String` | `bool` | Write message to log |
| `loadlog` | `NzbID: i32, IdFrom: i32, NumberOfEntries: i32` | `Vec<LogEntry>` | Retrieve per-NZB log |
| `servervolumes` | — | `Vec<ServerVolume>` | Download volume stats per server |
| `resetservervolume` | `ServerId: i32, Counter: String` | `bool` | Reset volume counter |

### Configuration

| Method | Parameters | Returns | Description |
|--------|-----------|---------|-------------|
| `config` | — | `Vec<ConfigEntry>` | Current running config |
| `loadconfig` | — | `Vec<ConfigEntry>` | Config as saved on disk |
| `saveconfig` | `ConfigEntries[]` | `bool` | Save config to disk |
| `configtemplates` | — | `Vec<ConfigTemplate>` | Config option definitions and defaults |

### Extensions

| Method | Parameters | Returns | Description |
|--------|-----------|---------|-------------|
| `loadextensions` | — | `Vec<ExtensionInfo>` | List installed extensions |
| `downloadextension` | `URL: String` | `bool` | Download and install extension |
| `updateextension` | `ExtName: String` | `bool` | Update extension |
| `deleteextension` | `ExtName: String` | `bool` | Delete extension |
| `testextension` | `ExtName: String, ...` | `String` | Run extension in test mode |

### Tests

| Method | Parameters | Returns | Description |
|--------|-----------|---------|-------------|
| `testserver` | `Host, Port, Username, ...` | `String` | Test news server connection |
| `testserverspeed` | `Host, Port, ...` | `String` | Test news server download speed |
| `testdiskspeed` | `Path: String` | `String` | Test disk write speed |
| `testnetworkspeed` | — | `String` | Test network throughput |

## StatusResponse

```rust
#[derive(Debug, Serialize)]
pub struct StatusResponse {
    // Remaining download size (Lo/Hi split for XML-RPC compat)
    #[serde(rename = "RemainingSizeLo")]
    pub remaining_size_lo: u32,
    #[serde(rename = "RemainingSizeHi")]
    pub remaining_size_hi: u32,
    #[serde(rename = "RemainingSizeMB")]
    pub remaining_size_mb: u64,

    // Forced remaining size (articles that must be downloaded)
    #[serde(rename = "ForcedSizeLo")]
    pub forced_size_lo: u32,
    #[serde(rename = "ForcedSizeHi")]
    pub forced_size_hi: u32,
    #[serde(rename = "ForcedSizeMB")]
    pub forced_size_mb: u64,

    // Download speed
    #[serde(rename = "DownloadRate")]
    pub download_rate: u64,         // bytes/sec
    #[serde(rename = "AverageDownloadRate")]
    pub average_download_rate: u64,
    #[serde(rename = "DownloadLimit")]
    pub download_limit: u64,        // 0 = unlimited

    // Article stats
    #[serde(rename = "ArticleCacheLo")]
    pub article_cache_lo: u32,
    #[serde(rename = "ArticleCacheHi")]
    pub article_cache_hi: u32,
    #[serde(rename = "ArticleCacheMB")]
    pub article_cache_mb: u64,

    // Timing
    #[serde(rename = "DownloadTimeSec")]
    pub download_time_sec: u64,
    #[serde(rename = "UpTimeSec")]
    pub uptime_sec: u64,
    #[serde(rename = "DownloadPaused")]
    pub download_paused: bool,
    #[serde(rename = "PostPaused")]
    pub post_paused: bool,
    #[serde(rename = "ScanPaused")]
    pub scan_paused: bool,

    // Totals
    #[serde(rename = "FreeDiskSpaceLo")]
    pub free_disk_space_lo: u32,
    #[serde(rename = "FreeDiskSpaceHi")]
    pub free_disk_space_hi: u32,
    #[serde(rename = "FreeDiskSpaceMB")]
    pub free_disk_space_mb: u64,

    #[serde(rename = "DownloadedSizeLo")]
    pub downloaded_size_lo: u32,
    #[serde(rename = "DownloadedSizeHi")]
    pub downloaded_size_hi: u32,
    #[serde(rename = "DownloadedSizeMB")]
    pub downloaded_size_mb: u64,

    // Queue counts
    #[serde(rename = "ServerStandBy")]
    pub server_standby: bool,
    #[serde(rename = "ThreadCount")]
    pub thread_count: u32,
    #[serde(rename = "PostJobCount")]
    pub post_job_count: u32,
    #[serde(rename = "NewsServers")]
    pub news_servers: Vec<NewsServerStatus>,

    #[serde(rename = "ServerTime")]
    pub server_time: u64,          // Unix timestamp
    #[serde(rename = "ResumeTime")]
    pub resume_time: u64,          // 0 if not scheduled

    #[serde(rename = "QueueScriptCount")]
    pub queue_script_count: u32,
}

#[derive(Debug, Serialize)]
pub struct NewsServerStatus {
    #[serde(rename = "ID")]
    pub id: u32,
    #[serde(rename = "Active")]
    pub active: bool,
}
```

## GroupResponse (listgroups)

```rust
#[derive(Debug, Serialize)]
pub struct GroupResponse {
    #[serde(rename = "NZBID")]
    pub nzb_id: i64,
    #[serde(rename = "NZBName")]
    pub nzb_name: String,
    #[serde(rename = "NZBNicename")]
    pub nzb_nicename: String,
    #[serde(rename = "NZBFilename")]
    pub nzb_filename: String,
    #[serde(rename = "DestDir")]
    pub dest_dir: String,
    #[serde(rename = "FinalDir")]
    pub final_dir: String,
    #[serde(rename = "Category")]
    pub category: String,
    #[serde(rename = "URL")]
    pub url: String,

    // Size fields (Lo/Hi split)
    #[serde(rename = "FileSizeLo")]
    pub file_size_lo: u32,
    #[serde(rename = "FileSizeHi")]
    pub file_size_hi: u32,
    #[serde(rename = "FileSizeMB")]
    pub file_size_mb: u64,
    #[serde(rename = "RemainingSizeLo")]
    pub remaining_size_lo: u32,
    #[serde(rename = "RemainingSizeHi")]
    pub remaining_size_hi: u32,
    #[serde(rename = "RemainingSizeMB")]
    pub remaining_size_mb: u64,
    #[serde(rename = "PausedSizeLo")]
    pub paused_size_lo: u32,
    #[serde(rename = "PausedSizeHi")]
    pub paused_size_hi: u32,
    #[serde(rename = "PausedSizeMB")]
    pub paused_size_mb: u64,

    // Counts
    #[serde(rename = "FileCount")]
    pub file_count: u32,
    #[serde(rename = "RemainingFileCount")]
    pub remaining_file_count: u32,
    #[serde(rename = "RemainingParCount")]
    pub remaining_par_count: u32,
    #[serde(rename = "TotalArticles")]
    pub total_articles: u32,
    #[serde(rename = "SuccessArticles")]
    pub success_articles: u32,
    #[serde(rename = "FailedArticles")]
    pub failed_articles: u32,

    // Status
    #[serde(rename = "Status")]
    pub status: String,           // "QUEUED", "DOWNLOADING", "PAUSED", etc.
    #[serde(rename = "MinPostTime")]
    pub min_post_time: u64,
    #[serde(rename = "MaxPostTime")]
    pub max_post_time: u64,
    #[serde(rename = "MaxPriority")]
    pub max_priority: i32,
    #[serde(rename = "ActiveDownloads")]
    pub active_downloads: u32,

    // Health
    #[serde(rename = "Health")]
    pub health: u32,              // 0–1000 (1000 = fully healthy)
    #[serde(rename = "CriticalHealth")]
    pub critical_health: u32,     // threshold below which repair is impossible

    // Post-processing
    #[serde(rename = "PostTotalTimeSec")]
    pub post_total_time_sec: u64,
    #[serde(rename = "PostStageTimeSec")]
    pub post_stage_time_sec: u64,
    #[serde(rename = "PostStageProgress")]
    pub post_stage_progress: u32,
    #[serde(rename = "PostInfoText")]
    pub post_info_text: String,

    // Timestamps
    #[serde(rename = "DownloadTimeSec")]
    pub download_time_sec: u64,
    #[serde(rename = "DownloadedSizeLo")]
    pub downloaded_size_lo: u32,
    #[serde(rename = "DownloadedSizeHi")]
    pub downloaded_size_hi: u32,
    #[serde(rename = "DownloadedSizeMB")]
    pub downloaded_size_mb: u64,

    // Parameters and logs
    #[serde(rename = "Parameters")]
    pub parameters: Vec<ParameterEntry>,
    #[serde(rename = "ServerStats")]
    pub server_stats: Vec<ServerStatEntry>,
    #[serde(rename = "Log")]
    pub log: Vec<LogEntry>,
}

#[derive(Debug, Serialize)]
pub struct ParameterEntry {
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Value")]
    pub value: String,
}

#[derive(Debug, Serialize)]
pub struct ServerStatEntry {
    #[serde(rename = "ServerID")]
    pub server_id: u32,
    #[serde(rename = "SuccessArticles")]
    pub success_articles: u32,
    #[serde(rename = "FailedArticles")]
    pub failed_articles: u32,
}

#[derive(Debug, Serialize)]
pub struct LogEntry {
    #[serde(rename = "ID")]
    pub id: i64,
    #[serde(rename = "Kind")]
    pub kind: String,             // "INFO", "WARNING", "ERROR", "DETAIL", "DEBUG"
    #[serde(rename = "Time")]
    pub time: u64,
    #[serde(rename = "Text")]
    pub text: String,
}
```

## 64-bit integer Hi/Lo splitting

XML-RPC only supports 32-bit integers (`<i4>`). To represent 64-bit values (file sizes, byte counts), nzbget splits them into `Hi` and `Lo` 32-bit halves:

```rust
pub fn split_i64(value: u64) -> (u32, u32) {
    let hi = (value >> 32) as u32;
    let lo = (value & 0xFFFFFFFF) as u32;
    (hi, lo)
}

pub fn combine_i64(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | (lo as u64)
}

/// Helper to populate Lo/Hi/MB fields from a byte count
pub struct SizeFields {
    pub lo: u32,
    pub hi: u32,
    pub mb: u64,
}

impl From<u64> for SizeFields {
    fn from(bytes: u64) -> Self {
        let (hi, lo) = split_i64(bytes);
        Self {
            lo,
            hi,
            mb: bytes / (1024 * 1024),
        }
    }
}
```

For JSON-RPC, these fields are still included for backwards compatibility, but clients can also use the `MB` variants or compute the full value from `Hi`/`Lo`.

## Gzip compression

Compress large responses when the client supports it:

```rust
use tower_http::compression::CompressionLayer;

fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/jsonrpc", post(handle_jsonrpc))
        .route("/xmlrpc", post(handle_xmlrpc))
        // ... other routes ...
        .layer(CompressionLayer::new().gzip(true))
        .with_state(state)
}
```

## CORS support

Allow browser-based API clients from different origins:

```rust
use tower_http::cors::{CorsLayer, Any};

fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        // ... routes ...
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
        .with_state(state)
}
```
