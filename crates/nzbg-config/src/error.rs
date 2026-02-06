use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("syntax error on line {line}: {message}")]
    SyntaxError { line: usize, message: String },

    #[error("unknown variable: ${0}")]
    UnknownVariable(String),

    #[error("invalid value for {option}: {value}")]
    InvalidValue { option: String, value: String },

    #[error("unknown option: {0}")]
    UnknownOption(String),

    #[error("read-only option: {0}")]
    ReadOnlyOption(String),

    #[error("missing required option: {0}")]
    MissingRequired(String),

    #[error("I/O error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("config file not found: {0}")]
    FileNotFound(String),
}
