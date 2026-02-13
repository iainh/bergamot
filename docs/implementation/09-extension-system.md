# Extension system

Extensions (called "scripts" in nzbget) are external programs invoked at various points in the download lifecycle. bergamot discovers, manages and executes them as child processes, communicating via environment variables and exit codes.

## Extension types

```
┌─────────────────────────────────────────────────────────────┐
│                    Extension Types                          │
│                                                             │
│  ┌────────────────┐  Triggered after NZB completes          │
│  │ PostProcessing │  post-processing (unpack, rename, etc.) │
│  └────────────────┘                                         │
│  ┌────────────────┐  Triggered when NZB file is scanned     │
│  │     Scan       │  from incoming directory                │
│  └────────────────┘                                         │
│  ┌────────────────┐  Triggered on queue events (add,        │
│  │     Queue      │  download complete, etc.)               │
│  └────────────────┘                                         │
│  ┌────────────────┐  Triggered on schedule (cron-like)      │
│  │   Scheduler    │                                         │
│  └────────────────┘                                         │
│  ┌────────────────┐  Augments feed filter evaluation        │
│  │     Feed       │                                         │
│  └────────────────┘                                         │
│  ┌────────────────┐  Manually triggered via API             │
│  │    Command     │                                         │
│  └────────────────┘                                         │
└─────────────────────────────────────────────────────────────┘
```

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionKind {
    PostProcessing,
    Scan,
    Queue,
    Scheduler,
    Feed,
    Command,
}
```

## Extension lifecycle

### Discovery

On startup (and on `RELOAD` command), bergamot scans the directories listed in `ScriptDir` for executable files. Each file's header comments are parsed for metadata.

```
ScriptDir/
├── my-notify.py          # single-file extension
├── cleanup/
│   └── main.py           # directory-based extension
└── v2-extension/
    └── manifest.json      # V2 format (nzbget v23+)
```

### Metadata parsing (V1 format)

V1 extensions embed metadata in header comments between marker lines:

```python
##############################################################################
### NZBGET POST-PROCESSING SCRIPT                                        ###

# My Notification Script.
#
# Sends a notification when download completes.

##############################################################################
### OPTIONS                                                              ###

# Server hostname.
#Host=localhost

# Server port (1-65535).
#Port=8080

# Enable TLS (yes, no).
#UseTLS=no

### NZBGET POST-PROCESSING SCRIPT                                        ###
##############################################################################
```

```rust
pub struct ExtensionMetadata {
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub kind: Vec<ExtensionKind>,
    pub parameters: Vec<ExtensionParameter>,
    pub author: Option<String>,
    pub homepage: Option<String>,
    pub version: Option<String>,
    pub nzbget_min_version: Option<String>,
}

pub struct ExtensionParameter {
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub default: String,
    pub select: Option<Vec<String>>, // for select-type params
    pub param_type: ParamType,
}

pub enum ParamType {
    String,
    Number { min: Option<f64>, max: Option<f64> },
    Bool,
    Password,
    Select,
}
```

### V2 extension format (nzbget v23+)

V2 extensions use a `manifest.json` file:

```json
{
  "main": "main.py",
  "name": "MyExtension",
  "homepage": "https://example.com",
  "kind": ["POST-PROCESSING"],
  "version": "1.0",
  "author": "Author Name",
  "displayName": "My Extension",
  "description": "Does something useful.",
  "queueEvents": "NZB_ADDED",
  "taskTime": "08:00",
  "options": [
    {
      "name": "Host",
      "displayName": "Server hostname",
      "value": "localhost",
      "description": "The hostname of the server.",
      "type": "string"
    }
  ]
}
```

## Script execution

All extension types share the same execution model:

```
┌──────────┐       env vars        ┌──────────────┐
│   bergamot   │ ────────────────────► │  Extension   │
│          │                       │  (child      │
│          │ ◄──── stdout/stderr   │   process)   │
│          │                       │              │
│          │ ◄──── exit code       │              │
└──────────┘                       └──────────────┘
```

```rust
use std::process::Stdio;
use tokio::process::Command;

pub struct ExtensionRunner {
    pub extension: ExtensionMetadata,
    pub script_path: PathBuf,
}

impl ExtensionRunner {
    pub async fn execute(
        &self,
        env_vars: HashMap<String, String>,
    ) -> Result<ExtensionResult, ExtensionError> {
        let mut cmd = Command::new(&self.script_path);
        cmd.envs(&env_vars)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let child = cmd.spawn()?;
        let output = child.wait_with_output().await?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let exit_code = output.status.code().unwrap_or(-1);

        // Parse special [NZB] commands from stdout
        let messages = parse_script_output(&stdout);

        Ok(ExtensionResult {
            exit_code,
            stdout,
            stderr,
            messages,
        })
    }
}

pub struct ExtensionResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub messages: Vec<ScriptMessage>,
}

/// Commands embedded in script stdout: `[NZB] KEY=VALUE`
pub enum ScriptMessage {
    NzbName(String),
    NzbCategory(String),
    NzbDirectory(String),
    NzbFinalDir(String),
    NzbNzbName(String),
    NzbTop,             // move to top of queue
    NzbPause,           // pause NZB
    NzbMark(MarkType),  // mark as bad, good, success, etc.
    Log(LogLevel, String),
}
```

### stdout Protocol

Extensions can send commands to bergamot by writing special lines to stdout:

```
[NZB] NZBNAME=New Name
[NZB] CATEGORY=Movies
[NZB] DIRECTORY=/path/to/dest
[NZB] FINALDIR=/path/to/final
[NZB] TOP
[NZB] PAUSE
[NZB] MARK=BAD
[INFO] Informational message
[WARNING] Warning message
[ERROR] Error message
[DETAIL] Detailed message
[DEBUG] Debug message
```

## Post-processing scripts

Executed after an NZB completes post-processing (PAR2 + unpack).

### Environment variables (`NZBPP_*`)

| Variable | Description |
|----------|-------------|
| `NZBPP_NZBNAME` | NZB name (without extension) |
| `NZBPP_NZBFILENAME` | Original NZB filename |
| `NZBPP_DIRECTORY` | Destination directory with downloaded files |
| `NZBPP_FINALDIR` | Final directory (if set by previous script) |
| `NZBPP_CATEGORY` | Category assigned to NZB |
| `NZBPP_TOTALSTATUS` | Overall status: `SUCCESS`, `WARNING`, `FAILURE`, `DELETED` |
| `NZBPP_STATUS` | Detailed status string |
| `NZBPP_PARSTATUS` | PAR status: `0`=skipped, `1`=failed, `2`=success, `3`=repair needed, `4`=manual |
| `NZBPP_UNPACKSTATUS` | Unpack status: `0`=skipped, `1`=failed, `2`=success, `3`=password needed |
| `NZBPP_URL` | URL the NZB was fetched from |
| `NZBPP_DUPEKEY` | Duplicate key |
| `NZBPP_DUPESCORE` | Duplicate score |

### Exit codes

| Code | Constant | Meaning |
|------|----------|---------|
| `93` | `POSTPROCESS_SUCCESS` | Post-processing succeeded |
| `94` | `POSTPROCESS_FAILURE` | Post-processing failed |
| `95` | `POSTPROCESS_NONE` | No action taken (neutral) |

```rust
pub enum PostProcessResult {
    Success,   // 93
    Failure,   // 94
    None,      // 95
}

impl PostProcessResult {
    pub fn from_exit_code(code: i32) -> Self {
        match code {
            93 => Self::Success,
            94 => Self::Failure,
            _ => Self::None,
        }
    }
}
```

## Scan scripts

Triggered when a new NZB file is detected in the incoming directory.

### Environment variables (`NZBNA_*`)

| Variable | Description |
|----------|-------------|
| `NZBNA_NZBNAME` | NZB name |
| `NZBNA_FILENAME` | Full path to the NZB file |
| `NZBNA_CATEGORY` | Category assigned |
| `NZBNA_PRIORITY` | Priority value |
| `NZBNA_TOP` | `0` or `1` — add to top of queue |
| `NZBNA_PAUSED` | `0` or `1` — add in paused state |
| `NZBNA_URL` | Source URL (if fetched) |
| `NZBNA_DUPEKEY` | Duplicate key |
| `NZBNA_DUPESCORE` | Duplicate score |
| `NZBNA_DUPEMODE` | Duplicate mode |

### Exit codes

| Code | Meaning |
|------|---------|
| `93` | Success — accept the NZB |
| `94` | Failure — reject the NZB |
| `95` | None — no decision (pass to next script) |

## Queue scripts

Triggered on queue events. The `QueueEvents` metadata field specifies which events the script handles.

### Queue events

| Event | `NZBNA_EVENT` value | Trigger |
|-------|---------------------|---------|
| NZB added | `NZB_ADDED` | NZB was added to queue |
| NZB downloaded | `NZB_DOWNLOADED` | All files downloaded (pre-post-processing) |
| File downloaded | `FILE_DOWNLOADED` | A single file within NZB completed |
| NZB deleted | `NZB_DELETED` | NZB removed from queue |
| NZB marked | `NZB_MARKED` | NZB marked (good/bad/etc.) |
| URL completed | `URL_COMPLETED` | URL fetch completed |

## Scheduler scripts

Triggered at a configured time. Uses `TaskTime` metadata:

```python
### NZBGET SCHEDULER SCRIPT
# Task Time: *;*;*;*;08:00
```

The time format follows nzbget's scheduler syntax: `WeekDay;Hour:Minute` with wildcards.

## Feed scripts

Executed during feed processing to augment filter evaluation. The script receives feed item details and can override accept/reject decisions.

### Environment variables (`NZBFP_*`)

| Variable | Description |
|----------|-------------|
| `NZBFP_TITLE` | Feed item title |
| `NZBFP_URL` | NZB URL |
| `NZBFP_CATEGORY` | Item category |
| `NZBFP_SIZE` | Item size in bytes |
| `NZBFP_IMDBID` | IMDB ID (if available) |

## Extension manager

The `ExtensionManager` handles discovery, ordering, and invocation:

```rust
pub struct ExtensionManager {
    extensions: Vec<ExtensionInfo>,
    script_dirs: Vec<PathBuf>,
    config_values: HashMap<String, String>,
}

pub struct ExtensionInfo {
    pub metadata: ExtensionMetadata,
    pub path: PathBuf,
    pub enabled: bool,
    pub order: u32,
}

impl ExtensionManager {
    /// Scan ScriptDir(s) and parse all extension metadata.
    pub fn discover(&mut self) -> Result<()> {
        for dir in &self.script_dirs {
            for entry in walkdir::WalkDir::new(dir).max_depth(2) {
                let entry = entry?;
                if self.is_extension(&entry) {
                    if let Ok(meta) = self.parse_metadata(&entry) {
                        self.extensions.push(ExtensionInfo {
                            metadata: meta,
                            path: entry.path().to_path_buf(),
                            enabled: true,
                            order: self.extensions.len() as u32,
                        });
                    }
                }
            }
        }
        self.apply_script_order();
        Ok(())
    }

    /// Apply ScriptOrder configuration to reorder extensions.
    fn apply_script_order(&mut self) {
        // ScriptOrder is a comma-separated list of script names
        // defining execution order. Unmentioned scripts run last.
        todo!()
    }

    /// Run all enabled extensions of a given kind.
    pub async fn run_extensions(
        &self,
        kind: ExtensionKind,
        env_vars: HashMap<String, String>,
    ) -> Vec<ExtensionResult> {
        let mut results = Vec::new();
        for ext in self.extensions_of_kind(kind) {
            if !ext.enabled {
                continue;
            }
            let mut vars = env_vars.clone();
            self.inject_config_vars(&ext, &mut vars);
            let runner = ExtensionRunner {
                extension: ext.metadata.clone(),
                script_path: ext.path.clone(),
            };
            match runner.execute(vars).await {
                Ok(result) => results.push(result),
                Err(e) => {
                    tracing::error!(ext = %ext.metadata.name, err = %e, "extension failed");
                }
            }
        }
        results
    }

    /// Build env vars map with extension-specific config values.
    fn inject_config_vars(
        &self,
        ext: &ExtensionInfo,
        vars: &mut HashMap<String, String>,
    ) {
        // For each parameter defined in the extension, set
        // NZBPO_ParamName = configured value or default
        for param in &ext.metadata.parameters {
            let key = format!("NZBPO_{}", param.name);
            let val = self
                .config_values
                .get(&format!("{}/{}", ext.metadata.name, param.name))
                .cloned()
                .unwrap_or_else(|| param.default.clone());
            vars.insert(key, val);
        }
    }
}
```

### Common environment variables

All extension types receive these base variables:

| Variable | Description |
|----------|-------------|
| `NZBOP_TEMPDIR` | Temp directory path |
| `NZBOP_DESTDIR` | Destination directory path |
| `NZBOP_CATEGORY` | Default category |
| `NZBOP_NZBDIR` | NZB directory path |
| `NZBOP_VERSION` | bergamot version string |
| `NZBPO_*` | Extension-specific option values |
