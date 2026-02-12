use std::collections::HashMap;

use crc32fast::Hasher;

use crate::error::{CrcLevel, YencError};
use crate::model::{DecodedSegment, DecoderState};

pub fn decode_yenc_line(line: &[u8], output: &mut Vec<u8>) -> usize {
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

#[derive(Debug)]
pub struct YencDecoder {
    state: DecoderState,
    filename: String,
    file_size: u64,
    part_number: Option<u32>,
    part_begin: Option<u64>,
    part_end: Option<u64>,
    part_crc: Hasher,
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
            part_crc: Hasher::new(),
            decoded: Vec::new(),
        }
    }

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
                let before = self.decoded.len();
                decode_yenc_line(line, &mut self.decoded);
                self.part_crc
                    .update(&self.decoded[before..self.decoded.len()]);
                if let Some(end) = self.part_end
                    && self.decoded.len() as u64 > end - self.part_begin.unwrap_or(1) + 1
                {
                    return Err(YencError::SizeOverflow);
                }
                Ok(None)
            }
            DecoderState::Finished => Ok(None),
            DecoderState::Error(ref e) => Err(e.clone()),
        }
    }

    fn parse_ybegin(&mut self, line: &[u8]) -> Result<(), YencError> {
        let fields = parse_key_values(line)?;
        let size = fields
            .get("size")
            .ok_or(YencError::MissingField { field: "size" })?;
        let name = fields
            .get("name")
            .ok_or(YencError::MissingField { field: "name" })?;
        self.file_size = parse_u64("size", size)?;
        self.filename = name.clone();
        self.part_number = fields
            .get("part")
            .map(|val| parse_u32("part", val))
            .transpose()?;
        Ok(())
    }

    fn parse_ypart(&mut self, line: &[u8]) -> Result<(), YencError> {
        let fields = parse_key_values(line)?;
        let begin = fields
            .get("begin")
            .ok_or(YencError::MissingField { field: "begin" })?;
        let end = fields
            .get("end")
            .ok_or(YencError::MissingField { field: "end" })?;
        self.part_begin = Some(parse_u64("begin", begin)?);
        self.part_end = Some(parse_u64("end", end)?);
        Ok(())
    }

    fn parse_yend(&mut self, line: &[u8]) -> Result<Option<u32>, YencError> {
        let fields = parse_key_values(line)?;
        let expected_crc = fields
            .get("pcrc32")
            .map(|val| parse_hex_u32("pcrc32", val))
            .transpose()?;
        let expected_size = fields
            .get("size")
            .map(|val| parse_u64("size", val))
            .transpose()?;
        if let Some(expected_size) = expected_size
            && expected_size != self.decoded.len() as u64
        {
            return Err(YencError::SizeOverflow);
        }
        Ok(expected_crc)
    }

    fn finalize_segment(
        &mut self,
        expected_crc: Option<u32>,
    ) -> Result<Option<DecodedSegment>, YencError> {
        if self.decoded.is_empty() {
            return Err(YencError::UnexpectedEnd);
        }
        let actual_crc = self.part_crc.clone().finalize();
        if let Some(expected) = expected_crc
            && expected != actual_crc
        {
            return Err(YencError::CrcMismatch {
                expected,
                actual: actual_crc,
                level: CrcLevel::Part,
            });
        }

        let begin = self.part_begin.unwrap_or(1);
        let end = self
            .part_end
            .unwrap_or(begin + self.decoded.len() as u64 - 1);
        let data = std::mem::take(&mut self.decoded);
        self.part_crc = Hasher::new();

        Ok(Some(DecodedSegment {
            begin,
            end,
            data,
            crc32: actual_crc,
        }))
    }
}

impl Default for YencDecoder {
    fn default() -> Self {
        Self::new()
    }
}

fn parse_key_values(line: &[u8]) -> Result<HashMap<String, String>, YencError> {
    let text = std::str::from_utf8(line)
        .map_err(|_| YencError::InvalidHeaderValue {
            field: "line",
            value: String::from("<non-utf8>"),
        })?
        .trim();
    let mut map = HashMap::new();
    for token in text.split_whitespace().skip(1) {
        if let Some((key, value)) = token.split_once('=') {
            map.insert(key.to_string(), value.to_string());
        }
    }
    Ok(map)
}

fn parse_u64(field: &'static str, value: &str) -> Result<u64, YencError> {
    value.parse().map_err(|_| YencError::InvalidHeaderValue {
        field,
        value: value.to_string(),
    })
}

fn parse_u32(field: &'static str, value: &str) -> Result<u32, YencError> {
    value.parse().map_err(|_| YencError::InvalidHeaderValue {
        field,
        value: value.to_string(),
    })
}

fn parse_hex_u32(field: &'static str, value: &str) -> Result<u32, YencError> {
    let value = value.trim();
    if value.len() != 8 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(YencError::InvalidHeaderValue {
            field,
            value: value.to_string(),
        });
    }
    u32::from_str_radix(value, 16).map_err(|_| YencError::InvalidHeaderValue {
        field,
        value: value.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_yenc_line_unescapes_bytes() {
        let encoded = b"=ybegin line=128 size=1 name=test\r\n";
        let mut out = Vec::new();
        decode_yenc_line(encoded, &mut out);
        assert!(!out.is_empty());
    }

    #[test]
    fn parse_single_part_segment() {
        let lines = vec![
            b"=ybegin line=128 size=3 name=test.bin\r\n".to_vec(),
            vec![b'a' + 42, b'b' + 42, b'c' + 42, b'\r', b'\n'],
            b"=yend size=3 pcrc32=352441c2\r\n".to_vec(),
        ];

        let mut decoder = YencDecoder::new();
        let mut segment = None;
        for line in &lines {
            if let Some(result) = decoder.decode_line(line).unwrap() {
                segment = Some(result);
            }
        }

        let segment = segment.expect("segment decoded");
        assert_eq!(segment.data, b"abc");
        assert_eq!(segment.begin, 1);
        assert_eq!(segment.end, 3);
    }

    #[test]
    fn decode_reports_crc_mismatch() {
        let lines = vec![
            b"=ybegin line=128 size=3 name=test.bin\r\n".to_vec(),
            vec![b'a' + 42, b'b' + 42, b'c' + 42, b'\r', b'\n'],
            b"=yend size=3 pcrc32=00000000\r\n".to_vec(),
        ];

        let mut decoder = YencDecoder::new();
        let mut error = None;
        for line in &lines {
            if let Err(err) = decoder.decode_line(line) {
                error = Some(err);
                break;
            }
        }

        let error = error.expect("error returned");
        assert_eq!(
            error,
            YencError::CrcMismatch {
                expected: 0,
                actual: crc32fast::hash(b"abc"),
                level: CrcLevel::Part,
            }
        );
    }
}
