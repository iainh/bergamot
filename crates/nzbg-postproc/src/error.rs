use thiserror::Error;

#[derive(Debug, Error)]
pub enum PostProcessError {
    #[error("PAR2 error: {0}")]
    Par2(#[from] Par2Error),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("unpack error: {message}")]
    Unpack { message: String },
}

#[derive(Debug, Error)]
pub enum Par2Error {
    #[error("command failed: {message}")]
    CommandFailed { message: String },

    #[error("command returned non-zero exit: {code:?}")]
    ExitStatus { code: Option<i32> },
}
