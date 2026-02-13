# Production readiness gap analysis: bergamot vs nzbget

## Current state

16 workspace crates with real implementations and comprehensive unit tests.
Phases 1–6 of the implementation plan are marked complete. All tests pass.
However, the implementation plan itself warns that **many components only work
to the extent needed to pass their tests** — the full runtime pipeline has not
been validated end-to-end against real NNTP servers or real clients.

---

## Critical blockers

### 1. End-to-end download not proven in real conditions

Individual crates test well in isolation, but no one has verified the full
runtime pipeline on a real NZB with a real NNTP server:

- **File assembly correctness** — Concurrent segment writes need either
  deterministic per-file serialization or correct random-access writes with
  preallocation. Segment offsets in `restore_queue()` are set to 0 for all
  segments, which may produce corrupt outputs.
- **Failure handling** — Missing articles (DMCA), timeouts, disconnects
  mid-body, and authentication failures need coherent retry/failover policies
  that match real-world NNTP behaviour.
- **Completion criteria** — `check_file_completion()` and
  `check_nzb_completion()` exist but haven't been exercised under realistic
  conditions (partial failures, mixed success/fail segments).
- **Crash safety** — Partially-written segments must not mark `Finished`
  prematurely; cancellation must revert state so restarts retry cleanly.

**Validation needed:** Enqueue a real multi-file NZB → download all segments →
assemble files → PAR2 verify/repair → unpack → move to destination → verify
final file hashes match expected output.

### 2. RPC schema fidelity for Sonarr/Radarr

Methods exist, but response shapes (field names, types, enum values) haven't
been validated against real client traffic. Sonarr/Radarr compatibility
requires **exact** NZBGet RPC behaviour, not just "a method with that name."

- **`append` payload format** — ✅ Full NZBGet signature now supported:
  `append(name, nzbcontent, category, priority, addToTop, addPaused, dupekey,
  dupescore, dupemode, params...)`. All parameters are parsed and forwarded
  to the queue via `AddNzbOptions`.
- **`status` response** — ✅ bergamot now populates day/month download
  counters from `SharedStatsTracker`, `QuotaReached` from configured
  `MonthlyQuota`/`DailyQuota`, `AverageDownloadRate` from downloaded
  bytes / uptime, and `ArticleCache` fields from live `BoundedCache`
  byte tracking.
- **`editqueue` actions** — ✅ `SetName`, `SetDupeKey`, `SetDupeScore`,
  `SetDupeMode`, `SetParameter`, `SetPriority`, `SetCategory`, `GroupMoveOffset`
  are now wired up and functional. `Split` and `Merge` remain as no-ops.
- **`postqueue`** — ✅ Now returns an empty array (NZBGet format) instead
  of a non-standard object.
- **`listgroups` / `history` / `listfiles`** — Field names,
  enum values, and defaults must match NZBGet exactly.

**Validation needed:** Run Sonarr/Radarr against bergamot, log every RPC request
and response, and diff against NZBGet for the same scenarios.

### 3. No bundled web UI

The server serves static files from an external `WebDir` directory, but there
are no HTML/JS/CSS assets in the repository. The only embedded UI is a minimal
hardcoded login page. Production requires either:

- Bundling NZBGet's webui (licensing permitting) into the release artifact
- Building a custom web UI
- Formally adopting and maintaining a fork of the NZBGet webui

The `handle_combined_file` handler exists for NZBGet webui compatibility
(concatenating JS/CSS files), which suggests the intent is to reuse NZBGet's
UI — but this needs to be formalized and tested.

---

## Missing features

### Duplicate detection & scoring

✅ Basic duplicate rejection is now enforced: `ingest_nzb()` returns a
`Duplicate` error when `check_duplicate()` finds a match, preventing the
download from being queued. `SetDupeKey`, `SetDupeScore`, and `SetDupeMode`
editqueue actions are now functional.

Remaining work:

- Move duplicates to history with `DupHidden` status instead of rejecting
- Score-based comparison (allow higher-score duplicates to replace lower)
- Per-category and per-append duplicate rules
- Accept `dupekey`/`dupescore`/`dupemode` from the `append` RPC call

### Deobfuscation & smart rename

bergamot relies mainly on NZB metadata/subject parsing for filenames. NZBGet
provides:

- Intelligent detection of obfuscated releases (regex-based hash pattern
  detection)
- PAR2-metadata-based file renaming (using PAR2 file lists to recover
  original names)
- Par renaming and RAR renaming for renamed segments
- Pre-unpack rename pipeline

### Direct rename / direct unpack

NZBGet can rename and unpack files as they are downloaded, without waiting for
the entire NZB to complete. This is a significant usability feature for large
downloads. Not implemented in bergamot.

### Download quotas

✅ `MonthlyQuota` and `DailyQuota` config keys are now parsed (in GB).
`QuotaReached` in the status response reflects whether quotas are exceeded.
Day/month download counters are populated from `SharedStatsTracker`.

Remaining work:

- Automatic pause of downloads when quota is reached
- Resume when quota resets (daily/monthly rollover)

### Extension manager lifecycle

bergamot can *run* extension scripts but lacks:

- Install/remove/update extensions from the UI or API
- `loadextensions` / `downloadextension` / `updateextension` /
  `deleteextension` RPC methods
- JSON manifest parsing for extension metadata, options, and requirements
- Dependency checking

### ✅ Multiple authentication roles

bergamot implements three access levels with per-method access control
(`crates/bergamot-server/src/auth.rs`):

| Role | Capabilities |
|------|-------------|
| **Control** | Full access to all API methods |
| **Restricted** | Can view status and queue but cannot change config |
| **Add** | Can only add downloads (for automated tools) |

A `required_access(method)` function maps each RPC method to its minimum
`AccessLevel` (Control, Restricted, Add, or Denied).

### PAR2 strategy (par-first downloading)

NZBGet intelligently prioritizes PAR2 file downloads:

- Downloads PAR2 files early to enable quick verification
- Decides whether to fetch additional repair blocks based on health
- Supports multiple PAR strategies (quick/full/force)
- Can pause the queue during PAR verification

bergamot has a native Rust PAR2 implementation (`bergamot-par2`) with parser,
verifier, repairer, and SIMD-accelerated GF multiplication — but no smart
download ordering.

### Windows service support

NZBGet supports running as a Windows service with install/remove commands.
bergamot only supports POSIX daemon mode.

### Update checking

NZBGet can check for new versions and notify users. Not implemented in bergamot.

### Testing tools (built-in diagnostics)

NZBGet provides built-in testing RPC methods:

- ✅ `testserver` — validate NNTP server connectivity (implemented)
- `testserverspeed` — measure NNTP server throughput
- `testdiskspeed` — measure disk I/O performance
- `testnetworkspeed` — measure network throughput
- `testextension` — validate extension script execution (stub registered)

---

## Testing & reliability gaps

### ✅ Integration tests

Comprehensive end-to-end tests exist in `crates/bergamot/tests/e2e_flow.rs`
using a stub NNTP server, covering: end-to-end download, multi-server failover,
crash recovery, RPC schema conformance, history schema, editqueue, pause/resume,
rate limiting, config roundtrip, auth rejection, multi-file NZB, concurrent
downloads, post-processing, extension scripts, graceful shutdown, and error
propagation.

### No RPC conformance tests

- Golden tests: given an RPC request, assert the response matches NZBGet's
  schema exactly (including optional fields, types, enum values)
- Client emulator tests: run Sonarr/Radarr's NZBGet integration flows against
  bergamot in CI (even smoke-level)

### No crash/restart recovery tests

Kill the process during:

- Segment write to disk
- Disk state flush
- Post-processing move/unpack

Assert that restart yields consistent queue state and no corrupt final outputs.

### No soak / concurrency testing

- Large queue + many connections + long runtime
- Memory growth over time
- Channel backpressure behaviour under load
- Lock contention / starvation detection
- Log buffer behaviour under sustained load

---

## Recommended path to production

### Step 1: Prove end-to-end correctness

Run a real NZB through the full pipeline against a real or simulated NNTP
server. This will immediately surface assembly, offset, naming, and completion
bugs that unit tests don't catch.

### Step 2: Validate Sonarr/Radarr compatibility

Point Sonarr at the RPC endpoint, trigger a download, and log/diff every RPC
request and response against NZBGet. Fix schema mismatches, especially in
`append`, `status`, `listgroups`, `history`, and `editqueue`.

### Step 3: Decide web UI strategy

Either bundle NZBGet's webui (check licensing), build a custom UI, or
explicitly document that users must supply their own webui directory.

### Step 4: Close highest-value feature gaps

Priority order based on user impact:

1. Duplicate detection policy enforcement
2. Deobfuscation / smart rename (PAR2-based)
3. Download quotas
4. Extension manager lifecycle

### Step 5: Add integration & failure tests

Build a production readiness test matrix covering:

1. Happy path: NZB → download → assemble → par2 → unpack → move
2. Partial failure: missing articles → PAR2 repair → success
3. Crash recovery: kill mid-download → restart → resume → complete
4. Client compatibility: Sonarr append → status polling → history check
5. Concurrent load: multiple NZBs, many connections, sustained throughput

---

## Feature parity summary

| Feature | nzbget | bergamot | Status |
|---------|--------|------|--------|
| NZB parsing | ✅ | ✅ | Complete |
| NNTP download | ✅ | ✅ | Implemented, untested e2e |
| yEnc decoding | ✅ | ✅ | Complete |
| Multi-server failover | ✅ | ✅ | Implemented |
| Connection pooling | ✅ | ✅ | Implemented |
| TLS/STARTTLS | ✅ | ✅ | Implemented |
| Queue management | ✅ | ✅ | Implemented |
| Speed limiting | ✅ | ✅ | Implemented |
| Article cache | ✅ | ✅ | Implemented |
| PAR2 verify/repair | ✅ | ✅ | Implemented |
| Unpack (rar/7z/zip) | ✅ | ✅ | Implemented |
| Post-processing pipeline | ✅ | ✅ | Implemented |
| JSON-RPC API | ✅ | ✅ | Full append signature, editqueue, status fields |
| XML-RPC API | ✅ | ✅ | Implemented |
| Extension script runner | ✅ | ✅ | Implemented |
| RSS/Atom feeds | ✅ | ✅ | Implemented |
| Scheduler | ✅ | ✅ | Implemented |
| Disk state persistence | ✅ | ✅ | Implemented |
| HTTPS control server | ✅ | ✅ | Implemented |
| IP whitelisting | ✅ | ✅ | Implemented |
| Form-based auth | ✅ | ✅ | Implemented |
| Daemon mode / pidfile | ✅ | ✅ | Implemented |
| NZB directory scanner | ✅ | ✅ | Implemented |
| Disk space monitoring | ✅ | ✅ | Implemented |
| History cleanup | ✅ | ✅ | Implemented |
| Config file compatibility | ✅ | ✅ | Implemented |
| Web UI | ✅ | ❌ | No bundled assets |
| Duplicate detection | ✅ | ⚠️ | Basic rejection enforced, scoring/history TBD |
| Deobfuscation | ✅ | ❌ | Not implemented |
| Direct rename/unpack | ✅ | ❌ | Not implemented |
| Download quotas | ✅ | ⚠️ | Config, counters, QuotaReached; auto-pause TBD |
| Extension manager | ✅ | ❌ | No install/remove lifecycle |
| Multiple auth roles | ✅ | ✅ | Three roles: Control, Restricted, Add |
| PAR-first strategy | ✅ | ❌ | No smart ordering |
| Built-in test tools | ✅ | ⚠️ | testserver implemented, others TBD |
| Windows service | ✅ | ❌ | Not supported |
| Update checking | ✅ | ❌ | Not implemented |
| Integration tests | ✅ | ✅ | E2E tests with stub NNTP server |
| Crash recovery tests | ✅ | ✅ | crash_recovery_resumes_download test |
