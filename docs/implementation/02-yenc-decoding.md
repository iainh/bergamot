# yEnc decoding & article assembly

## yEnc format overview

yEnc is the standard binary encoding used on Usenet. Each byte of the original
binary data is encoded by adding 42 (mod 256) to produce a printable character.
A small set of "critical" bytes that would break NNTP framing are escaped with a
`=` prefix, where the escaped byte has an additional 64 added.

Decoding is the reverse: subtract 42 from each character, handling escape
sequences by subtracting 106 (42 + 64) from the byte following `=`.

## Message structure

### Single-part article

```
=ybegin line=128 size=123456 name=example.bin
<encoded data lines>
=yend size=123456 crc32=ABCD1234
```

### Multi-part article

```
=ybegin part=1 line=128 size=123456 name=example.bin
=ypart begin=1 end=65536
<encoded data lines>
=yend size=65536 part=1 pcrc32=DEAD0000 crc32=ABCD1234
```

The `=ypart` header specifies the byte range within the original file that this
segment covers. `begin` is 1-indexed.

## Header fields

| Field   | Header    | Description                                      |
|---------|-----------|--------------------------------------------------|
| `line`  | `=ybegin` | Maximum encoded line length                      |
| `size`  | `=ybegin` | Total file size in bytes                         |
| `name`  | `=ybegin` | Original filename                                |
| `part`  | `=ybegin` | Part number (multi-part only)                    |
| `begin` | `=ypart`  | Start offset in file (1-indexed)                 |
| `end`   | `=ypart`  | End offset in file (inclusive)                    |
| `pcrc32`| `=yend`   | CRC32 of this part's decoded data                |
| `crc32` | `=yend`   | CRC32 of the entire file (often only on last part)|

## Core decode algorithm

```rust
/// Decode a single yEnc-encoded line into `output`.
/// Returns the number of bytes written.
fn decode_yenc_line(line: &[u8], output: &mut Vec<u8>) -> usize {
    let mut i = 0;
    let start_len = output.len();

    while i < line.len() {
        let b = line[i];
        match b {
            b'\r' | b'\n' => {
                i += 1;
                continue;
            }
            b'=' => {
                i += 1;
                if i < line.len() {
                    output.push(line[i].wrapping_sub(106));
                }
            }
            _ => {
                output.push(b.wrapping_sub(42));
            }
        }
        i += 1;
    }

    output.len() - start_len
}
```

## Escaped characters

| Original Byte | Value | Encoded As | Reason                        |
|---------------|-------|------------|-------------------------------|
| NUL           | 0x00  | `=@`      | Null byte breaks protocols    |
| LF            | 0x0A  | `=J`      | Line terminator               |
| CR            | 0x0D  | `=M`      | Line terminator               |
| `=`           | 0x3D  | `=}`      | Escape character itself       |
| `.`           | 0x2E  | `=n`      | Dot at line start (NNTP dot-stuffing) |

Each escaped form is computed as `= (original + 42 + 64) mod 256`.

## Streaming decoder design

The decoder processes article data line-by-line as it arrives from the NNTP
connection, avoiding the need to buffer the entire article in memory.

```rust
pub enum DecoderState {
    /// Waiting for `=ybegin` header line.
    WaitingForHeader,
    /// Header parsed; waiting for `=ypart` (multi-part) or first data line.
    WaitingForPart,
    /// Actively decoding body data.
    DecodingBody,
    /// Reached `=yend`; decoding complete.
    Finished,
    /// An unrecoverable error occurred.
    Error(YencError),
}

pub struct YencDecoder {
    state: DecoderState,
    /// Parsed header information.
    filename: String,
    file_size: u64,
    part_number: Option<u32>,
    part_begin: Option<u64>,
    part_end: Option<u64>,
    /// Running CRC32 of decoded bytes for this part.
    part_crc: crc32fast::Hasher,
    /// Accumulated decoded data for the current part.
    decoded: Vec<u8>,
}

impl YencDecoder {
    pub fn new() -> Self {
        Self {
            state: DecoderState::WaitingForHeader,
            filename: String::new(),
            file_size: 0,
            part_number: None,
            part_begin: None,
            part_end: None,
            part_crc: crc32fast::Hasher::new(),
            decoded: Vec::new(),
        }
    }

    /// Feed a single line (including any trailing CRLF) to the decoder.
    /// Returns `Ok(Some(segment))` when a complete part has been decoded.
    pub fn decode_line(&mut self, line: &[u8]) -> Result<Option<DecodedSegment>, YencError> {
        match self.state {
            DecoderState::WaitingForHeader => {
                if line.starts_with(b"=ybegin ") {
                    self.parse_ybegin(line)?;
                    self.state = if self.part_number.is_some() {
                        DecoderState::WaitingForPart
                    } else {
                        DecoderState::DecodingBody
                    };
                }
                Ok(None)
            }
            DecoderState::WaitingForPart => {
                if line.starts_with(b"=ypart ") {
                    self.parse_ypart(line)?;
                    self.state = DecoderState::DecodingBody;
                }
                Ok(None)
            }
            DecoderState::DecodingBody => {
                if line.starts_with(b"=yend ") {
                    let expected_crc = self.parse_yend(line)?;
                    self.state = DecoderState::Finished;
                    return self.finalize_segment(expected_crc);
                }
                decode_yenc_line(line, &mut self.decoded);
                self.part_crc.write(&self.decoded[self.decoded.len()..]);
                Ok(None)
            }
            DecoderState::Finished => Ok(None),
            DecoderState::Error(ref e) => Err(e.clone()),
        }
    }
}
```

## CRC32 validation

Validation happens at two levels using `crc32fast`:

- **Segment level (`pcrc32`)** — verified immediately after decoding each part.
  A mismatch means the article is corrupt and should be re-downloaded or the
  segment discarded.
- **File level (`crc32`)** — verified after all parts have been assembled. A
  mismatch may indicate a missing or silently corrupt segment.

```rust
fn verify_part_crc(hasher: crc32fast::Hasher, expected: u32) -> Result<(), YencError> {
    let actual = hasher.finalize();
    if actual != expected {
        return Err(YencError::CrcMismatch {
            expected,
            actual,
            level: CrcLevel::Part,
        });
    }
    Ok(())
}
```

## YencError

```rust
#[derive(Debug, Clone, thiserror::Error)]
pub enum YencError {
    #[error("missing required header field: {field}")]
    MissingField { field: &'static str },

    #[error("invalid header value for {field}: {value:?}")]
    InvalidHeaderValue { field: &'static str, value: String },

    #[error("CRC32 mismatch at {level}: expected {expected:#010x}, got {actual:#010x}")]
    CrcMismatch {
        expected: u32,
        actual: u32,
        level: CrcLevel,
    },

    #[error("unexpected end of article")]
    UnexpectedEnd,

    #[error("article body data exceeds declared size")]
    SizeOverflow,
}

#[derive(Debug, Clone, Copy)]
pub enum CrcLevel {
    Part,
    File,
}
```

## Article writing strategies

### Direct write mode

Each decoded segment is written directly to the target output file at its
correct byte offset. The output file is pre-allocated to `file_size` bytes.

```rust
use std::io::{Seek, SeekFrom, Write};

fn write_segment_direct(
    file: &mut std::fs::File,
    segment: &DecodedSegment,
) -> std::io::Result<()> {
    // part_begin is 1-indexed in yEnc; convert to 0-indexed file offset.
    let offset = segment.begin - 1;
    file.seek(SeekFrom::Start(offset))?;
    file.write_all(&segment.data)?;
    Ok(())
}
```

### Temporary file mode

Segments are written to a temporary file (e.g., `filename.bergamot.tmp`) and
renamed to the final name only after all parts have been received and verified.
This prevents incomplete files from appearing in the output directory.

## Article cache

An in-memory cache holds decoded segments before they are flushed to disk. This
allows batching writes and reduces random I/O when segments arrive out of order.

```rust
use std::collections::HashMap;
use std::path::PathBuf;

pub struct ArticleCache {
    /// Keyed by (nzb_id, file_index) → ordered segments.
    entries: HashMap<(u32, u32), Vec<DecodedSegment>>,
    /// Current total bytes held in memory.
    current_size: usize,
    /// Maximum bytes before forced flush.
    max_size: usize,
}

pub struct DecodedSegment {
    pub begin: u64,
    pub end: u64,
    pub data: Vec<u8>,
    pub crc32: u32,
}

impl ArticleCache {
    pub fn new(max_size: usize) -> Self {
        Self {
            entries: HashMap::new(),
            current_size: 0,
            max_size,
        }
    }

    /// Store a decoded segment. If the cache exceeds `max_size`, the
    /// largest file's segments are flushed automatically.
    pub fn store(
        &mut self,
        nzb_id: u32,
        file_index: u32,
        segment: DecodedSegment,
    ) {
        self.current_size += segment.data.len();
        self.entries
            .entry((nzb_id, file_index))
            .or_default()
            .push(segment);

        if self.current_size > self.max_size {
            self.evict_largest();
        }
    }

    /// Flush all cached segments for a file to disk.
    pub fn flush_file(
        &mut self,
        nzb_id: u32,
        file_index: u32,
        output: &mut std::fs::File,
    ) -> std::io::Result<()> {
        if let Some(segments) = self.entries.remove(&(nzb_id, file_index)) {
            for seg in &segments {
                let offset = seg.begin - 1;
                output.seek(std::io::SeekFrom::Start(offset))?;
                output.write_all(&seg.data)?;
                self.current_size -= seg.data.len();
            }
        }
        Ok(())
    }

    fn evict_largest(&mut self) {
        // Find the key with the most bytes cached and flush it.
        // (Implementation would open/create the target file and call flush_file.)
    }
}
```

## File completion

When all articles for a file have been downloaded and decoded:

1. **Flush cache** — `article_cache.flush_file(...)` writes any remaining
   in-memory segments to the output file.
2. **Verify CRC** — if the `=yend` of the last part contained a file-level
   `crc32`, compute the CRC32 of the entire assembled file and compare.
3. **Rename** — move the temporary file to its final path.

```rust
fn complete_file(
    cache: &mut ArticleCache,
    nzb_id: u32,
    file_index: u32,
    tmp_path: &std::path::Path,
    final_path: &std::path::Path,
    expected_crc: Option<u32>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(tmp_path)?;

    cache.flush_file(nzb_id, file_index, &mut file)?;
    drop(file);

    if let Some(expected) = expected_crc {
        let data = std::fs::read(tmp_path)?;
        let actual = crc32fast::hash(&data);
        if actual != expected {
            return Err(Box::new(YencError::CrcMismatch {
                expected,
                actual,
                level: CrcLevel::File,
            }));
        }
    }

    std::fs::rename(tmp_path, final_path)?;
    Ok(())
}
```

## Performance considerations

| Technique              | Benefit                                                   |
|------------------------|-----------------------------------------------------------|
| Avoid copies           | Decode directly into a pre-allocated `Vec<u8>`; use `&[u8]` slices instead of `String` for header parsing. |
| SIMD acceleration      | The yEnc decode loop (subtract 42, detect `=`) is amenable to SIMD — scan 16/32 bytes at a time for `=`, `\r`, `\n` and batch-subtract the rest. |
| Hardware CRC32         | `crc32fast` auto-detects and uses hardware CRC32C instructions on x86_64 and aarch64. |
| Write buffering        | `BufWriter` around the output file reduces syscall overhead when writing many small segments. |
| Direct I/O             | On Linux, `O_DIRECT` bypasses the page cache for large sequential writes, reducing memory pressure. Use `std::os::unix::fs::OpenOptionsExt` to set flags. |
| Pre-allocation         | `fallocate` / `ftruncate` the output file to the declared `size` upfront to avoid fragmentation and repeated metadata updates. |
