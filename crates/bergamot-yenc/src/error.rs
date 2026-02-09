use thiserror::Error;

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum YencError {
    #[error("missing required header field: {field}")]
    MissingField { field: &'static str },

    #[error("invalid header value for {field}: {value:?}")]
    InvalidHeaderValue { field: &'static str, value: String },

    #[error("CRC32 mismatch at {level:?}: expected {expected:#010x}, got {actual:#010x}")]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrcLevel {
    Part,
    File,
}
