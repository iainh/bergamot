use thiserror::Error;

#[derive(Debug, Error)]
pub enum Par2ParseError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("invalid packet header at offset {offset}")]
    InvalidHeader { offset: u64 },

    #[error("packet body too short for {packet_type}")]
    BodyTooShort { packet_type: &'static str },

    #[error("no main packet found")]
    NoMainPacket,

    #[error("inconsistent recovery set IDs")]
    InconsistentSetId,
}

#[derive(Debug, Error)]
pub enum Par2VerifyError {
    #[error("parse error: {0}")]
    Parse(#[from] Par2ParseError),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}
