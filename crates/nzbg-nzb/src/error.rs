use thiserror::Error;

#[derive(Debug, Error)]
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
