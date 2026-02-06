use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::BufRead;
use std::sync::LazyLock;

use flate2::read::GzDecoder;
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use regex::Regex;

use crate::error::NzbError;
use crate::model::{NzbFile, NzbInfo, NzbMeta, ParStatus, Segment};

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

#[derive(Debug)]
pub struct NzbParser {
    state: ParseState,
    meta: NzbMeta,
    files: Vec<NzbFile>,
    current_file: Option<NzbFileBuilder>,
    current_text: String,
}

#[derive(Debug)]
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
        let mut saw_nzb = false;

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => {
                    if e.name().as_ref() == b"nzb" {
                        saw_nzb = true;
                    }
                    self.handle_start(e)?;
                }
                Ok(Event::End(ref e)) => self.handle_end(e.name().as_ref())?,
                Ok(Event::Text(ref e)) => {
                    self.current_text.push_str(
                        &e.unescape()
                            .map_err(|e| NzbError::XmlError(e.to_string()))?,
                    );
                }
                Ok(Event::Eof) => break,
                Ok(_) => {}
                Err(e) => return Err(NzbError::XmlError(e.to_string())),
            }
            buf.clear();
        }

        if !saw_nzb {
            return Err(NzbError::MalformedNzb("missing nzb root".into()));
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
                let key = Self::get_attr(e, b"type")?.unwrap_or_default();
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
                    .ok_or_else(|| {
                        NzbError::InvalidSegment("missing bytes attribute".into())
                    })?;
                let number = Self::get_attr(e, b"number")?
                    .and_then(|s| s.parse::<u32>().ok())
                    .ok_or_else(|| {
                        NzbError::InvalidSegment("missing number attribute".into())
                    })?;
                ParseState::InSegment { bytes, number }
            }
            _ => return Ok(()),
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
                    if message_id.is_empty() {
                        return Err(NzbError::InvalidSegment(
                            "missing message id".into(),
                        ));
                    }
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
                    if builder.groups.is_empty() {
                        return Err(NzbError::MalformedNzb(
                            "file missing groups".into(),
                        ));
                    }
                    if builder.segments.is_empty() {
                        return Err(NzbError::MalformedNzb(
                            "file missing segments".into(),
                        ));
                    }
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

    fn build_nzb_info(mut self) -> NzbInfo {
        for file in &mut self.files {
            file.segments
                .sort_by(|left, right| left.number.cmp(&right.number));
        }

        ensure_unique_filenames(&mut self.files);

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

impl Default for NzbParser {
    fn default() -> Self {
        Self::new()
    }
}

pub fn parse_nzb_auto(data: &[u8]) -> Result<NzbInfo, NzbError> {
    let is_gzip = data.len() >= 2 && data[0] == 0x1f && data[1] == 0x8b;

    if is_gzip {
        let decoder = GzDecoder::new(data);
        let reader = std::io::BufReader::new(decoder);
        NzbParser::new().parse(reader)
    } else {
        let reader = std::io::BufReader::new(data);
        NzbParser::new().parse(reader)
    }
}

static FILENAME_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#""([^\"]+\.[a-zA-Z0-9]{2,4})""#).expect("valid regex")
});

pub fn extract_filename(subject: &str) -> Option<String> {
    FILENAME_RE
        .captures_iter(subject)
        .last()
        .map(|cap| cap[1].to_string())
}

static PAR2_VOL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\.vol(\d+)\+(\d+)\.par2$").expect("valid regex")
});

pub fn classify_par(filename: &Option<String>) -> ParStatus {
    let name = match filename {
        Some(n) => n,
        None => return ParStatus::NotPar,
    };
    let lower = name.to_ascii_lowercase();
    if !lower.ends_with(".par2") {
        return ParStatus::NotPar;
    }
    PAR2_VOL_RE
        .captures(name)
        .map(|caps| ParStatus::RepairVolume {
            block_offset: caps
                .get(1)
                .and_then(|cap| cap.as_str().parse().ok())
                .unwrap_or(0),
            block_count: caps
                .get(2)
                .and_then(|cap| cap.as_str().parse().ok())
                .unwrap_or(0),
        })
        .unwrap_or(ParStatus::MainPar)
}

pub fn compute_content_hash(files: &[NzbFile]) -> u32 {
    let mut ids: Vec<&str> = files
        .iter()
        .flat_map(|file| file.segments.iter().map(|segment| segment.message_id.as_str()))
        .collect();
    ids.sort_unstable();

    let mut hasher = DefaultHasher::new();
    for id in ids {
        id.hash(&mut hasher);
    }
    hasher.finish() as u32
}

pub fn compute_name_hash(files: &[NzbFile]) -> u32 {
    let mut names: Vec<&str> = files
        .iter()
        .filter_map(|file| file.filename.as_deref())
        .collect();
    names.sort_unstable();

    let mut hasher = DefaultHasher::new();
    for name in names {
        name.hash(&mut hasher);
    }
    hasher.finish() as u32
}

fn ensure_unique_filenames(files: &mut [NzbFile]) {
    use std::collections::HashMap;

    let mut used: HashMap<String, usize> = HashMap::new();

    for file in files {
        let filename = match file.filename.clone() {
            Some(name) if !name.is_empty() => name,
            _ => continue,
        };

        let current_index = match used.get(&filename) {
            Some(value) => *value,
            None => {
                used.insert(filename, 1);
                continue;
            }
        };

        let mut suffix = current_index + 1;
        loop {
            let candidate = format!("{}_{}", filename, suffix);
            if !used.contains_key(&candidate) {
                file.filename = Some(candidate.clone());
                used.insert(candidate, 1);
                used.insert(filename.clone(), suffix);
                break;
            }
            suffix += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
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
      <segment bytes="739811" number="2">part2of50.abc123@news.example.com</segment>
      <segment bytes="739811" number="1">part1of50.abc123@news.example.com</segment>
    </segments>
  </file>
</nzb>
"#;

    #[test]
    fn parse_nzb_extracts_meta_and_files() {
        let info = NzbParser::new()
            .parse(std::io::Cursor::new(SAMPLE))
            .expect("parse sample");

        assert_eq!(info.meta.title.as_deref(), Some("My.Linux.Distro.x64"));
        assert_eq!(info.meta.password.as_deref(), Some("secret123"));
        assert_eq!(info.meta.category.as_deref(), Some("Apps"));
        assert_eq!(info.meta.tags, vec!["linux".to_string()]);

        assert_eq!(info.file_count, 1);
        assert_eq!(info.total_segments, 2);
        assert_eq!(info.total_size, 1_479_622);

        let file = &info.files[0];
        assert_eq!(file.filename.as_deref(), Some("distro.part01.rar"));
        assert_eq!(file.segments[0].number, 1);
        assert_eq!(file.segments[1].number, 2);
    }

    #[test]
    fn parse_nzb_rejects_files_without_groups() {
        let xml = r#"<?xml version="1.0"?>
<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">
  <file poster="p" date="1" subject="test">
    <segments>
      <segment bytes="1" number="1">id</segment>
    </segments>
  </file>
</nzb>
"#;

        let err = NzbParser::new()
            .parse(std::io::Cursor::new(xml))
            .expect_err("parse should fail");

        match err {
            NzbError::MalformedNzb(message) => {
                assert!(message.contains("groups"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn parse_nzb_deduplicates_filenames() {
        let xml = r#"<?xml version="1.0"?>
<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">
  <file poster="p" date="1" subject='"same.rar" yEnc'>
    <groups>
      <group>alt.test</group>
    </groups>
    <segments>
      <segment bytes="1" number="1">id1</segment>
    </segments>
  </file>
  <file poster="p" date="2" subject='"same.rar" yEnc'>
    <groups>
      <group>alt.test</group>
    </groups>
    <segments>
      <segment bytes="1" number="1">id2</segment>
    </segments>
  </file>
</nzb>
"#;

        let info = NzbParser::new()
            .parse(std::io::Cursor::new(xml))
            .expect("parse");

        assert_eq!(info.files.len(), 2);
        assert_eq!(info.files[0].filename.as_deref(), Some("same.rar"));
        assert_eq!(info.files[1].filename.as_deref(), Some("same.rar_2"));
    }

    #[test]
    fn parse_nzb_auto_handles_gzip_payload() {
        use flate2::write::GzEncoder;
        use flate2::Compression;

        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        std::io::Write::write_all(&mut encoder, SAMPLE.as_bytes()).expect("write");
        let data = encoder.finish().expect("finish");

        let info = parse_nzb_auto(&data).expect("parse gzip");
        assert_eq!(info.file_count, 1);
    }

    #[test]
    fn extract_filename_handles_standard_subjects() {
        let subject = r#"My.Linux.Distro [01/15] - "distro.part01.rar" yEnc (1/50)"#;
        assert_eq!(extract_filename(subject), Some("distro.part01.rar".into()));
    }

    #[test]
    fn extract_filename_returns_none_when_missing() {
        let subject = "Random text without any quoted filename";
        assert_eq!(extract_filename(subject), None);
    }

    #[test]
    fn classify_par_returns_main_and_repair_variants() {
        assert_eq!(
            classify_par(&Some("show.par2".into())),
            ParStatus::MainPar
        );
        assert_eq!(
            classify_par(&Some("show.vol00+01.par2".into())),
            ParStatus::RepairVolume {
                block_offset: 0,
                block_count: 1,
            }
        );
        assert_eq!(
            classify_par(&Some("show.mkv".into())),
            ParStatus::NotPar
        );
    }

    #[test]
    fn compute_hashes_are_stable() {
        let files = vec![NzbFile {
            poster: "user".to_string(),
            date: 0,
            subject: "subject".to_string(),
            filename: Some("file1.rar".to_string()),
            groups: vec!["alt.test".to_string()],
            segments: vec![
                Segment {
                    number: 1,
                    bytes: 100,
                    message_id: "a".to_string(),
                },
                Segment {
                    number: 2,
                    bytes: 200,
                    message_id: "b".to_string(),
                },
            ],
            par_status: ParStatus::NotPar,
            total_size: 300,
        }];

        let content_hash = compute_content_hash(&files);
        let name_hash = compute_name_hash(&files);

        assert_eq!(content_hash, compute_content_hash(&files));
        assert_eq!(name_hash, compute_name_hash(&files));
    }
}
