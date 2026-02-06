use thiserror::Error;

#[derive(Debug, Error)]
pub enum QueueError {
    #[error("NZB not found: {0}")]
    NzbNotFound(u32),

    #[error("queue is shutting down")]
    Shutdown,

    #[error("invalid move position")]
    InvalidMove,
}
