# Production Readiness Gap Analysis: bergamot vs nzbget

## Current State

14 workspace crates with real implementations and comprehensive unit tests.
Phases 1–6 of the implementation plan are marked complete. All tests pass.
However, the implementation plan itself warns that **many components only work
to the extent needed to pass their tests** — the full runtime pipeline has not
been validated end-to-end against real NNTP servers or real clients.

---

## Critical Blockers

### 1. End-to-End Download Not Proven in Real Conditions

Individual crates test well in isolation, but no one has verified the full
runtime pipeline on a real NZB with a real NNTP server:

- **File assembly correctness** — Concurrent segment writes need either
  deterministic per-file serialization or correct random-access writes with
  preallocation. Segment offsets in `restore_queue()` are set to 0 for all
  segments, which may produce corrupt outputs.
- **Failure handling** — Missing articles (DMCA), timeouts, disconnects
  mid-body, and authentication failures need coherent retry/failover policies
  that match real-world NNTP behavior.
- **Completion criteria** — `check_file_completion()` and
  `check_nzb_completion()` exist but haven't been exercised under realistic
  conditions (partial failures, mixed success/fail segments).
- **Crash safety** — Partially-written segments must not mark `Finished`
  prematurely; cancellation must revert state so restarts retry cleanly.

**Validation needed:** Enqueue a real multi-file NZB → download all segments →
assemble files → PAR2 verify/repair → unpack → move to destination → verify
final file hashes match expected output.

### 2. RPC Schema Fidelity for Sonarr/Radarr

Methods exist, but response shapes (field names, types, enum values) haven't
been validated against real client traffic. Sonarr/Radarr compatibility
requires **exact** NZBGet RPC behavior, not just "a method with that name."

- **`append` payload format** — Many clients send NZB data as base64 content,
  not a local file path or URL. The exact NZBGet signature
  (`append(name, nzbcontent, category, priority, addToTop, addPaused, dupekey,
  dupescore, dupemode, params...)`) must be supported.
- **`status` response** — NZBGet exposes many fields (day/month counters,
  quotas, cache sizes, postproc states). bergamot currently returns month/day
  size as zero and likely omits or approximates other fields.
- **`editqueue` actions** — Several actions are explicitly no-ops: `Split`,
  `Merge`, `SetName`, `SetDupeKey`, `SetDupeScore`, `SetDupeMode`,
  `SetParameter`. If RPC advertises these but they don't work, clients will
  silently break.
- **`listgroups` / `history` / `listfiles` / `postqueue`** — Field names,
  enum values, and defaults must match NZBGet exactly.

**Validation needed:** Run Sonarr/Radarr against bergamot, log every RPC request
and response, and diff against NZBGet for the same scenarios.

### 3. No Bundled Web UI

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

## Missing Features

### Duplicate Detection & Scoring

Fields exist on `NzbInfo` (`dup_key`, `dup_mode`, `dup_score`) and a
`check_duplicate()` helper is present, but production requires:

- Policy enforcement: hide/move duplicates to history with `DupHidden` status
- Scoring behavior consistent with NZBGet (SCORE/ALL/FORCE modes)
- Per-category and per-append duplicate rules
- Integration with the queue coordinator's add/restore paths

### Deobfuscation & Smart Rename

bergamot relies mainly on NZB metadata/subject parsing for filenames. NZBGet
provides:

- Intelligent detection of obfuscated releases (regex-based hash pattern
  detection)
- PAR2-metadata-based file renaming (using PAR2 file lists to recover
  original names)
- Par renaming and RAR renaming for renamed segments
- Pre-unpack rename pipeline

### Direct Rename / Direct Unpack

NZBGet can rename and unpack files as they are downloaded, without waiting for
the entire NZB to complete. This is a significant usability feature for large
downloads. Not implemented in bergamot.

### Download Quotas

NZBGet supports daily and monthly download quotas with:

- Configurable quota sizes and reset schedules
- Automatic pause when quota is reached
- Day/month download counters in `status` response

bergamot's `status()` returns day/month counters as zero. No quota enforcement
exists.

### Extension Manager Lifecycle

bergamot can *run* extension scripts but lacks:

- Install/remove/update extensions from the UI or API
- `loadextensions` / `downloadextension` / `updateextension` /
  `deleteextension` RPC methods
- JSON manifest parsing for extension metadata, options, and requirements
- Dependency checking

### Multiple Authentication Roles

NZBGet supports three access levels with different permissions:

| Role | Capabilities |
|------|-------------|
| **Control** | Full access to all API methods |
| **Restricted** | Can view status and queue but cannot change config |
| **Add** | Can only add downloads (for automated tools) |

bergamot appears to have single-role authentication only.

### PAR Strategy (Par-First Downloading)

NZBGet intelligently prioritizes PAR2 file downloads:

- Downloads PAR2 files early to enable quick verification
- Decides whether to fetch additional repair blocks based on health
- Supports multiple PAR strategies (quick/full/force)
- Can pause the queue during PAR verification

bergamot has PAR2 verify/repair in post-processing but no smart download ordering.

### Windows Service Support

NZBGet supports running as a Windows service with install/remove commands.
bergamot only supports POSIX daemon mode.

### Update Checking

NZBGet can check for new versions and notify users. Not implemented in bergamot.

### Testing Tools (Built-in Diagnostics)

NZBGet provides built-in testing RPC methods:

- `testserver` — validate NNTP server connectivity
- `testserverspeed` — measure NNTP server throughput
- `testdiskspeed` — measure disk I/O performance
- `testnetworkspeed` — measure network throughput
- `testextension` — validate extension script execution

None of these are implemented in bergamot.

---

## Testing & Reliability Gaps

### No Integration Tests

Unit and crate-level tests exist, but no integration tests exercise the full
pipeline:

- Spin up a local NNTP server (or deterministic stub) that simulates missing
  articles, slow responses, timeouts, disconnects mid-body, and auth failures
- Feed a real multi-file multi-part yEnc NZB and verify final assembled files
  match expected hashes
- Verify restart mid-download resumes correctly
- Verify history entries and statuses match NZBGet semantics

### No RPC Conformance Tests

- Golden tests: given an RPC request, assert the response matches NZBGet's
  schema exactly (including optional fields, types, enum values)
- Client emulator tests: run Sonarr/Radarr's NZBGet integration flows against
  bergamot in CI (even smoke-level)

### No Crash/Restart Recovery Tests

Kill the process during:

- Segment write to disk
- Disk state flush
- Post-processing move/unpack

Assert that restart yields consistent queue state and no corrupt final outputs.

### No Soak / Concurrency Testing

- Large queue + many connections + long runtime
- Memory growth over time
- Channel backpressure behavior under load
- Lock contention / starvation detection
- Log buffer behavior under sustained load

---

## Recommended Path to Production

### Step 1: Prove End-to-End Correctness

Run a real NZB through the full pipeline against a real or simulated NNTP
server. This will immediately surface assembly, offset, naming, and completion
bugs that unit tests don't catch.

### Step 2: Validate Sonarr/Radarr Compatibility

Point Sonarr at the RPC endpoint, trigger a download, and log/diff every RPC
request and response against NZBGet. Fix schema mismatches, especially in
`append`, `status`, `listgroups`, `history`, and `editqueue`.

### Step 3: Decide Web UI Strategy

Either bundle NZBGet's webui (check licensing), build a custom UI, or
explicitly document that users must supply their own webui directory.

### Step 4: Close Highest-Value Feature Gaps

Priority order based on user impact:

1. Duplicate detection policy enforcement
2. Deobfuscation / smart rename (PAR2-based)
3. Download quotas
4. Multiple auth roles
5. Extension manager lifecycle

### Step 5: Add Integration & Failure Tests

Build a production readiness test matrix covering:

1. Happy path: NZB → download → assemble → par2 → unpack → move
2. Partial failure: missing articles → PAR2 repair → success
3. Crash recovery: kill mid-download → restart → resume → complete
4. Client compatibility: Sonarr append → status polling → history check
5. Concurrent load: multiple NZBs, many connections, sustained throughput

---

## Feature Parity Summary

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
| JSON-RPC API | ✅ | ✅ | Partial (schema gaps) |
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
| Duplicate detection | ✅ | ⚠️ | Fields exist, no policy |
| Deobfuscation | ✅ | ❌ | Not implemented |
| Direct rename/unpack | ✅ | ❌ | Not implemented |
| Download quotas | ✅ | ❌ | Not implemented |
| Extension manager | ✅ | ❌ | No install/remove lifecycle |
| Multiple auth roles | ✅ | ❌ | Single-role only |
| PAR-first strategy | ✅ | ❌ | No smart ordering |
| Built-in test tools | ✅ | ❌ | Not implemented |
| Windows service | ✅ | ❌ | Not supported |
| Update checking | ✅ | ❌ | Not implemented |
| Integration tests | ✅ | ❌ | None |
| Crash recovery tests | ✅ | ❌ | None |
