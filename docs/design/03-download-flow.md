# End-to-end download flow

This document traces the complete lifecycle of a download from NZB acquisition through to final placement in history. Each stage describes the data transformations, error paths, and the Rust components involved.

---

## Flow overview

```
 ┌─────────────────────────────────────────────────────────────────┐
 │                                                                 │
 │  ①  NZB Acquisition                                             │
 │  (upload, URL fetch, dir scan, RSS feed, API append)            │
 │                         │                                       │
 │  ②  NZB Parsing         ▼                                       │
 │  (XML → NzbInfo, filename extraction, PAR2 detection, reorder)  │
 │                         │                                       │
 │  ③  Queue Addition      ▼                                       │
 │  (dupe check, category, priority, pause PAR2 volumes)           │
 │                         │                                       │
 │  ④  Article Scheduling  ▼                                       │
 │  (priority selection, connection from pool)                     │
 │                         │                                       │
 │  ⑤  NNTP Download       ▼                                       │
 │  (connect, auth, GROUP, BODY, streaming read)                   │
 │                         │                                       │
 │  ⑥  yEnc Decoding       ▼                                       │
 │  (header parse, byte decode, CRC32 verify)                      │
 │                         │                                       │
 │  ⑦  Article Writing     ▼                                       │
 │  (direct write at offset or cache for batched flush)            │
 │                         │                                       │
 │  ⑧  File Completion     ▼                                       │
 │  (all articles done, flush cache, verify CRC, rename)           │
 │                         │                                       │
 │  ⑨  NZB Completion      ▼                                       │
 │  (all files done, trigger post-processing)                      │
 │                         │                                       │
 │  ⑩  Post-Processing     ▼                                       │
 │  (PAR2 rename/verify/repair, unpack, cleanup, move, extensions) │
 │                         │                                       │
 │  ⑪  History             ▼                                       │
 │  (record final status, retention management)                    │
 │                                                                 │
 └─────────────────────────────────────────────────────────────────┘
```

---

## Stage 1: NZB acquisition

NZB files enter bergamot through five paths, all converging on the same parsing pipeline:

| Source | Trigger | Entry Point | Notes |
|--------|---------|-------------|-------|
| **File upload** | User uploads via web UI or API | `POST /api/append` (base64-encoded NZB body) | Most common for manual use |
| **URL fetch** | User or automation provides a URL | `POST /api/appendurl` | bergamot fetches the NZB via HTTP(S) using `reqwest` |
| **Directory scan** | Daemon polls `NzbDir` at interval | `ScanDir` polling loop | Detects `.nzb` and `.nzb.gz` files, moves to queue dir after parsing |
| **RSS feed** | Feed coordinator polls RSS/Atom feeds | `FeedCoordinator` scheduler | Matching items have their NZB URLs fetched automatically |
| **API append** | Raw XML submitted via JSON-RPC/XML-RPC | `append` RPC method | Used by Sonarr, Radarr, NZBHydra |

### Gzip handling

`.nzb.gz` files are detected by checking for gzip magic bytes (`1f 8b`) and transparently decompressed via `flate2::read::GzDecoder`.

### Error paths

| Error | Recovery |
|-------|----------|
| URL fetch fails (network, HTTP error) | Retry with backoff; move to history as `Failure` after max retries |
| File not found in scan dir | Log warning, skip file |
| Invalid file (not XML, not gzip) | Log error, move to history as `Failure` |

---

## Stage 2: NZB parsing

The raw XML bytes are parsed into an `NzbInfo` structure by `bergamot-nzb` using `quick-xml`'s streaming pull parser.

### Processing steps

1. **XML parsing** — streaming state machine processes `<nzb>`, `<head>`, `<meta>`, `<file>`, `<groups>`, `<segments>` elements without building a DOM.
2. **Metadata extraction** — `<meta>` elements populate `NzbMeta` (title, password, category, tags).
3. **Filename extraction** — regex extracts the quoted filename from each `<file>` subject line (e.g., `"distro.part01.rar"` from `My.Distro [01/15] - "distro.part01.rar" yEnc (1/50)`).
4. **Filename deduplication** — duplicate filenames get `_2`, `_3` suffixes.
5. **PAR2 classification** — each file is classified as `NotPar`, `MainPar`, or `RepairVolume { block_offset, block_count }` based on filename patterns.
6. **Segment sorting** — segments within each file sorted by `number` ascending.
7. **Size calculation** — sum of `segment.bytes` per file and across all files.
8. **Health calculation** — `available_segments / expected_segments` as a permille value (0–1000).
9. **Hash computation** — `content_hash` (sorted message-IDs) and `name_hash` (sorted filenames) for duplicate detection.
10. **File reordering** (if `ReorderFiles` enabled) — content files first, then main PAR2, then repair volumes sorted by block count ascending.

### Error paths

| Error | Recovery |
|-------|----------|
| Malformed XML | Return `NzbError::XmlError`, NZB rejected |
| No `<file>` elements | Return `NzbError::NoFiles`, NZB rejected |
| Missing segment attributes | Return `NzbError::InvalidSegment`, NZB rejected |
| No filename extractable from subject | File proceeds with `None` filename; handled during download |

---

## Stage 3: Queue addition

The parsed `NzbInfo` is added to the `DownloadQueue` via a `QueueCommand::AddNzb` message to the coordinator.

### Steps

1. **Duplicate check** — compare `content_hash` and `name_hash` against existing queue entries and history. Based on `DupeMode` (Score, All, Force), the NZB may be rejected, paused, or accepted.
2. **Category assignment** — from NZB metadata, API parameter, filename pattern match, or feed rule.
3. **Priority assignment** — from API parameter, category default, or `Normal`.
4. **Directory setup** — create temp directory under `InterDir` (e.g., `InterDir/My.Distro.x64/`).
5. **PAR2 volume pausing** — repair volumes (`*.vol*+*.par2`) are paused by default. Only the main PAR2 index file downloads automatically. Repair volumes are unpaused on-demand if PAR2 verification detects damage.
6. **ID assignment** — unique monotonic IDs for the NZB and each file.
7. **Disk state save** — queue snapshot written atomically (write to `.tmp`, then rename).

### Error paths

| Error | Recovery |
|-------|----------|
| Duplicate detected (DupeMode::Score, lower score) | NZB rejected, moved to history as `Dupe` |
| Disk full (can't create temp dir) | NZB rejected with error, reported via API reply |

---

## Stage 4: Article scheduling

The coordinator's main loop fills download slots by selecting the next eligible article.

### Selection algorithm

```
for each NZB (sorted by priority descending, Force first):
    if NZB is paused: skip
    for each file in NZB (sorted by file type priority):
        if file is paused: skip
        if active downloads for this file >= max_articles_per_file: skip
        for each segment in file:
            if segment.status == Undefined:
                return this segment
```

### File type priority order

| Priority | File Type | Rationale |
|----------|-----------|-----------|
| 0 (first) | Main PAR2 (`.par2`) | Small file, needed to detect repair requirements |
| 1 | Data files (content) | The actual desired content |
| 2 (last) | PAR2 repair volumes | Downloaded on-demand only when needed |

### Connection assignment

Once an article is selected, the coordinator acquires a connection from the server pool:

1. Try servers at level 0 (primary) first.
2. Within a level, try servers in load-balancing groups (least-connections first).
3. Skip servers that are blocked (recent auth failure), at capacity, or whose retention is too short for the article's age.
4. If no level-0 server can serve the article, try level 1 (fill/backup), then level 2, etc.
5. If no connection is available across all servers, the article waits.

### Error paths

| Error | Recovery |
|-------|----------|
| No connections available | Article waits; slot-fill retried every 100ms |
| All servers at capacity | Download proceeds at reduced parallelism |
| Speed limit reached | Scheduling pauses until quota replenishes |

---

## Stage 5: NNTP download

Each article download runs as an independent tokio task with a checked-out connection.

### Protocol sequence

```
C: (TCP connect + optional TLS handshake)
S: 200 news.example.com NNRP Service Ready
C: AUTHINFO USER bob
S: 381 Password required
C: AUTHINFO PASS secret
S: 281 Authentication accepted
C: GROUP alt.binaries.linux         (if JoinGroup enabled and group changed)
S: 211 50000 1000 51000 alt.binaries.linux
C: BODY <part1of50.abc123@news.example.com>
S: 222 0 <part1of50.abc123@news.example.com>
S: =ybegin part=1 line=128 size=739811 name=file.rar
S: =ypart begin=1 end=128000
S: [yEnc-encoded data lines...]
S: =yend size=128000 part=1 pcrc32=abcd1234
S: .
```

The body is streamed line-by-line to the yEnc decoder — the full article body is never buffered in memory.

### Group caching

`NntpConnection` caches the current group. If the next article is in the same group, the `GROUP` command is skipped, saving a round-trip.

### Error paths

| Error | Response Code | Recovery |
|-------|---------------|----------|
| Article not found | 430 | Mark server as failed for this article; retry on next server level |
| Auth required | 480 | Re-authenticate; if fails, block server temporarily |
| Auth failed | 502 | Block server, try other servers |
| Connection timeout | — | Close connection, create new one, retry article |
| Connection dropped | — | Return connection to pool as broken; acquire new connection, retry |
| Server quota exceeded | — | Block server for configured backoff period |

### Server failover

When an article fails on one server, it is marked as failed for that specific server and returned to `Undefined` status. The scheduler will assign it to the next eligible server (same or higher level). The article only permanently fails when all servers at all levels have been tried.

---

## Stage 6: yEnc decoding

As body lines stream in from the NNTP connection, the `YencDecoder` processes them through a state machine:

### State machine

```
WaitingForHeader ──[=ybegin]──► WaitingForPart (if multi-part)
                                     │
WaitingForHeader ──[=ybegin]──► DecodingBody (if single-part)
                                     │
WaitingForPart ──[=ypart]──► DecodingBody
                                     │
DecodingBody ──[data lines]──► DecodingBody (decode bytes, update CRC)
                                     │
DecodingBody ──[=yend]──► Finished (verify CRC, emit DecodedSegment)
```

### Decoding

Each data byte is decoded by subtracting 42 (mod 256). Escape sequences (`=` followed by a byte) subtract 106 (42 + 64). Line terminators (`\r`, `\n`) are skipped.

### CRC32 verification

A `crc32fast::Hasher` computes a running CRC32 of decoded bytes. On `=yend`, the computed CRC is compared against the `pcrc32` header value. Hardware-accelerated CRC32 instructions (SSE4.2 on x86_64, CRC32 on ARMv8) are used automatically.

### Error paths

| Error | Recovery |
|-------|----------|
| Missing `=ybegin` header | `YencError::MissingField`, article treated as corrupt |
| CRC mismatch (`pcrc32`) | `YencError::CrcMismatch`, article re-downloaded from another server |
| Data exceeds declared size | `YencError::SizeOverflow`, article treated as corrupt |
| Unexpected end of data | `YencError::UnexpectedEnd`, article re-downloaded |

---

## Stage 7: Article writing

After successful decoding, the `DecodedSegment` (containing the decoded bytes, byte range, and CRC) must be written to disk.

### Direct write mode

The segment is written directly to the output file at its correct byte offset. The output file is pre-allocated to the full file size using `fallocate`/`ftruncate`:

```
Output file (pre-allocated to file_size bytes):
┌──────────┬──────────┬──────────┬──────────┐
│ Segment 1│ Segment 2│ Segment 3│ Segment 4│
│ (written)│ (gap)    │ (written)│ (gap)    │
└──────────┴──────────┴──────────┴──────────┘
             ▲                      ▲
             │                      │
        not yet received       not yet received
```

Segments can arrive out of order — each is written at its `begin - 1` offset (yEnc uses 1-indexed offsets).

### Cache mode

An in-memory `ArticleCache` holds decoded segments before flushing to disk. This batches writes and reduces random I/O when segments arrive out of order. The cache is bounded by `ArticleCacheSize` (configurable in bytes). When the cache exceeds its limit, the largest file's segments are evicted and flushed.

### Error paths

| Error | Recovery |
|-------|----------|
| Disk full | Pause downloads, log error, retry when space available |
| I/O error on write | Mark article as failed, retry |
| Cache eviction failure | Log error, attempt direct write fallback |

---

## Stage 8: File completion

When all segments for a file have a terminal status (`Completed` or `Failed`), the coordinator finalizes the file.

### Steps

1. **Flush cache** — any remaining in-memory segments for this file are written to disk.
2. **File-level CRC verify** — if the last segment's `=yend` contained a `crc32` field, the CRC32 of the entire assembled file is computed and compared.
3. **Rename** — the temporary file (e.g., `filename.bergamot.tmp`) is renamed to its final name within the temp directory.
4. **Record completion** — a `CompletedFile` entry is created with status (`Success` or `Failure`), CRC, and size.
5. **Update NZB counters** — `remaining_file_count`, `success_size`, `failed_size` are updated.
6. **Health recalculation** — the NZB's health is recomputed. If health drops below `critical_health`, the configured `HealthCheck` action is taken (Park, Delete, or None).

### Error paths

| Error | Recovery |
|-------|----------|
| File-level CRC mismatch | File marked as `Failure`; PAR2 repair may recover it |
| Rename fails (permissions) | Log error, file marked as `Failure` |
| All articles for file failed | File marked as `Failure`; health updated |

---

## Stage 9: NZB completion

When all files in an NZB have been completed (successfully or not), the coordinator triggers post-processing.

### Steps

1. **Completion detection** — the coordinator checks `nzb.files.iter().all(|f| f.completed)`.
2. **Status update** — NZB status set to `PostProcessing`.
3. **Post-processing request** — a `PostProcessRequest` is sent to the `PostProcessor` via its `mpsc` channel, containing the NZB ID, name, working directory, category, parameters, and accumulated status.
4. **Download stats finalization** — download time, total bytes, article success/failure counts are recorded.

### PAR2 volume on-demand download

If file completion reveals damage (failed articles, CRC mismatches), and PAR2 repair volumes are paused in the queue, the coordinator may unpause enough repair volumes to cover the needed blocks before triggering post-processing. This is determined by comparing damaged blocks against available recovery blocks.

### Error paths

| Error | Recovery |
|-------|----------|
| Health below critical, no repair possible | NZB marked as `Failure`, moved to history (or parked if `HealthCheck::Park`) |
| PostProcessor channel full | Backpressure — coordinator waits; bounded channel prevents memory growth |

---

## Stage 10: Post-processing

The `PostProcessor` runs as its own tokio task, processing completed NZBs sequentially (or in parallel, depending on `PostStrategy`).

### Pipeline stages

```
PAR2 Rename ──► PAR2 Verify ──► PAR2 Repair (if needed)
                                       │
                                       ▼
                               RAR Rename ──► Unpack ──► Cleanup ──► Move ──► Extensions
```

#### 10a: PAR2 rename

Obfuscated files are renamed to their original names by matching MD5 hashes against PAR2 metadata. This must happen before verification so that PAR2 can find the files it expects.

#### 10b: PAR2 verify

**Quick verify**: compare stored article CRCs against PAR2 block checksums — no disk reads needed if CRCs were captured during download.

**Full verify** (if quick verify inconclusive): read each file from disk, compute MD5 per block, compare against PAR2 metadata.

#### 10c: PAR2 repair

If verification found damaged or missing blocks:

1. Count needed recovery blocks.
2. If paused PAR2 repair volumes exist in the queue, unpause them, wait for download.
3. Load recovery volumes and reconstruct damaged blocks using Reed-Solomon.
4. Re-verify repaired files.

If insufficient recovery blocks are available, PAR2 repair fails.

#### 10d: RAR rename

After PAR2 rename (which handles data files), obfuscated RAR archives are identified by reading RAR magic bytes and internal headers, then renamed.

#### 10e: Unpack

Extract archives (RAR, 7z, ZIP) using external tools (`unrar`, `7z`). Passwords are tried from NZB metadata, API parameters, filename `{{password}}` format, and password file.

#### 10f: Cleanup

Remove archive files, PAR2 files, and files matching configured cleanup extensions after successful unpack.

#### 10g: Move

Move files from `InterDir` (temp directory) to `DestDir` (final destination). Category-specific destination directories are supported. Cross-filesystem moves fall back to copy + delete.

#### 10h: Extensions

Run user-defined post-processing scripts (Python, Bash, etc.) with nzbget-compatible environment variables (`NZBOP_*`, `NZBNA_*`, `NZBPR_*`).

### Post-processing strategies

| Strategy | Behaviour |
|----------|-----------|
| `Sequential` | One job at a time; downloads pause during PAR2 repair |
| `Balanced` | One job at a time; downloads continue during repair |
| `Rocket` | Multiple jobs in parallel; downloads continue |
| `Aggressive` | Like Rocket; also starts unpacking during download (direct unpack) |

### Error paths

| Stage | Error | Recovery |
|-------|-------|----------|
| PAR2 Verify | Main PAR2 file missing | Skip verification, proceed (may fail at unpack) |
| PAR2 Repair | Insufficient recovery blocks | Mark `ParStatus::Failure`, continue pipeline |
| Unpack | Wrong password | Try next password from list; if all fail, `UnpackStatus::Password` |
| Unpack | Disk space insufficient | `UnpackStatus::Space`, abort pipeline |
| Unpack | Archive corrupt | `UnpackStatus::Failure`, continue to cleanup/move |
| Move | Target directory not writable | `MoveStatus::Failure`, files remain in temp dir |
| Extensions | Script returns non-zero | Record script status, continue pipeline |

---

## Stage 11: History

After post-processing completes (or after deletion/failure), the NZB is moved from the download queue to history.

### History entry creation

A `HistoryEntry` is created from the NZB state and post-processing results:

- **`TotalStatus`** — `Success`, `Warning` (repair was needed), `Failure`, or `Deleted`.
- **`ParStatus`** — `None`, `Success`, `RepairPossible`, `Failure`, `Manual`.
- **`UnpackStatus`** — `None`, `Success`, `Failure`, `PasswordRequired`, `Space`.
- **`MoveStatus`** — `None`, `Success`, `Failure`.
- **Script statuses** — per-extension exit codes.

### Timing and statistics

The history entry records:

- Total download time (seconds)
- Post-processing time (seconds)
- Total size, downloaded size
- File count, article count, failed article count
- Final health value

### Retention management

The `KeepHistory` config option controls retention:

| Value | Behavior |
|-------|----------|
| `0` | Keep forever |
| `N` | Auto-purge entries older than N days |

Purging runs after each new entry is added and periodically.

### History operations (API)

| Operation | Description |
|-----------|-------------|
| `HistoryDelete` | Remove entry permanently |
| `HistoryReturn` | Return NZB to download queue for re-download |
| `HistoryRedownload` | Re-add and re-download from scratch |
| `HistoryMarkBad` | Mark as bad (affects duplicate scoring) |
| `HistoryMarkGood` | Mark as good |
| `HistoryMarkSuccess` | Override status to success |

---

## Concurrency model

Multiple NZBs, files, and articles are processed simultaneously across all stages:

```
                    ┌─────────────────────────────────────┐
                    │         Queue Coordinator            │
                    │         (single actor task)          │
                    └──────┬────────────┬─────────────────┘
                           │            │
              ┌────────────┘            └──────────────┐
              ▼                                        ▼
    ┌──────────────────┐                    ┌──────────────────┐
    │  NZB-1 downloads │                    │  NZB-2 downloads │
    │                  │                    │                  │
    │  File A          │                    │  File D          │
    │  ├── Art 1 ──►Conn(Srv1)             │  ├── Art 1 ──►Conn(Srv1)
    │  ├── Art 2 ──►Conn(Srv1)             │  └── Art 2 ──►Conn(Srv2)
    │  └── Art 3 ──►Conn(Srv2)             │                  │
    │                  │                    │  File E          │
    │  File B          │                    │  └── Art 1 ──►Conn(Srv2)
    │  └── Art 1 ──►Conn(Srv2)             │                  │
    └──────────────────┘                    └──────────────────┘
              │                                        │
              ▼                                        ▼
    ┌──────────────────┐                    ┌──────────────────┐
    │  Post-Processing │                    │  (still          │
    │  (PAR2, unpack)  │                    │   downloading)   │
    └──────────────────┘                    └──────────────────┘
```

### Concurrency boundaries

| Resource | Concurrency Limit | Configured By |
|----------|-------------------|---------------|
| Total active article downloads | Sum of all server connection limits | `Server1.Connections`, etc. |
| Connections per server | `ServerN.Connections` | Per-server config |
| Articles per file | `ArticleConnections` (internal limit) | Prevents one file from monopolizing all connections |
| Post-processing jobs | 1 (Sequential/Balanced) or N (Rocket/Aggressive) | `PostStrategy` |
| Feed polling | Sequential per feed, periodic interval | `FeedInterval` |

### Data flow between stages

| From | To | Mechanism |
|------|-----|-----------|
| API/Feed/Scan → Coordinator | `QueueCommand::AddNzb` | `mpsc` channel |
| Coordinator → Download Task | Spawned `tokio::spawn` with article info + connection | Task spawn |
| Download Task → Coordinator | `DownloadCompletion` | `mpsc` channel |
| Coordinator → PostProcessor | `PostProcessRequest` | `mpsc` channel |
| PostProcessor → History | `HistoryEntry` | Direct call or channel |
| Shutdown signal → All subsystems | `()` | `broadcast` channel |
