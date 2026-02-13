# NZB file parsing

## NZB XML format

NZB (Newzbin) files are XML documents that describe a set of Usenet articles needed
to reconstruct binary files. The format was created by the Newzbin indexing service
and is now the de facto standard for Usenet binary downloading.

### Example NZB file

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE nzb PUBLIC "-//newzbin//DTD NZB 1.1//EN"
  "http://www.newzbin.com/DTD/nzb/nzb-1.1.dtd">
<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">

  <head>
    <meta type="title">My.Linux.Distro.x64</meta>
    <meta type="password">secret123</meta>
    <meta type="tag">linux</meta>
    <meta type="category">Apps</meta>
  </head>

  <file poster="user@example.com (User)"
        date="1706140800"
        subject='My.Linux.Distro.x64 [01/15] - "distro.part01.rar" yEnc (1/50)'>
    <groups>
      <group>alt.binaries.linux</group>
      <group>alt.binaries.misc</group>
    </groups>
    <segments>
      <segment bytes="739811" number="1">part1of50.abc123@news.example.com</segment>
      <segment bytes="739811" number="2">part2of50.abc123@news.example.com</segment>
      <segment bytes="184953" number="3">part3of50.abc123@news.example.com</segment>
    </segments>
  </file>

  <file poster="user@example.com (User)"
        date="1706140900"
        subject='My.Linux.Distro.x64 [02/15] - "distro.part02.rar" yEnc (1/50)'>
    <groups>
      <group>alt.binaries.linux</group>
    </groups>
    <segments>
      <segment bytes="739811" number="1">part1of50.def456@news.example.com</segment>
      <segment bytes="739811" number="2">part2of50.def456@news.example.com</segment>
    </segments>
  </file>

</nzb>
```

### Element reference

| Element | Parent | Attributes | Content |
|---------|--------|------------|---------|
| `<nzb>` | root | `xmlns` | Container for the entire NZB |
| `<head>` | `<nzb>` | — | Optional metadata section |
| `<meta>` | `<head>` | `type` (key name) | Text value of the metadata entry |
| `<file>` | `<nzb>` | `poster`, `date` (unix epoch), `subject` | One logical file (e.g., a RAR volume) |
| `<groups>` | `<file>` | — | Newsgroups where the articles were posted |
| `<group>` | `<groups>` | — | Newsgroup name text |
| `<segments>` | `<file>` | — | Article segments composing the file |
| `<segment>` | `<segments>` | `bytes` (segment size), `number` (1-based order) | Message-ID (without angle brackets) |

---

## Parser strategy: `quick-xml`

| Crate | Style | Allocation | Speed | Recommendation |
|-------|-------|------------|-------|----------------|
| **quick-xml** | Pull / SAX streaming | Low — borrows from input buffer | Fastest | **Recommended** |
| xml-rs | Pull / Iterator | Medium | Moderate | Acceptable fallback |
| roxmltree | DOM tree | High — builds full tree in memory | Fast for small docs | Avoid for large NZBs |

`quick-xml` is the best fit because:

- **Streaming**: processes XML without loading the entire DOM, important for NZB files
  that can contain thousands of `<file>` and `<segment>` elements.
- **Zero-copy where possible**: borrows attribute values and text directly from the
  input buffer, minimizing allocations.
- **Battle-tested**: widely used in the Rust ecosystem (used by `calamine`, `serde-xml-rs`,
  and others).

```toml
[dependencies]
quick-xml = "0.37"
```

---

## Parsing flow

```
┌──────────┐     ┌──────────────┐     ┌─────────────────────────┐     ┌────────────────┐     ┌─────────┐
│ XML bytes│────▶│ quick-xml    │────▶│ NzbParser State Machine │────▶│ Post-processing│────▶│ NzbInfo │
│ (reader) │     │ Pull Reader  │     │                         │     │                │     │         │
└──────────┘     └──────────────┘     └─────────────────────────┘     └────────────────┘     └─────────┘
                                              │
                                     ┌────────┴────────┐
                                     │   Parse States  │
                                     ├─────────────────┤
                                     │ Initial         │
                                     │ InNzb           │
                                     │ InHead          │
                                     │ InMeta(key)     │
                                     │ InFile          │
                                     │ InGroups        │
                                     │ InGroup         │
                                     │ InSegments      │
                                     │ InSegment(attrs)│
                                     └─────────────────┘
```

### State transitions

```
Initial ──[<nzb>]──▶ InNzb
InNzb ──[<head>]──▶ InHead
InHead ──[<meta type="...">]──▶ InMeta(key)
InMeta ──[</meta>]──▶ InHead
InHead ──[</head>]──▶ InNzb
InNzb ──[<file ...>]──▶ InFile
InFile ──[<groups>]──▶ InGroups
InGroups ──[<group>]──▶ InGroup
InGroup ──[</group>]──▶ InGroups
InGroups ──[</groups>]──▶ InFile
InFile ──[<segments>]──▶ InSegments
InSegments ──[<segment ...>]──▶ InSegment(bytes, number)
InSegment ──[</segment>]──▶ InSegments
InSegments ──[</segments>]──▶ InFile
InFile ──[</file>]──▶ InNzb  (emit completed NzbFile)
InNzb ──[</nzb>]──▶ done
```

---

## Rust parser implementation sketch

```rust
use std::io::BufRead;
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

/// Represents a single article segment.
#[derive(Debug, Clone)]
pub struct Segment {
    pub number: u32,
    pub bytes: u64,
    pub message_id: String,
}

/// A logical file within an NZB.
#[derive(Debug, Clone)]
pub struct NzbFile {
    pub poster: String,
    pub date: i64,
    pub subject: String,
    pub filename: Option<String>,
    pub groups: Vec<String>,
    pub segments: Vec<Segment>,
    pub par_status: ParStatus,
    pub total_size: u64,
}

/// Metadata from the `<head>` section.
#[derive(Debug, Clone, Default)]
pub struct NzbMeta {
    pub title: Option<String>,
    pub password: Option<String>,
    pub category: Option<String>,
    pub tags: Vec<String>,
    /// Raw key-value pairs for any unrecognized meta types.
    pub extra: Vec<(String, String)>,
}

/// Complete parsed NZB.
#[derive(Debug, Clone)]
pub struct NzbInfo {
    pub meta: NzbMeta,
    pub files: Vec<NzbFile>,
    pub total_size: u64,
    pub file_count: usize,
    pub total_segments: usize,
    pub content_hash: u32,
    pub name_hash: u32,
}

#[derive(Debug)]
enum ParseState {
    Initial,
    InNzb,
    InHead,
    InMeta(String),
    InFile,
    InGroups,
    InGroup,
    InSegments,
    InSegment { bytes: u64, number: u32 },
}

pub struct NzbParser {
    state: ParseState,
    meta: NzbMeta,
    files: Vec<NzbFile>,
    // Work-in-progress accumulators
    current_file: Option<NzbFileBuilder>,
    current_text: String,
}

struct NzbFileBuilder {
    poster: String,
    date: i64,
    subject: String,
    groups: Vec<String>,
    segments: Vec<Segment>,
}

impl NzbParser {
    pub fn new() -> Self {
        Self {
            state: ParseState::Initial,
            meta: NzbMeta::default(),
            files: Vec::new(),
            current_file: None,
            current_text: String::new(),
        }
    }

    pub fn parse<R: BufRead>(mut self, input: R) -> Result<NzbInfo, NzbError> {
        let mut reader = Reader::from_reader(input);
        reader.config_mut().trim_text(true);
        let mut buf = Vec::with_capacity(4096);

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => self.handle_start(e)?,
                Ok(Event::End(ref e)) => self.handle_end(e.name().as_ref())?,
                Ok(Event::Text(ref e)) => {
                    self.current_text.push_str(
                        &e.unescape().map_err(|e| NzbError::XmlError(e.to_string()))?,
                    );
                }
                Ok(Event::Eof) => break,
                Ok(_) => {} // Skip comments, PI, decl, etc.
                Err(e) => return Err(NzbError::XmlError(e.to_string())),
            }
            buf.clear();
        }

        if self.files.is_empty() {
            return Err(NzbError::NoFiles);
        }

        Ok(self.build_nzb_info())
    }

    fn handle_start(&mut self, e: &BytesStart) -> Result<(), NzbError> {
        let tag = e.name();
        self.current_text.clear();

        self.state = match (&self.state, tag.as_ref()) {
            (ParseState::Initial, b"nzb") => ParseState::InNzb,
            (ParseState::InNzb, b"head") => ParseState::InHead,
            (ParseState::InHead, b"meta") => {
                let key = Self::get_attr(e, b"type")?
                    .unwrap_or_default();
                ParseState::InMeta(key)
            }
            (ParseState::InNzb, b"file") => {
                let poster = Self::get_attr(e, b"poster")?.unwrap_or_default();
                let date = Self::get_attr(e, b"date")?
                    .and_then(|s| s.parse::<i64>().ok())
                    .unwrap_or(0);
                let subject = Self::get_attr(e, b"subject")?.unwrap_or_default();
                self.current_file = Some(NzbFileBuilder {
                    poster,
                    date,
                    subject,
                    groups: Vec::new(),
                    segments: Vec::new(),
                });
                ParseState::InFile
            }
            (ParseState::InFile, b"groups") => ParseState::InGroups,
            (ParseState::InGroups, b"group") => ParseState::InGroup,
            (ParseState::InFile, b"segments") => ParseState::InSegments,
            (ParseState::InSegments, b"segment") => {
                let bytes = Self::get_attr(e, b"bytes")?
                    .and_then(|s| s.parse::<u64>().ok())
                    .ok_or(NzbError::InvalidSegment("missing bytes attribute".into()))?;
                let number = Self::get_attr(e, b"number")?
                    .and_then(|s| s.parse::<u32>().ok())
                    .ok_or(NzbError::InvalidSegment("missing number attribute".into()))?;
                ParseState::InSegment { bytes, number }
            }
            _ => return Ok(()), // Ignore unknown elements
        };
        Ok(())
    }

    fn handle_end(&mut self, tag: &[u8]) -> Result<(), NzbError> {
        self.state = match (&self.state, tag) {
            (ParseState::InMeta(key), b"meta") => {
                let value = std::mem::take(&mut self.current_text);
                match key.as_str() {
                    "title" => self.meta.title = Some(value),
                    "password" => self.meta.password = Some(value),
                    "category" => self.meta.category = Some(value),
                    "tag" => self.meta.tags.push(value),
                    other => self.meta.extra.push((other.to_string(), value)),
                }
                ParseState::InHead
            }
            (ParseState::InHead, b"head") => ParseState::InNzb,
            (ParseState::InGroup, b"group") => {
                if let Some(ref mut f) = self.current_file {
                    f.groups.push(std::mem::take(&mut self.current_text));
                }
                ParseState::InGroups
            }
            (ParseState::InGroups, b"groups") => ParseState::InFile,
            (ParseState::InSegment { bytes, number }, b"segment") => {
                let message_id = std::mem::take(&mut self.current_text);
                if let Some(ref mut f) = self.current_file {
                    f.segments.push(Segment {
                        number: *number,
                        bytes: *bytes,
                        message_id,
                    });
                }
                ParseState::InSegments
            }
            (ParseState::InSegments, b"segments") => ParseState::InFile,
            (ParseState::InFile, b"file") => {
                if let Some(builder) = self.current_file.take() {
                    let filename = extract_filename(&builder.subject);
                    let par_status = classify_par(&filename);
                    let total_size = builder.segments.iter().map(|s| s.bytes).sum();
                    self.files.push(NzbFile {
                        poster: builder.poster,
                        date: builder.date,
                        subject: builder.subject,
                        filename,
                        groups: builder.groups,
                        segments: builder.segments,
                        par_status,
                        total_size,
                    });
                }
                ParseState::InNzb
            }
            (ParseState::InNzb, b"nzb") => ParseState::Initial,
            _ => return Ok(()),
        };
        Ok(())
    }

    fn get_attr(e: &BytesStart, name: &[u8]) -> Result<Option<String>, NzbError> {
        for attr in e.attributes().flatten() {
            if attr.key.as_ref() == name {
                return Ok(Some(
                    attr.unescape_value()
                        .map_err(|e| NzbError::XmlError(e.to_string()))?
                        .into_owned(),
                ));
            }
        }
        Ok(None)
    }

    fn build_nzb_info(self) -> NzbInfo {
        let total_size = self.files.iter().map(|f| f.total_size).sum();
        let total_segments = self.files.iter().map(|f| f.segments.len()).sum();
        let file_count = self.files.len();
        let content_hash = compute_content_hash(&self.files);
        let name_hash = compute_name_hash(&self.files);
        NzbInfo {
            meta: self.meta,
            files: self.files,
            total_size,
            file_count,
            total_segments,
            content_hash,
            name_hash,
        }
    }
}
```

---

## Filename extraction from subject lines

Usenet posting subjects follow informal but consistent patterns. The filename is
almost always enclosed in double quotes within the subject. Common formats:

| Pattern | Example Subject |
|---------|----------------|
| Standard yEnc | `My.File [01/15] - "file.part01.rar" yEnc (1/50)` |
| No index | `"file.part01.rar" yEnc (1/50)` |
| Alt format | `(My File) [01/15] - "file.part01.rar" - 750.00 MB yEnc` |
| No yEnc tag | `My.File - "file.part01.rar" (1/50)` |

### Extraction logic

```rust
use regex::Regex;
use std::sync::LazyLock;

static FILENAME_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#""([^"]+\.[a-zA-Z0-9]{2,4})""#).unwrap()
});

/// Extracts the filename from a Usenet subject line.
/// Returns the last quoted string that looks like a filename.
pub fn extract_filename(subject: &str) -> Option<String> {
    FILENAME_RE
        .captures_iter(subject)
        .last()
        .map(|cap| cap[1].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_standard_yenc_subject() {
        let subject = r#"My.Linux.Distro [01/15] - "distro.part01.rar" yEnc (1/50)"#;
        assert_eq!(extract_filename(subject), Some("distro.part01.rar".into()));
    }

    #[test]
    fn test_no_filename() {
        let subject = "Random text without any quoted filename";
        assert_eq!(extract_filename(subject), None);
    }
}
```

---

## PAR2 file detection and classification

PAR2 (Parity Archive 2) files provide error correction for Usenet downloads. The
downloader must identify them to support repair workflows:

| Classification | Pattern | Purpose |
|----------------|---------|---------|
| `MainPar` | `*.par2` (no `.vol` prefix) | Index file listing recoverable files |
| `RepairVolume` | `*.vol00+01.par2`, `*.vol01+02.par2` | Recovery blocks for repair |
| `NotPar` | Everything else | Regular content file |

```rust
use regex::Regex;
use std::sync::LazyLock;

#[derive(Debug, Clone, PartialEq)]
pub enum ParStatus {
    NotPar,
    MainPar,
    RepairVolume { block_offset: u32, block_count: u32 },
}

static PAR2_VOL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\.vol(\d+)\+(\d+)\.par2$").unwrap()
});

/// Classify a filename (or subject) as a PAR2 file type.
pub fn classify_par(filename: &Option<String>) -> ParStatus {
    let name = match filename {
        Some(n) => n,
        None => return ParStatus::NotPar,
    };
    let lower = name.to_ascii_lowercase();
    if !lower.ends_with(".par2") {
        return ParStatus::NotPar;
    }
    if let Some(caps) = PAR2_VOL_RE.captures(name) {
        let offset = caps[1].parse().unwrap_or(0);
        let count = caps[2].parse().unwrap_or(0);
        ParStatus::RepairVolume {
            block_offset: offset,
            block_count: count,
        }
    } else {
        ParStatus::MainPar
    }
}
```

---

## File ordering (ReorderFiles)

When `ReorderFiles` is enabled, parsed files are sorted to optimize download
and post-processing:

1. **Regular content files** first — sorted by filename to ensure volume order
   (e.g., `file.part01.rar` before `file.part02.rar`).
2. **Main PAR2 index** file — downloaded after content to check if repair is needed.
3. **PAR2 repair volumes** — sorted by `block_count` ascending (smallest first),
   downloaded only as needed for repair.

```rust
impl NzbInfo {
    pub fn reorder_files(&mut self) {
        self.files.sort_by(|a, b| {
            let ord_a = file_sort_order(a);
            let ord_b = file_sort_order(b);
            ord_a.cmp(&ord_b)
        });
    }
}

fn file_sort_order(file: &NzbFile) -> (u8, u32, String) {
    match &file.par_status {
        ParStatus::NotPar => (0, 0, file.filename.clone().unwrap_or_default()),
        ParStatus::MainPar => (1, 0, file.filename.clone().unwrap_or_default()),
        ParStatus::RepairVolume { block_count, .. } => {
            (2, *block_count, file.filename.clone().unwrap_or_default())
        }
    }
}
```

---

## Hash calculation for duplicate detection

Two hashes are computed to detect duplicate NZB submissions:

### Content hash

Derived from the sorted message-IDs of all segments across all files. Two NZBs
with identical articles (regardless of metadata differences) produce the same
content hash.

```rust
use std::hash::{Hash, Hasher};
use std::collections::hash_map::DefaultHasher;

pub fn compute_content_hash(files: &[NzbFile]) -> u32 {
    let mut ids: Vec<&str> = files
        .iter()
        .flat_map(|f| f.segments.iter().map(|s| s.message_id.as_str()))
        .collect();
    ids.sort_unstable();

    let mut hasher = DefaultHasher::new();
    for id in ids {
        id.hash(&mut hasher);
    }
    hasher.finish() as u32
}
```

### Name hash

Derived from sorted filenames extracted from subjects. Used to detect re-uploads
of the same content under different message-IDs.

```rust
pub fn compute_name_hash(files: &[NzbFile]) -> u32 {
    let mut names: Vec<&str> = files
        .iter()
        .filter_map(|f| f.filename.as_deref())
        .collect();
    names.sort_unstable();

    let mut hasher = DefaultHasher::new();
    for name in names {
        name.hash(&mut hasher);
    }
    hasher.finish() as u32
}
```

---

## Post-parse processing steps

After the XML is parsed into raw structures, several processing steps produce the
final `NzbInfo`:

| Step | Description |
|------|-------------|
| **Size calculation** | Sum `segment.bytes` per file → `NzbFile::total_size`; sum all files → `NzbInfo::total_size` |
| **Segment sorting** | Sort each file's segments by `number` ascending |
| **Health calculation** | `health = available_segments / expected_segments`; flag files with missing segments |
| **Filename extraction** | Apply `extract_filename()` to each file's subject |
| **Filename dedup** | Append `_2`, `_3`, etc. if duplicate filenames are found |
| **PAR2 classification** | Apply `classify_par()` to each extracted filename |
| **ID assignment** | Assign each `NzbFile` a stable numeric ID (index in the files vec) |
| **Group validation** | Warn/skip files with no `<group>` elements |
| **Parameter extraction** | Populate `NzbMeta` from `<meta>` elements (title, password, category, tags) |
| **Hash computation** | Compute `content_hash` and `name_hash` for duplicate detection |
| **File reordering** | If `ReorderFiles` is enabled, sort files by type and name |

---

## Error types

```rust
#[derive(Debug, thiserror::Error)]
pub enum NzbError {
    #[error("XML parsing error: {0}")]
    XmlError(String),

    #[error("Malformed NZB structure: {0}")]
    MalformedNzb(String),

    #[error("NZB contains no files")]
    NoFiles,

    #[error("Invalid segment: {0}")]
    InvalidSegment(String),

    #[error("I/O error: {0}")]
    IoError(#[from] std::io::Error),
}
```

---

## NZB sources

NZB files can enter the system through several paths:

| Source | Description | Entry Point |
|--------|-------------|-------------|
| **File upload** | User uploads an NZB via the web UI or API (`append` method) | `POST /api/append` |
| **Directory scan** | Daemon watches `NzbDir` for new `.nzb` files (and `.nzb.gz`) | `ScanDir` polling loop |
| **URL fetch** | User provides a URL; downloader fetches the NZB over HTTP(S) | `POST /api/appendurl` |
| **RSS feed** | RSS/Atom feeds are polled; matching items have their NZB URLs fetched | `RssChecker` scheduler |
| **API append** | Raw NZB XML content submitted directly via the JSON-RPC/XML-RPC API | `append` RPC method |

All sources converge on the same `NzbParser::parse()` entry point. The system
handles `.nzb.gz` (gzip-compressed) transparently by detecting the gzip magic
bytes (`1f 8b`) and wrapping the reader in a `flate2::read::GzDecoder`.

```rust
use std::io::{BufReader, Read};
use flate2::read::GzDecoder;

pub fn parse_nzb_auto(data: &[u8]) -> Result<NzbInfo, NzbError> {
    let is_gzip = data.len() >= 2 && data[0] == 0x1f && data[1] == 0x8b;

    if is_gzip {
        let decoder = GzDecoder::new(data);
        let reader = BufReader::new(decoder);
        NzbParser::new().parse(reader)
    } else {
        let reader = BufReader::new(data);
        NzbParser::new().parse(reader)
    }
}
```
