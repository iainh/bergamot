use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use nzbg_core::models::Priority;

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
    params: &serde_json::Value,
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
        "append" => rpc_append(params, state).await,
        "listgroups" => rpc_listgroups(state).await,
        "editqueue" => rpc_editqueue(params, state).await,
        "shutdown" => rpc_shutdown(state).await,
        _ => Err(JsonRpcError {
            code: -32601,
            message: format!("Method not found: {method}"),
        }),
    }
}

fn require_queue(state: &AppState) -> Result<&nzbg_queue::QueueHandle, JsonRpcError> {
    state.queue_handle().ok_or_else(|| JsonRpcError {
        code: -32000,
        message: "Queue not available".to_string(),
    })
}

fn rpc_error(msg: impl std::fmt::Display) -> JsonRpcError {
    JsonRpcError {
        code: -32000,
        message: msg.to_string(),
    }
}

fn priority_from_i32(val: i32) -> Priority {
    match val {
        -100 => Priority::VeryLow,
        -50 => Priority::Low,
        50 => Priority::High,
        100 => Priority::VeryHigh,
        900 => Priority::Force,
        _ => Priority::Normal,
    }
}

async fn rpc_append(
    params: &serde_json::Value,
    state: &AppState,
) -> Result<serde_json::Value, JsonRpcError> {
    let queue = require_queue(state)?;
    let arr = params
        .as_array()
        .ok_or_else(|| rpc_error("params must be an array"))?;

    let path = arr
        .first()
        .and_then(|v| v.as_str())
        .ok_or_else(|| rpc_error("missing NZB path"))?;

    let category = arr.get(1).and_then(|v| v.as_str()).unwrap_or("");
    let category = if category.is_empty() {
        None
    } else {
        Some(category.to_string())
    };

    let priority_val = arr.get(2).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
    let priority = priority_from_i32(priority_val);

    let id = queue
        .add_nzb(PathBuf::from(path), category, priority)
        .await
        .map_err(rpc_error)?;

    Ok(serde_json::json!(id))
}

async fn rpc_listgroups(state: &AppState) -> Result<serde_json::Value, JsonRpcError> {
    let queue = require_queue(state)?;
    let list = queue.get_nzb_list().await.map_err(rpc_error)?;

    let entries: Vec<serde_json::Value> = list
        .into_iter()
        .map(|entry| {
            serde_json::json!({
                "NZBID": entry.id,
                "NZBName": entry.name,
                "Priority": entry.priority as i32,
            })
        })
        .collect();

    Ok(serde_json::json!(entries))
}

async fn rpc_editqueue(
    params: &serde_json::Value,
    state: &AppState,
) -> Result<serde_json::Value, JsonRpcError> {
    let queue = require_queue(state)?;
    let arr = params
        .as_array()
        .ok_or_else(|| rpc_error("params must be an array"))?;

    let command = arr
        .first()
        .and_then(|v| v.as_str())
        .ok_or_else(|| rpc_error("missing command"))?;

    let _param = arr.get(1).and_then(|v| v.as_str()).unwrap_or("");

    let ids: Vec<u32> = arr
        .get(2)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_u64().map(|n| n as u32))
                .collect()
        })
        .unwrap_or_default();

    let action = parse_edit_command(command)?;

    queue.edit_queue(action, ids).await.map_err(rpc_error)?;

    Ok(serde_json::json!(true))
}

fn parse_edit_command(command: &str) -> Result<nzbg_queue::EditAction, JsonRpcError> {
    match command {
        "GroupMoveTop" => Ok(nzbg_queue::EditAction::Move(nzbg_queue::MovePosition::Top)),
        "GroupMoveBottom" => Ok(nzbg_queue::EditAction::Move(
            nzbg_queue::MovePosition::Bottom,
        )),
        "GroupPause" => Ok(nzbg_queue::EditAction::Pause),
        "GroupResume" => Ok(nzbg_queue::EditAction::Resume),
        "GroupDelete" | "GroupFinalDelete" => Ok(nzbg_queue::EditAction::Delete {
            delete_files: command == "GroupFinalDelete",
        }),
        "GroupPauseAllPars" | "GroupPauseExtraPars" => Ok(nzbg_queue::EditAction::Pause),
        _ => Err(rpc_error(format!("unknown editqueue command: {command}"))),
    }
}

async fn rpc_shutdown(state: &AppState) -> Result<serde_json::Value, JsonRpcError> {
    if let Some(shutdown) = state.shutdown_handle() {
        shutdown.trigger();
    }
    Ok(serde_json::json!(true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nzbg_core::models::Priority;
    use nzbg_queue::QueueCoordinator;

    fn state_with_queue() -> (
        AppState,
        nzbg_queue::QueueHandle,
        tokio::task::JoinHandle<()>,
    ) {
        let (mut coordinator, handle, _rx) = QueueCoordinator::new(2, 1);
        let coordinator_handle = tokio::spawn(async move { coordinator.run().await });
        let state = AppState::default().with_queue(handle.clone());
        (state, handle, coordinator_handle)
    }

    #[tokio::test]
    async fn dispatch_rpc_returns_version() {
        let state = AppState::default();
        let result = dispatch_rpc("version", &serde_json::json!([]), &state)
            .await
            .expect("version");
        assert_eq!(result, serde_json::json!("0.1.0"));
    }

    #[tokio::test]
    async fn dispatch_append_adds_nzb_and_returns_id() {
        let (state, handle, _coord) = state_with_queue();
        let params = serde_json::json!(["/tmp/test.nzb", "", 0]);
        let result = dispatch_rpc("append", &params, &state)
            .await
            .expect("append");
        assert_eq!(result, serde_json::json!(1));
        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn dispatch_append_with_category_and_priority() {
        let (state, handle, _coord) = state_with_queue();
        let params = serde_json::json!(["/tmp/test.nzb", "movies", 50]);
        let result = dispatch_rpc("append", &params, &state)
            .await
            .expect("append");
        assert_eq!(result, serde_json::json!(1));

        let list = handle.get_nzb_list().await.expect("list");
        assert_eq!(list[0].name, "test.nzb");
        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn dispatch_listgroups_returns_nzb_entries() {
        let (state, handle, _coord) = state_with_queue();
        handle
            .add_nzb(
                std::path::PathBuf::from("/tmp/first.nzb"),
                None,
                Priority::Normal,
            )
            .await
            .expect("add");

        let result = dispatch_rpc("listgroups", &serde_json::json!([]), &state)
            .await
            .expect("listgroups");
        let groups = result.as_array().expect("array");
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0]["NZBID"], 1);
        assert_eq!(groups[0]["NZBName"], "first.nzb");
        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn dispatch_editqueue_pauses_nzb() {
        let (state, handle, _coord) = state_with_queue();
        let id = handle
            .add_nzb(
                std::path::PathBuf::from("/tmp/test.nzb"),
                None,
                Priority::Normal,
            )
            .await
            .expect("add");

        let params = serde_json::json!(["GroupPause", "", [id]]);
        let result = dispatch_rpc("editqueue", &params, &state)
            .await
            .expect("editqueue");
        assert_eq!(result, serde_json::json!(true));
        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn dispatch_shutdown_triggers_shutdown() {
        let (shutdown_handle, rx) = crate::shutdown::ShutdownHandle::new();
        let (mut coordinator, handle, _rx) = QueueCoordinator::new(2, 1);
        tokio::spawn(async move { coordinator.run().await });
        let state = AppState::default()
            .with_queue(handle.clone())
            .with_shutdown(shutdown_handle);

        let result = dispatch_rpc("shutdown", &serde_json::json!([]), &state)
            .await
            .expect("shutdown");
        assert_eq!(result, serde_json::json!(true));
        assert!(*rx.borrow());
    }

    #[tokio::test]
    async fn dispatch_rpc_without_queue_returns_error() {
        let state = AppState::default();
        let result = dispatch_rpc("listgroups", &serde_json::json!([]), &state).await;
        assert!(result.is_err());
    }
}
