use thiserror::Error;

#[derive(Debug, Error)]
pub enum NntpError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("TLS error: {0}")]
    TlsError(String),

    #[error("Authentication failed: {0}")]
    AuthFailed(String),

    #[error("Authentication required")]
    AuthRequired,

    #[error("Article not found: {0}")]
    ArticleNotFound(String),

    #[error("Unexpected response {0}: {1}")]
    UnexpectedResponse(u16, String),

    #[error("Protocol error: {0}")]
    ProtocolError(String),

    #[error("CRC mismatch: expected {expected:#010x}, got {actual:#010x}")]
    CrcMismatch { expected: u32, actual: u32 },

    #[error("Connection timed out")]
    Timeout,

    #[error("Server blocked: {0}")]
    ServerBlocked(String),

    #[error("Quota exceeded")]
    QuotaExceeded,

    #[error("No news servers configured")]
    NoServersConfigured,
}
