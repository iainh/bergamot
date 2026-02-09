# Configuration System

bergamot uses an INI-style configuration file compatible with nzbget's `nzbget.conf` format. The configuration controls every aspect of the application: paths, security, news servers, categories, download behavior, post-processing, and logging.

## Config File Format

### Basic Syntax

```ini
# This is a comment
# Empty lines are ignored

MainDir=/downloads
DestDir=${MainDir}/completed
TempDir=${MainDir}/tmp
```

Lines starting with `#` are comments. Options use `Key=Value` syntax with no spaces around the `=`. Values are trimmed of leading/trailing whitespace.

### Variable Interpolation

Values may reference other options using `${OptionName}` syntax. The tilde character `~` in paths expands to the user's home directory.

```ini
MainDir=~/downloads
DestDir=${MainDir}/completed    # resolves to ~/downloads/completed
InterDir=${MainDir}/intermediate
QueueDir=${MainDir}/queue
TempDir=${MainDir}/tmp
LogFile=${DestDir}/bergamot.log
```

Interpolation is resolved at load time in a single pass. Forward references (referencing an option defined later in the file) resolve against defaults or previously loaded values.

### Numbered Sections

Servers, categories, RSS feeds, and scheduled tasks use numbered option groups rather than INI section headers:

```ini
# Server 1
Server1.Active=yes
Server1.Host=news.example.com
Server1.Port=563
Server1.Encryption=yes
Server1.Connections=8
Server1.Username=user
Server1.Password=secret

# Server 2 (fill server)
Server2.Active=yes
Server2.Host=fill.example.com
Server2.Port=563
Server2.Level=1
Server2.Optional=yes
Server2.Connections=4

# Categories
Category1.Name=Movies
Category1.DestDir=${MainDir}/movies

Category2.Name=TV
Category2.DestDir=${MainDir}/tv
Category2.Aliases=Television, Series

# Scheduled tasks
Task1.Time=03:00
Task1.WeekDays=1-7
Task1.Command=PauseDownload
Task1.Param=

# RSS feeds
Feed1.Name=My Indexer
Feed1.URL=https://indexer.example.com/rss?apikey=xxx
Feed1.Interval=15
Feed1.Filter=size:>100MB
```

Numbers must be sequential starting from 1. Gaps are not permitted.

## Configuration Options Reference

### System / Paths

| Option | Default | Description |
|--------|---------|-------------|
| `MainDir` | `~/downloads` | Root directory for all other default paths. |
| `DestDir` | `${MainDir}/dst` | Final destination for completed downloads. |
| `InterDir` | `${MainDir}/inter` | Intermediate directory for downloads being post-processed. Empty string disables (files stay in `DestDir`). |
| `NzbDir` | `${MainDir}/nzb` | Directory scanned for incoming `.nzb` files. |
| `QueueDir` | `${MainDir}/queue` | Internal queue state and download progress. |
| `TempDir` | `${MainDir}/tmp` | Temporary directory for article assembly and partial files. |
| `WebDir` | (built-in) | Path to the web UI static files. Empty uses the embedded UI. |
| `ScriptDir` | `${MainDir}/scripts` | Directory containing post-processing and other extension scripts. |
| `LogFile` | `${DestDir}/bergamot.log` | Path to the log file. Empty string disables file logging. |
| `ConfigTemplate` | (built-in) | Path to the config template file used by the web UI for option descriptions. |
| `RequiredDir` | (empty) | Comma-separated list of directories that must be available before downloading starts. Useful for network mounts. |
| `CertStore` | (system default) | Path to CA certificate bundle for TLS verification. Empty uses the system store. |

### Security

| Option | Default | Description |
|--------|---------|-------------|
| `ControlIP` | `0.0.0.0` | IP address the server listens on. Use `127.0.0.1` for local-only access. |
| `ControlPort` | `6789` | Port for HTTP/API access. |
| `ControlUsername` | `nzbget` | Username for full access via web UI and API. |
| `ControlPassword` | `tegbzn6789` | Password for full access. |
| `RestrictedUsername` | (empty) | Username for restricted access (no settings changes). |
| `RestrictedPassword` | (empty) | Password for restricted access. |
| `AddUsername` | (empty) | Username for add-only access (can only add downloads). |
| `AddPassword` | (empty) | Password for add-only access. |
| `FormAuth` | `no` | Use form-based login instead of HTTP basic auth. |
| `SecureControl` | `no` | Enable HTTPS for the control interface. |
| `SecurePort` | `6791` | Port for HTTPS access when `SecureControl` is enabled. |
| `SecureCert` | (empty) | Path to TLS certificate file (PEM format). |
| `SecureKey` | (empty) | Path to TLS private key file (PEM format). |
| `AuthorizedIP` | (empty) | Comma-separated list of IP addresses or CIDR ranges that can connect without authentication. |
| `CertCheck` | `no` | Verify TLS certificates when connecting to news servers. |
| `DaemonUsername` | `root` | System user to run as when started as root (drops privileges). |
| `UMask` | `1000` | File creation mask (octal). `1000` means use the system default. |

### News Servers (Numbered: `Server1.*`, `Server2.*`, ...)

| Option | Default | Description |
|--------|---------|-------------|
| `Active` | `yes` | Whether this server is enabled. |
| `Name` | (empty) | Display name for this server. |
| `Level` | `0` | Priority level. `0` is tried first; higher levels are tried if lower levels fail. |
| `Optional` | `no` | If `yes`, articles not found on this server are not counted as failures. |
| `Group` | `0` | Server group ID. Servers with the same non-zero group share a failure count. |
| `Host` | (empty) | Server hostname or IP address. |
| `Port` | `119` | Server port. Common: `119` (plain), `443` or `563` (TLS). |
| `Username` | (empty) | NNTP authentication username. |
| `Password` | (empty) | NNTP authentication password. |
| `Encryption` | `no` | Enable TLS encryption for this server. |
| `Cipher` | (empty) | Allowed TLS cipher list. Empty uses system defaults. |
| `Connections` | `1` | Maximum simultaneous connections to this server. |
| `Retention` | `0` | Server article retention in days. `0` means unlimited. |
| `JoinGroup` | `no` | Send NNTP `GROUP` command before fetching articles. |
| `IpVersion` | `auto` | IP version: `auto`, `ipv4`, or `ipv6`. |
| `CertVerification` | `no` | Verify the server's TLS certificate against the CA store. |
| `Notes` | (empty) | Free-form notes about this server (not used by the application). |

### Categories (Numbered: `Category1.*`, `Category2.*`, ...)

| Option | Default | Description |
|--------|---------|-------------|
| `Name` | (empty) | Category name used when adding NZBs. |
| `DestDir` | (empty) | Destination directory override for this category. Empty uses `DestDir/CategoryName`. |
| `Unpack` | `yes` | Whether to unpack archives for downloads in this category. |
| `Extensions` | (empty) | Comma-separated list of extension scripts to run for this category. |
| `Aliases` | (empty) | Comma-separated alternative names that map to this category. |

### Download

| Option | Default | Description |
|--------|---------|-------------|
| `AppendCategoryDir` | `yes` | Append the category name as a subdirectory to `DestDir`. |
| `NzbDirInterval` | `5` | Seconds between scans of `NzbDir` for new files. |
| `NzbDirFileAge` | `60` | Minimum file age in seconds before processing an NZB from `NzbDir`. Prevents reading partially written files. |
| `DupeCheck` | `yes` | Check for duplicate NZBs by name and content hash. |
| `FlushQueue` | `yes` | Flush download queue to disk after each change. |
| `ContinuePartial` | `yes` | Resume partially downloaded files after restart. |
| `PropagationDelay` | `0` | Minutes to wait after NZB is added before starting download. Allows Usenet propagation. |
| `ArticleCache` | `0` | Article cache size in MB. `0` disables caching and writes directly. |
| `DirectWrite` | `yes` | Write decoded articles directly to the output file at the correct offset. Reduces disk I/O. |
| `WriteBuffer` | `0` | Write buffer size in KB. `0` uses the system default. |
| `FileNaming` | `auto` | File naming mode: `auto` (from NZB), `nzb` (use NZB filename), `article` (from article headers). |
| `ReorderFiles` | `yes` | Reorder files within an NZB for optimal PAR2 repair (download verification files first). |
| `PostStrategy` | `balanced` | Post-processing strategy: `sequential`, `balanced`, `aggressive`, or `rocket`. |
| `DiskSpace` | `250` | Minimum free disk space in MB. Downloads pause when space is below this threshold. `0` disables the check. |
| `NzbCleanupDisk` | `yes` | Delete the source `.nzb` file from `NzbDir` after adding to the queue. |
| `KeepHistory` | `30` | Days to keep completed downloads in history. `0` disables history. |
| `FeedHistory` | `7` | Days to keep RSS feed item history to avoid duplicate fetches. |
| `SkipWrite` | `no` | Skip writing downloaded data to disk (benchmark/testing mode). |
| `RawArticle` | `no` | Save raw NNTP articles without decoding (debugging). |

### Connection

| Option | Default | Description |
|--------|---------|-------------|
| `ArticleRetries` | `3` | Number of retry attempts for failed article downloads. |
| `ArticleInterval` | `10` | Seconds between article retry attempts. |
| `ArticleTimeout` | `60` | Seconds before an article download times out. |
| `ArticleReadChunkSize` | `4` | Read chunk size in KB for article downloads. |
| `UrlRetries` | `3` | Number of retry attempts for URL/NZB fetches. |
| `UrlInterval` | `10` | Seconds between URL retry attempts. |
| `UrlTimeout` | `60` | Seconds before a URL fetch times out. |
| `RemoteTimeout` | `90` | Timeout in seconds for remote API calls. |
| `DownloadRate` | `0` | Download speed limit in KB/s. `0` means unlimited. |
| `UrlConnections` | `4` | Maximum simultaneous URL fetch connections. |
| `MonthlyQuota` | `0` | Monthly download quota in GB. `0` means unlimited. |
| `QuotaStartDay` | `1` | Day of month when the monthly quota resets (1–31). |
| `DailyQuota` | `0` | Daily download quota in GB. `0` means unlimited. |

### PAR2 / Verification

| Option | Default | Description |
|--------|---------|-------------|
| `CrcCheck` | `yes` | Verify article CRC checksums during download. |
| `ParCheck` | `auto` | When to perform PAR2 verification: `auto`, `always`, `force`, or `manual`. |
| `ParRepair` | `yes` | Attempt PAR2 repair when verification fails. |
| `ParScan` | `limited` | PAR2 file scan mode: `limited` (quick), `full` (deep), or `auto`. |
| `ParQuick` | `yes` | Use quick verification (file-level) before full block-level check. |
| `ParBuffer` | `16` | PAR2 processing buffer size in MB. |
| `ParThreads` | `0` | Number of threads for PAR2 repair. `0` uses all available cores. |
| `ParIgnoreExt` | `.sfv, .nzb, .nfo` | Comma-separated file extensions to exclude from PAR2 verification. |
| `ParRename` | `yes` | Detect and rename obfuscated filenames using PAR2 data. |
| `HealthCheck` | `park` | Action when download health is too low: `park` (pause), `delete`, or `none`. |
| `ParTimeLimit` | `0` | Maximum minutes for PAR2 repair. `0` means unlimited. |
| `ParPauseQueue` | `no` | Pause the download queue during PAR2 check/repair. |

### Unpack

| Option | Default | Description |
|--------|---------|-------------|
| `Unpack` | `yes` | Enable automatic archive unpacking. |
| `DirectUnpack` | `no` | Begin unpacking while the download is still in progress. |
| `UnpackPauseQueue` | `no` | Pause the download queue during unpacking. |
| `UnpackCleanupDisk` | `yes` | Delete archive files after successful unpacking. |
| `UnrarCmd` | `unrar` | Path to the `unrar` executable. |
| `SevenZipCmd` | `7z` | Path to the `7z` executable. |
| `ExtCleanupDisk` | `.par2, .sfv` | Comma-separated file extensions to delete after successful post-processing. |
| `UnpackIgnoreExt` | `.cbr` | Comma-separated archive extensions to skip during unpacking. |
| `UnpackPassFile` | (empty) | Path to a file containing passwords for encrypted archives (one per line). |

### Logging

| Option | Default | Description |
|--------|---------|-------------|
| `WriteLog` | `append` | Log file write mode: `append`, `reset` (truncate on start), or `none`. |
| `RotateLog` | `3` | Number of rotated log files to keep. `0` disables rotation. |
| `ErrorTarget` | `both` | Where to send error messages: `screen`, `log`, `both`, or `none`. |
| `WarningTarget` | `both` | Where to send warning messages. |
| `InfoTarget` | `both` | Where to send info messages. |
| `DetailTarget` | `log` | Where to send detail messages. |
| `DebugTarget` | `none` | Where to send debug messages. |
| `LogBuffer` | `1000` | Maximum number of log lines kept in the in-memory buffer for the web UI. |
| `NzbLog` | `yes` | Create per-NZB log files in the download directory. |
| `CrashTrace` | `yes` | Print a stack trace on panic/crash. |
| `CrashDump` | `no` | Write a core dump file on crash. |
| `TimeCorrection` | `0` | Time offset in minutes applied to log timestamps. |

## Implementation

### Config Struct

```rust
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    // System / Paths
    pub main_dir: PathBuf,
    pub dest_dir: PathBuf,
    pub inter_dir: PathBuf,
    pub nzb_dir: PathBuf,
    pub queue_dir: PathBuf,
    pub temp_dir: PathBuf,
    pub web_dir: PathBuf,
    pub script_dir: PathBuf,
    pub log_file: PathBuf,
    pub config_template: PathBuf,
    pub required_dir: Vec<PathBuf>,
    pub cert_store: PathBuf,

    // Security
    pub control_ip: String,
    pub control_port: u16,
    pub control_username: String,
    pub control_password: String,
    pub secure_control: bool,
    pub secure_port: u16,
    pub authorized_ip: Vec<String>,

    // Numbered sections
    pub servers: Vec<ServerConfig>,
    pub categories: Vec<CategoryConfig>,
    pub feeds: Vec<FeedConfig>,
    pub tasks: Vec<TaskConfig>,

    // Download, connection, PAR2, unpack, logging...
    pub download_rate: u32,
    pub article_cache: u32,
    pub disk_space: u32,
    pub keep_history: u32,

    // Raw key-value store for interpolation and API access
    raw: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub id: u32,
    pub active: bool,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub encryption: bool,
    pub connections: u32,
    pub level: u32,
    pub optional: bool,
    pub group: u32,
    pub retention: u32,
    pub cipher: String,
    pub cert_verification: bool,
    pub ip_version: IpVersion,
}

#[derive(Debug, Clone)]
pub enum IpVersion {
    Auto,
    IPv4,
    IPv6,
}

#[derive(Debug, Clone)]
pub struct CategoryConfig {
    pub name: String,
    pub dest_dir: PathBuf,
    pub unpack: bool,
    pub extensions: Vec<String>,
    pub aliases: Vec<String>,
}
```

### Parsing and Variable Interpolation

```rust
use std::collections::HashMap;

fn parse_config(content: &str) -> Result<HashMap<String, String>, ConfigError> {
    let mut values: HashMap<String, String> = HashMap::new();

    for (line_num, line) in content.lines().enumerate() {
        let line = line.trim();

        // Skip comments and empty lines
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let (key, raw_value) = line.split_once('=')
            .ok_or(ConfigError::SyntaxError {
                line: line_num + 1,
                message: "expected Key=Value".into(),
            })?;

        let key = key.trim().to_string();
        let value = interpolate(raw_value.trim(), &values)?;
        values.insert(key, value);
    }

    Ok(values)
}

fn interpolate(
    value: &str,
    resolved: &HashMap<String, String>,
) -> Result<String, ConfigError> {
    let mut result = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '~' && result.is_empty() {
            // Tilde expansion at the start of a value
            if let Some(home) = dirs::home_dir() {
                result.push_str(&home.to_string_lossy());
            }
        } else if ch == '$' && chars.peek() == Some(&'{') {
            chars.next(); // consume '{'
            let var_name: String = chars.by_ref().take_while(|&c| c != '}').collect();
            if let Some(var_value) = resolved.get(&var_name) {
                result.push_str(var_value);
            } else {
                return Err(ConfigError::UnknownVariable(var_name));
            }
        } else {
            result.push(ch);
        }
    }

    Ok(result)
}
```

### Extracting Numbered Sections

```rust
fn extract_servers(raw: &HashMap<String, String>) -> Vec<ServerConfig> {
    let mut servers = Vec::new();

    for id in 1.. {
        let prefix = format!("Server{id}.");
        let host_key = format!("{prefix}Host");

        match raw.get(&host_key) {
            Some(host) if !host.is_empty() => {
                servers.push(ServerConfig {
                    id: id as u32,
                    active: parse_bool(raw.get(&format!("{prefix}Active")), true),
                    name: raw.get(&format!("{prefix}Name"))
                        .cloned().unwrap_or_default(),
                    host: host.clone(),
                    port: raw.get(&format!("{prefix}Port"))
                        .and_then(|v| v.parse().ok()).unwrap_or(119),
                    username: raw.get(&format!("{prefix}Username"))
                        .cloned().unwrap_or_default(),
                    password: raw.get(&format!("{prefix}Password"))
                        .cloned().unwrap_or_default(),
                    encryption: parse_bool(
                        raw.get(&format!("{prefix}Encryption")), false
                    ),
                    connections: raw.get(&format!("{prefix}Connections"))
                        .and_then(|v| v.parse().ok()).unwrap_or(1),
                    level: raw.get(&format!("{prefix}Level"))
                        .and_then(|v| v.parse().ok()).unwrap_or(0),
                    optional: parse_bool(
                        raw.get(&format!("{prefix}Optional")), false
                    ),
                    group: raw.get(&format!("{prefix}Group"))
                        .and_then(|v| v.parse().ok()).unwrap_or(0),
                    retention: raw.get(&format!("{prefix}Retention"))
                        .and_then(|v| v.parse().ok()).unwrap_or(0),
                    cipher: raw.get(&format!("{prefix}Cipher"))
                        .cloned().unwrap_or_default(),
                    cert_verification: parse_bool(
                        raw.get(&format!("{prefix}CertVerification")), false
                    ),
                    ip_version: parse_ip_version(
                        raw.get(&format!("{prefix}IpVersion"))
                    ),
                });
            }
            _ => break,
        }
    }

    servers
}

fn parse_bool(value: Option<&String>, default: bool) -> bool {
    match value.map(|s| s.to_lowercase()).as_deref() {
        Some("yes" | "true" | "1") => true,
        Some("no" | "false" | "0") => false,
        _ => default,
    }
}
```

### Runtime Config Changes via API

The JSON-RPC/XML-RPC API supports modifying configuration at runtime:

```rust
impl Config {
    /// Set an option value at runtime. Some options take effect immediately;
    /// others require a restart of the affected subsystem.
    pub fn set_option(
        &mut self,
        key: &str,
        value: &str,
    ) -> Result<(), ConfigError> {
        self.validate_option(key, value)?;

        // Re-interpolate the value
        let resolved = interpolate(value, &self.raw)?;
        self.raw.insert(key.to_string(), resolved.clone());

        // Apply immediately for known hot-reload options
        match key {
            "DownloadRate" => {
                self.download_rate = resolved.parse().map_err(|_| {
                    ConfigError::InvalidValue {
                        option: key.into(),
                        value: value.into(),
                    }
                })?;
            }
            "DiskSpace" => {
                self.disk_space = resolved.parse().map_err(|_| {
                    ConfigError::InvalidValue {
                        option: key.into(),
                        value: value.into(),
                    }
                })?;
            }
            _ => {}
        }

        Ok(())
    }

    /// Persist the current configuration to disk.
    pub fn save(&self, path: &std::path::Path) -> Result<(), ConfigError> {
        let mut output = String::new();
        for (key, value) in &self.raw {
            output.push_str(&format!("{key}={value}\n"));
        }
        std::fs::write(path, &output)
            .map_err(|e| ConfigError::IoError(e))?;
        Ok(())
    }

    fn validate_option(
        &self,
        key: &str,
        value: &str,
    ) -> Result<(), ConfigError> {
        // Validate option name exists in the schema
        // Validate value type and range
        // Return error for unknown or read-only options
        Ok(())
    }
}
```

### Error Types

```rust
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("syntax error on line {line}: {message}")]
    SyntaxError { line: usize, message: String },

    #[error("unknown variable: ${0}")]
    UnknownVariable(String),

    #[error("invalid value for {option}: {value}")]
    InvalidValue { option: String, value: String },

    #[error("unknown option: {0}")]
    UnknownOption(String),

    #[error("read-only option: {0}")]
    ReadOnlyOption(String),

    #[error("missing required option: {0}")]
    MissingRequired(String),

    #[error("I/O error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("config file not found: {0}")]
    FileNotFound(String),
}
```
