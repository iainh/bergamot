# Performance Bottlenecks

## 1. CRITICAL — Full state serialized+fsync'd on every article completion

**File:** `crates/bergamot-diskstate/src/lib.rs:71-75`

**Current code:**
```rust
pub fn save_file_state(&self, file_id: u32, state: &FileArticleState) -> anyhow::Result<()> {
    let data = self.format.serialize(state)?;
    let path = self.state_dir.join(format!("file/{file_id:08}.state"));
    atomic_write(&path, &data)
}
```

**Why:** Write amplification and sync latency cap throughput and stall downloads. Every article completion triggers a full JSON serialize + fsync + dir sync.

**Callers:**
- `crates/bergamot-scheduler/src/lib.rs` — `DiskStateFlush::tick` runs every ~5 seconds and calls `save_file_state` for *every* file with completed articles (lines ~1189-1197).
- `crates/bergamot/src/app.rs` — shutdown flush (lines ~682-693) saves all file states.
- `load_file_state` (lib.rs:77-81) reads `.state` files back on restore.

**Fix — WAL/append-only journal:**

1. **Add a per-file WAL file** alongside existing snapshots:
   - Snapshot: `file/{file_id:08}.state` (existing, JSON)
   - WAL: `file/{file_id:08}.wal` (new, binary append-only)

2. **WAL binary record format** (little-endian, 8 bytes per entry):
   - `u32 article_index`
   - `u32 crc32`
   - Partial/torn tail (length not multiple of 8): ignore trailing bytes on read.

3. **New `DiskState` APIs:**
   - `fn wal_path(&self, file_id: u32) -> PathBuf` — returns `file/{file_id:08}.wal`
   - `pub fn append_file_wal(&self, file_id: u32, entries: &[(u32, u32)]) -> anyhow::Result<()>` — opens with `OpenOptions::new().create(true).append(true)`, writes records, does NOT fsync (pairs with Issue 2).
   - Update `load_file_state` — load snapshot, then replay WAL records by calling `state.mark_article_done(idx, crc)` for each.
   - `pub fn compact_file_state(&self, file_id: u32, state: &FileArticleState) -> anyhow::Result<()>` — write snapshot atomically, then truncate/delete WAL.

4. **Update `DiskStateFlush::tick`** in `crates/bergamot-scheduler/src/lib.rs`:
   - Track `last_flushed: HashMap<u32, usize>` (file_id → count of completed articles seen last time).
   - Only append *new* completions to WAL via `append_file_wal`.
   - Periodically compact (e.g., if WAL > 1 MiB or N entries).
   - `snap.completed_articles` is built in ascending index order, so slicing `[start..]` for deltas is stable.

5. **Update `delete_file_state`** — must delete both `.state` and `.wal` files.

6. **Update `validate_consistency`** (lib.rs:179-219) — treat either snapshot or WAL as "present" for a file_id.

**Invariants to preserve:**
- `total_articles` must be available for `FileArticleState::new`. Ensure at least one snapshot is written per file (e.g., at file creation/first flush) since WAL alone doesn't store `total_articles`. Alternative: add a WAL header record with magic + version + total_articles.
- `mark_article_done` is idempotent (sets bits, overwrites CRC entry), so WAL replay with duplicates is safe.
- Crash during compaction: write snapshot atomically first (tmp + rename), then truncate WAL. If crash before truncation, replaying WAL again is safe.

**Tests to add** in `crates/bergamot-diskstate/src/lib.rs`:
- WAL roundtrip: create snapshot baseline, `append_file_wal` entries, `load_file_state` returns state with those bits set and CRCs present.
- Partial WAL tail: write 8*N + 1 bytes, assert load doesn't error, only applies full records.
- Delete removes both `.state` and `.wal`.
- `validate_consistency` accepts WAL-only presence if applicable.

---

## 2. HIGH — `atomic_write` calls `file.sync_all()` and `dir.sync_all()` every time

**File:** `crates/bergamot-diskstate/src/lib.rs:406-421`

**Current code:**
```rust
pub fn atomic_write(path: &Path, data: &[u8]) -> anyhow::Result<()> {
    let tmp_path = path.with_extension("tmp");
    let mut file = fs::File::create(&tmp_path)?;
    file.write_all(data)?;
    file.sync_all()?;
    fs::rename(&tmp_path, path)?;
    if let Some(parent) = path.parent() {
        let dir = fs::File::open(parent)?;
        dir.sync_all()?;
    }
    Ok(())
}
```

**Why:** Blocking disk I/O in the hot path reduces throughput. Every state save pays two fsyncs.

**Callers:** `save_queue`, `save_file_state`, `save_history`, `save_stats`, `save_feeds`, `save_nzb_file`.

**Fix — Add options to control sync behavior:**

1. **Add an options struct and new function:**
   ```rust
   pub struct AtomicWriteOptions {
       pub sync_file: bool,
       pub sync_dir: bool,
   }

   pub fn atomic_write_with_options(path: &Path, data: &[u8], opt: AtomicWriteOptions) -> anyhow::Result<()> { ... }
   ```

2. **Keep `atomic_write` as a durable wrapper** (current behavior, calls with both `true`) for backward compatibility.

3. **Add `atomic_write_relaxed`** that uses `sync_file: false, sync_dir: false` (or `sync_data()` instead of `sync_all()` for a lighter option).

4. **Update hot-path writers:**
   - After Issue 1, WAL appends bypass `atomic_write` entirely (plain `write_all` + append mode).
   - Snapshot compaction (rare) uses durable `atomic_write`.
   - `save_queue` can remain durable.

**Invariants:** The `.tmp` cleanup in `recover()` (lib.rs:159-177) remains important when durability is relaxed. On Windows, directory fsync semantics differ; gate `sync_dir` on `cfg(unix)` if cross-platform matters.

**Tests:** Existing diskstate tests should pass. Add a test exercising `atomic_write_with_options(sync=false)` to verify correct contents (functional, not durability).

---

## 3. MEDIUM — Uses `serde_json::to_vec_pretty` for internal state writes

**File:** `crates/bergamot-diskstate/src/lib.rs:23-25`

**Current code:**
```rust
impl StateFormat for JsonFormat {
    fn serialize<T: Serialize>(&self, value: &T) -> anyhow::Result<Vec<u8>> {
        Ok(serde_json::to_vec_pretty(value)?)
    }
```

**Why:** Extra CPU and I/O for whitespace that no consumer reads.

**Fix:** Change `serde_json::to_vec_pretty` to `serde_json::to_vec`. One-line change. No other code changes required. JSON remains JSON so deserialization is backward compatible.

**Tests:** Existing diskstate roundtrip tests should pass without changes (they deserialize, not byte-compare).

---

## 4. HIGH — `BodyReader::read_line` allocates a new `Vec<u8>` per line

**File:** `crates/bergamot-nntp/src/protocol.rs:202-228`

**Current code:**
```rust
pub async fn read_line(&mut self) -> Result<Option<Vec<u8>>, NntpError> {
    let mut buf = Vec::new();               // <-- allocation per call
    let bytes = match &mut self.conn.stream {
        NntpStream::Plain(reader) => reader.read_until(b'\n', &mut buf).await?,
        NntpStream::Tls(reader) => reader.read_until(b'\n', &mut buf).await?,
    };
    // ...
}
```

**Why:** Severe allocation churn for multi-line articles. Primary consumer is `crates/bergamot-nntp/src/pool.rs:316-323` which loops `while let Some(line) = reader.read_line().await?`.

**Fix — Add a reusable buffer to `BodyReader`:**

1. Add field to struct:
   ```rust
   pub struct BodyReader<'a> {
       conn: &'a mut NntpConnection,
       read_buf: Vec<u8>,
   }
   ```

2. Initialize in constructor:
   ```rust
   fn new(conn: &'a mut NntpConnection) -> Self {
       Self { conn, read_buf: Vec::with_capacity(8192) }
   }
   ```

3. In `read_line`: call `self.read_buf.clear()` then pass `&mut self.read_buf` to `read_until`. Use `machine::trim_crlf(&self.read_buf)` as before.

**Invariants:** `read_until` appends, so `clear()` is required each call. `trim_crlf` returns a slice; `machine.handle_input(...)` must process synchronously and not store the slice (it does).

**Tests:** Existing tokio tests (`body_reader_unstuffs_and_terminates`, `body_reader_empty_body`, `body_reader_eof_mid_body`, `body_reader_dot_stuffed_lines`) should pass unchanged.

---

## 5. HIGH — DeflateStream allocates a new output buffer every decompress loop iteration

**File:** `crates/bergamot-nntp/src/protocol.rs:71` (read path), `protocol.rs:122` and `protocol.rs:153` (write/flush paths)

**Current code (read path):**
```rust
let mut out = vec![0u8; buf.remaining().max(DEFLATE_BUF_SIZE)];  // line 71, inside loop
```

**Current code (write path):**
```rust
let mut out = vec![0u8; buf.len() + 64];  // line 122
```

**Current code (flush path):**
```rust
let mut out = vec![0u8; 64];  // line 153
```

**Why:** Allocator overhead in the hot data path when NNTP compression is enabled.

**Fix — Add persistent temporary buffers to `DeflateStream`:**

1. Add fields:
   ```rust
   struct DeflateStream<T> {
       // ... existing fields ...
       tmp_read_out: Vec<u8>,
       tmp_write_out: Vec<u8>,
   }
   ```

2. Initialize in `new_with_buffered`:
   ```rust
   tmp_read_out: vec![0u8; DEFLATE_BUF_SIZE],
   tmp_write_out: vec![0u8; 64],
   ```

3. In `poll_read`: `me.tmp_read_out.resize(buf.remaining().max(DEFLATE_BUF_SIZE), 0);` then decompress into `&mut me.tmp_read_out[..]` and `buf.put_slice(&me.tmp_read_out[..produced])`.

4. In `poll_write`: `me.tmp_write_out.resize(buf.len() + 64, 0);` then compress into it.

5. In `poll_flush`: `me.tmp_write_out.resize(64, 0);` and reuse.

**Invariants:** Keep current loop semantics (may produce 0 bytes after consuming input; continue looping). Avoid unbounded growth with reasonable caps if needed.

**Tests:** Existing `compress_wraps_stream_deflate` test should pass. Optionally add a regression test sending multiple compressed chunks.

---

## 6. HIGH — Scalar byte-by-byte yEnc decode prevents SIMD optimization

**File:** `crates/bergamot-yenc/src/decode.rs:8-33`

**Current code:**
```rust
pub fn decode_yenc_line(line: &[u8], output: &mut Vec<u8>) -> usize {
    let mut i = 0;
    let start_len = output.len();
    while i < line.len() {
        let b = line[i];
        match b {
            b'\r' | b'\n' => { i += 1; continue; }
            b'=' => {
                i += 1;
                if i < line.len() {
                    output.push(line[i].wrapping_sub(106));
                }
            }
            _ => { output.push(b.wrapping_sub(42)); }
        }
        i += 1;
    }
    output.len() - start_len
}
```

**Why:** yEnc decoding is CPU-bound; scalar is far slower than SIMD. Called by `YencDecoder::decode_line` (decode.rs:87-90) for every body line.

**Fix — SIMD fast-path (SSE2 on x86_64, NEON on aarch64):**

1. **Rename existing function** to `decode_yenc_line_scalar` (keep as private fallback).

2. **Trim CR/LF once** at the start:
   ```rust
   let mut end = line.len();
   while end > 0 && (line[end-1] == b'\n' || line[end-1] == b'\r') { end -= 1; }
   let line = &line[..end];
   ```

3. **SIMD fast-path** (x86_64 SSE2 example): process 16-byte chunks:
   - Load `__m128i` from input.
   - Compare each byte to `b'='`: `cmp = _mm_cmpeq_epi8(v, eq_vec)`.
   - `mask = _mm_movemask_epi8(cmp)`.
   - If `mask == 0` (no escapes in chunk): subtract 42 (`_mm_sub_epi8(v, sub_vec)`) and store 16 bytes to output via unsafe extend.
   - If `mask != 0`: fall back to scalar for this chunk (or up to the next aligned boundary).

4. **Scalar fallback** for remaining bytes and escape handling uses existing logic.

**Invariants to preserve:**
- `=` at end-of-line with no following byte produces no output byte.
- CR/LF anywhere in input are skipped.
- Output must be byte-identical to scalar version for all inputs.

**Tests:**
- Keep `decode_yenc_line_scalar` and compare SIMD output against it for: lines without `=`, lines with `=` escapes, random byte lines.
- Existing `parse_single_part_segment` and `decode_yenc_line_unescapes_bytes` should pass.

---

## 7. MEDIUM — Two-pass decode + CRC32 calculation

**File:** `crates/bergamot-yenc/src/decode.rs:87-90`

**Current code:**
```rust
let before = self.decoded.len();
decode_yenc_line(line, &mut self.decoded);
self.part_crc
    .update(&self.decoded[before..self.decoded.len()]);
```

**Why:** Extra memory bandwidth and cache thrash from reading the decoded bytes a second time for CRC.

**Fix — Update CRC32 during decode:**

1. **Change decode function signature** to accept an optional CRC hasher:
   ```rust
   pub fn decode_yenc_line_into(
       line: &[u8],
       output: &mut Vec<u8>,
       crc: Option<&mut Hasher>,
   ) -> usize
   ```

2. **Update implementation:** after writing decoded bytes (whether SIMD block or scalar), call `crc.update(...)` on exactly the bytes just written, using a slice of the output buffer.

3. **Update `YencDecoder::decode_line`** (decode.rs:81-96):
   - Remove the `before` variable and the separate `.update()` call.
   - Call `decode_yenc_line_into(line, &mut self.decoded, Some(&mut self.part_crc))`.

**Invariants:** CRC must be identical to `crc32fast::hash(decoded_bytes)` for the segment. `self.part_crc` is reset in `finalize_segment` (line 174) — preserve this.

**Tests:** Existing `decode_reports_crc_mismatch` remains valid. Add a test asserting `segment.crc32 == crc32fast::hash(&segment.data)`.

---

## 8. HIGH — Scalar GF multiplication on x86_64 (missing SIMD)

**File:** `crates/bergamot-par2/src/galois.rs:162-183` (scalar `muladd_scalar`), `galois.rs:150-160` (dispatch), `galois.rs:247-264` (`mul_slice_inplace` dispatch)

**Current dispatch code (muladd):**
```rust
pub fn muladd_with_table(dst: &mut [u8], src: &[u8], table: &MulTable) {
    let len = dst.len().min(src.len());
    #[cfg(target_arch = "aarch64")]
    { muladd_neon(dst, src, len, table); }
    #[cfg(not(target_arch = "aarch64"))]
    muladd_scalar(dst, src, len, table);
}
```

**Why:** PAR2 repair is compute-intensive; x86_64 falls back to scalar while aarch64 gets NEON. The existing NEON implementation (galois.rs:186-244) uses nibble-table lookups via `vqtbl1q_u8` — the same approach maps directly to SSSE3 `pshufb` (`_mm_shuffle_epi8`).

**Hot callers:** `crates/bergamot-par2/src/repair.rs` — `mul_slice_inplace` (~line 307) and `muladd_with_table` (~line 332).

**Fix — Add SSSE3 (and optional AVX2) variants mirroring the NEON nibble-table method:**

1. **Enable `NibbleTable` on x86_64:**
   Change `#[cfg(target_arch = "aarch64")]` on the `nibble` field in `MulTable` (line 89) and its construction (lines 109-126) to:
   ```rust
   #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
   ```

2. **Add SSSE3 `muladd_ssse3` function:**
   - `#[target_feature(enable = "ssse3")] unsafe fn muladd_ssse3(dst: &mut [u8], src: &[u8], len: usize, table: &MulTable)`
   - Process 16-byte chunks using `_mm_shuffle_epi8` for nibble lookups (equivalent to NEON `vqtbl1q_u8`).
   - Extract even/odd bytes: use shuffle masks `[0,2,4,6,8,10,12,14, 0x80,0x80,...]` and `[1,3,5,7,9,11,13,15, 0x80,...]`.
   - Nibble extraction: `n0 = even & 0x0F`, `n1 = (even >> 4) & 0x0F` (use `_mm_srli_epi16` then mask), same for odd.
   - Lookup: `r_lo = pshufb(t0_lo, n0) ^ pshufb(t1_lo, n1) ^ pshufb(t2_lo, n2) ^ pshufb(t3_lo, n3)`, same for `r_hi`.
   - Repack: `product = _mm_unpacklo_epi8(r_lo, r_hi)` (equivalent to NEON `vzip1q_u8`).
   - XOR with dst chunk and store.
   - Tail: scalar for remaining bytes.

3. **Add SSSE3 `mul_slice_inplace_ssse3`:** same pattern but store `product` directly (no XOR with dst).

4. **Add runtime dispatch:**
   ```rust
   #[cfg(target_arch = "x86_64")]
   fn muladd_x86_dispatch(dst: &mut [u8], src: &[u8], len: usize, table: &MulTable) {
       if is_x86_feature_detected!("ssse3") {
           unsafe { muladd_ssse3(dst, src, len, table) }
       } else {
           muladd_scalar(dst, src, len, table)
       }
   }
   ```

5. **Update dispatch points:**
   - `muladd_with_table`: add `#[cfg(target_arch = "x86_64")] { muladd_x86_dispatch(...); }` before the `#[cfg(not(...))]` scalar fallback.
   - `mul_slice_inplace`: same pattern.

**Invariants:** Must preserve exact GF(2^16) arithmetic. Little-endian u16 word interpretation must match scalar path byte-for-byte. SSSE3 `pshufb` zeros output bytes when high bit of selector is set; use `0x80` for zero-fill in shuffle masks. For buffers < 16 bytes, use scalar.

**Tests:** Existing `galois.rs` tests (`muladd_basic`, `muladd_accumulates`, `muladd_zero_coeff_is_noop`, `mul_slice_inplace_*`, `multable_matches_mul`) validate correctness across backends. Add:
```rust
#[cfg(target_arch = "x86_64")]
#[test]
fn muladd_ssse3_matches_scalar() {
    if !std::arch::is_x86_feature_detected!("ssse3") { return; }
    // Compare scalar vs SSSE3 outputs on varied data sizes and coefficients
}
```
