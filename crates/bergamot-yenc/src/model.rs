use crate::error::YencError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecoderState {
    WaitingForHeader,
    WaitingForPart,
    DecodingBody,
    Finished,
    Error(YencError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedSegment {
    pub begin: u64,
    pub end: u64,
    pub data: Vec<u8>,
    pub crc32: u32,
}
