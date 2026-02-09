use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Error)]
#[error("JSON-RPC error {code}: {message}")]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcErrorBody {
    pub code: i32,
    pub message: String,
}

impl From<JsonRpcError> for JsonRpcErrorBody {
    fn from(err: JsonRpcError) -> Self {
        Self {
            code: err.code,
            message: err.message,
        }
    }
}
