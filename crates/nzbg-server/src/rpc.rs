use serde::{Deserialize, Serialize};

use crate::error::JsonRpcError;
use crate::server::AppState;

#[derive(Debug, Deserialize, Serialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: Option<String>,
    pub method: String,
    pub params: serde_json::Value,
    pub id: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub result: Option<serde_json::Value>,
    pub error: Option<serde_json::Value>,
    pub id: serde_json::Value,
}

pub async fn dispatch_rpc(
    method: &str,
    _params: &serde_json::Value,
    state: &AppState,
) -> Result<serde_json::Value, JsonRpcError> {
    match method {
        "version" => Ok(serde_json::json!(state.version())),
        "status" => Ok(
            serde_json::to_value(state.status()).map_err(|err| JsonRpcError {
                code: -32000,
                message: err.to_string(),
            })?,
        ),
        _ => Err(JsonRpcError {
            code: -32601,
            message: format!("Method not found: {method}"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dispatch_rpc_returns_version() {
        let state = AppState::default();
        let result = dispatch_rpc("version", &serde_json::json!([]), &state)
            .await
            .expect("version");
        assert_eq!(result, serde_json::json!("0.1.0"));
    }
}
