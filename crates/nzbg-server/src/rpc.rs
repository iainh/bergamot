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
        "listfiles" => rpc_listfiles(params, state).await,
        "postqueue" => Ok(serde_json::json!([])),
        "writelog" => Ok(serde_json::json!(true)),
        "loadlog" => Ok(serde_json::json!([])),
        "servervolumes" => Ok(serde_json::json!([])),
        "config" => Ok(serde_json::json!([])),
        "loadconfig" => Ok(serde_json::json!([])),
        "saveconfig" => Ok(serde_json::json!(true)),
        "configtemplates" => Ok(serde_json::json!([])),
        "history" => rpc_history(params, state).await,
        "rate" => rpc_rate(params, state).await,
        "pausedownload" => rpc_pausedownload(state).await,
        "resumedownload" => rpc_resumedownload(state).await,
        "pausepost" => Ok(serde_json::json!(true)),
        "resumepost" => Ok(serde_json::json!(true)),
        "pausescan" => Ok(serde_json::json!(true)),
        "resumescan" => Ok(serde_json::json!(true)),
        "scan" => Ok(serde_json::json!(true)),
        "feeds" => Ok(serde_json::json!([])),
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

    let nzb_bytes = std::fs::read(path).map_err(|e| rpc_error(format!("reading NZB: {e}")))?;

    let id = queue
        .add_nzb(PathBuf::from(path), category, priority)
        .await
        .map_err(rpc_error)?;

    if let Some(disk) = state.disk() {
        let _ = disk.save_nzb_file(id, &nzb_bytes);
    }

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

    if command.starts_with("History") {
        return rpc_editqueue_history(params, state).await;
    }

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

async fn rpc_editqueue_history(
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
    let ids: Vec<u32> = arr
        .get(2)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_u64().map(|n| n as u32))
                .collect()
        })
        .unwrap_or_default();

    for id in ids {
        match command {
            "HistoryReturn" => {
                queue.history_return(id).await.map_err(rpc_error)?;
            }
            "HistoryRedownload" => {
                queue.history_redownload(id).await.map_err(rpc_error)?;
            }
            "HistoryDelete" | "HistoryFinalDelete" => {
                queue.history_delete(id).await.map_err(rpc_error)?;
            }
            "HistoryMarkGood" => {
                queue
                    .history_mark(id, nzbg_core::models::MarkStatus::Good)
                    .await
                    .map_err(rpc_error)?;
            }
            "HistoryMarkBad" => {
                queue
                    .history_mark(id, nzbg_core::models::MarkStatus::Bad)
                    .await
                    .map_err(rpc_error)?;
            }
            "HistoryMarkSuccess" => {
                queue
                    .history_mark(id, nzbg_core::models::MarkStatus::Success)
                    .await
                    .map_err(rpc_error)?;
            }
            _ => return Err(rpc_error(format!("unknown history command: {command}"))),
        }
    }
    Ok(serde_json::json!(true))
}

async fn rpc_shutdown(state: &AppState) -> Result<serde_json::Value, JsonRpcError> {
    if let Some(shutdown) = state.shutdown_handle() {
        shutdown.trigger();
    }
    Ok(serde_json::json!(true))
}

async fn rpc_listfiles(
    _params: &serde_json::Value,
    state: &AppState,
) -> Result<serde_json::Value, JsonRpcError> {
    let _queue = require_queue(state)?;
    Ok(serde_json::json!([]))
}

async fn rpc_rate(
    params: &serde_json::Value,
    state: &AppState,
) -> Result<serde_json::Value, JsonRpcError> {
    let queue = require_queue(state)?;
    let arr = params
        .as_array()
        .ok_or_else(|| rpc_error("params must be an array"))?;
    let limit_kb = arr.first().and_then(|v| v.as_u64()).unwrap_or(0);
    queue
        .set_download_rate(limit_kb * 1024)
        .await
        .map_err(rpc_error)?;
    Ok(serde_json::json!(true))
}

async fn rpc_history(
    _params: &serde_json::Value,
    state: &AppState,
) -> Result<serde_json::Value, JsonRpcError> {
    let queue = require_queue(state)?;
    let entries = queue.get_history().await.map_err(rpc_error)?;
    let result: Vec<serde_json::Value> = entries
        .into_iter()
        .map(|e| {
            let time_secs = e
                .time
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            serde_json::json!({
                "NZBID": e.id,
                "Name": e.name,
                "Category": e.category,
                "FileSizeLo": (e.size & 0xFFFF_FFFF) as u32,
                "FileSizeHi": (e.size >> 32) as u32,
                "HistoryTime": time_secs,
                "ParStatus": e.par_status as u32,
                "UnpackStatus": e.unpack_status as u32,
                "MoveStatus": e.move_status as u32,
                "DeleteStatus": e.delete_status as u32,
                "MarkStatus": e.mark_status as u32,
                "Health": e.health,
                "Kind": match e.kind {
                    nzbg_core::models::HistoryKind::Nzb => "NZB",
                    nzbg_core::models::HistoryKind::Url => "URL",
                    nzbg_core::models::HistoryKind::DupHidden => "DUP",
                },
                "Status": format_history_status(&e),
            })
        })
        .collect();
    Ok(serde_json::json!(result))
}

fn format_history_status(e: &nzbg_queue::HistoryListEntry) -> String {
    if e.mark_status == nzbg_core::models::MarkStatus::Good {
        return "SUCCESS/GOOD".to_string();
    }
    if e.mark_status == nzbg_core::models::MarkStatus::Bad {
        return "FAILURE/BAD".to_string();
    }
    match e.delete_status {
        nzbg_core::models::DeleteStatus::Manual => return "DELETED/MANUAL".to_string(),
        nzbg_core::models::DeleteStatus::Health => return "DELETED/HEALTH".to_string(),
        nzbg_core::models::DeleteStatus::Dupe => return "DELETED/DUPE".to_string(),
        nzbg_core::models::DeleteStatus::Bad => return "DELETED/BAD".to_string(),
        nzbg_core::models::DeleteStatus::Scan => return "DELETED/SCAN".to_string(),
        nzbg_core::models::DeleteStatus::Copy => return "DELETED/COPY".to_string(),
        _ => {}
    }
    if e.par_status == nzbg_core::models::ParStatus::Failure
        || e.unpack_status == nzbg_core::models::UnpackStatus::Failure
    {
        return "FAILURE".to_string();
    }
    "SUCCESS".to_string()
}

async fn rpc_pausedownload(state: &AppState) -> Result<serde_json::Value, JsonRpcError> {
    let queue = require_queue(state)?;
    queue.pause_all().await.map_err(rpc_error)?;
    Ok(serde_json::json!(true))
}

async fn rpc_resumedownload(state: &AppState) -> Result<serde_json::Value, JsonRpcError> {
    let queue = require_queue(state)?;
    queue.resume_all().await.map_err(rpc_error)?;
    Ok(serde_json::json!(true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nzbg_core::models::Priority;
    use nzbg_queue::QueueCoordinator;
    use std::io::Write;

    const VALID_NZB: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">
  <file poster="user@example.com" date="1706140800"
        subject='Test [01/01] - "data.rar" yEnc (1/1)'>
    <groups><group>alt.binaries.test</group></groups>
    <segments>
      <segment bytes="100" number="1">seg1@example.com</segment>
    </segments>
  </file>
</nzb>"#;

    fn nzb_tempfile() -> tempfile::NamedTempFile {
        let mut f = tempfile::Builder::new()
            .suffix(".nzb")
            .tempfile()
            .expect("tempfile");
        f.write_all(VALID_NZB.as_bytes()).expect("write");
        f.flush().expect("flush");
        f
    }

    fn state_with_queue() -> (
        AppState,
        nzbg_queue::QueueHandle,
        tokio::task::JoinHandle<()>,
    ) {
        let (mut coordinator, handle, _rx, _rate_rx) = QueueCoordinator::new(2, 1);
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
        let _nzb_file = nzb_tempfile();
        let params = serde_json::json!([_nzb_file.path().to_str().unwrap(), "", 0]);
        let result = dispatch_rpc("append", &params, &state)
            .await
            .expect("append");
        assert_eq!(result, serde_json::json!(1));
        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn dispatch_append_saves_nzb_to_disk_state() {
        use nzbg_diskstate::{DiskState, JsonFormat};

        let tmp_disk = tempfile::tempdir().expect("tempdir");
        let disk = std::sync::Arc::new(
            DiskState::new(tmp_disk.path().to_path_buf(), JsonFormat).expect("disk"),
        );

        let (mut coordinator, handle, _rx, _rate_rx) = QueueCoordinator::new(2, 1);
        let coordinator_handle = tokio::spawn(async move { coordinator.run().await });
        let state = AppState::default()
            .with_queue(handle.clone())
            .with_disk(disk.clone());

        let nzb_file = nzb_tempfile();
        let params = serde_json::json!([nzb_file.path().to_str().unwrap(), "", 0]);
        let result = dispatch_rpc("append", &params, &state)
            .await
            .expect("append");
        let id = result.as_u64().expect("id") as u32;

        let saved = disk.load_nzb_file(id).expect("load saved nzb");
        assert_eq!(saved, VALID_NZB.as_bytes());

        handle.shutdown().await.expect("shutdown");
        let _ = coordinator_handle.await;
    }

    #[tokio::test]
    async fn dispatch_append_with_category_and_priority() {
        let (state, handle, _coord) = state_with_queue();
        let _nzb_file = nzb_tempfile();
        let params = serde_json::json!([_nzb_file.path().to_str().unwrap(), "movies", 50]);
        let result = dispatch_rpc("append", &params, &state)
            .await
            .expect("append");
        assert_eq!(result, serde_json::json!(1));

        let list = handle.get_nzb_list().await.expect("list");
        assert_eq!(list.len(), 1);
        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn dispatch_listgroups_returns_nzb_entries() {
        let (state, handle, _coord) = state_with_queue();
        let _nzb_file = nzb_tempfile();
        handle
            .add_nzb(
                _nzb_file.path().to_path_buf(),
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
        assert!(groups[0]["NZBName"].as_str().is_some());
        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn dispatch_editqueue_pauses_nzb() {
        let (state, handle, _coord) = state_with_queue();
        let _nzb_file = nzb_tempfile();
        let id = handle
            .add_nzb(
                _nzb_file.path().to_path_buf(),
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
        let (mut coordinator, handle, _rx, _rate_rx) = QueueCoordinator::new(2, 1);
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

    #[tokio::test]
    async fn dispatch_listfiles_returns_empty_array() {
        let (state, handle, _coord) = state_with_queue();
        let result = dispatch_rpc("listfiles", &serde_json::json!([1]), &state)
            .await
            .expect("listfiles");
        assert_eq!(result, serde_json::json!([]));
        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn dispatch_postqueue_returns_empty_array() {
        let state = AppState::default();
        let result = dispatch_rpc("postqueue", &serde_json::json!([]), &state)
            .await
            .expect("postqueue");
        assert_eq!(result, serde_json::json!([]));
    }

    #[tokio::test]
    async fn dispatch_writelog_returns_true() {
        let state = AppState::default();
        let result = dispatch_rpc(
            "writelog",
            &serde_json::json!(["info", "test message"]),
            &state,
        )
        .await
        .expect("writelog");
        assert_eq!(result, serde_json::json!(true));
    }

    #[tokio::test]
    async fn dispatch_loadlog_returns_empty_array() {
        let state = AppState::default();
        let result = dispatch_rpc("loadlog", &serde_json::json!([1, 0, 100]), &state)
            .await
            .expect("loadlog");
        assert_eq!(result, serde_json::json!([]));
    }

    #[tokio::test]
    async fn dispatch_servervolumes_returns_empty_array() {
        let state = AppState::default();
        let result = dispatch_rpc("servervolumes", &serde_json::json!([]), &state)
            .await
            .expect("servervolumes");
        assert_eq!(result, serde_json::json!([]));
    }

    #[tokio::test]
    async fn dispatch_config_returns_empty_array() {
        let state = AppState::default();
        let result = dispatch_rpc("config", &serde_json::json!([]), &state)
            .await
            .expect("config");
        assert_eq!(result, serde_json::json!([]));
    }

    #[tokio::test]
    async fn dispatch_loadconfig_returns_empty_array() {
        let state = AppState::default();
        let result = dispatch_rpc("loadconfig", &serde_json::json!([]), &state)
            .await
            .expect("loadconfig");
        assert_eq!(result, serde_json::json!([]));
    }

    #[tokio::test]
    async fn dispatch_saveconfig_returns_true() {
        let state = AppState::default();
        let result = dispatch_rpc("saveconfig", &serde_json::json!([]), &state)
            .await
            .expect("saveconfig");
        assert_eq!(result, serde_json::json!(true));
    }

    #[tokio::test]
    async fn dispatch_configtemplates_returns_empty_array() {
        let state = AppState::default();
        let result = dispatch_rpc("configtemplates", &serde_json::json!([]), &state)
            .await
            .expect("configtemplates");
        assert_eq!(result, serde_json::json!([]));
    }

    #[tokio::test]
    async fn dispatch_history_returns_empty_array() {
        let (state, handle, _coord) = state_with_queue();
        let result = dispatch_rpc("history", &serde_json::json!([false]), &state)
            .await
            .expect("history");
        assert_eq!(result, serde_json::json!([]));
        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn dispatch_rate_sets_download_rate() {
        let (state, handle, _coord) = state_with_queue();
        let result = dispatch_rpc("rate", &serde_json::json!([500]), &state)
            .await
            .expect("rate");
        assert_eq!(result, serde_json::json!(true));
        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn dispatch_pausedownload_returns_true() {
        let (state, handle, _coord) = state_with_queue();
        let result = dispatch_rpc("pausedownload", &serde_json::json!([]), &state)
            .await
            .expect("pausedownload");
        assert_eq!(result, serde_json::json!(true));
        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn dispatch_resumedownload_returns_true() {
        let (state, handle, _coord) = state_with_queue();
        let result = dispatch_rpc("resumedownload", &serde_json::json!([]), &state)
            .await
            .expect("resumedownload");
        assert_eq!(result, serde_json::json!(true));
        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn dispatch_pausepost_returns_true() {
        let state = AppState::default();
        let result = dispatch_rpc("pausepost", &serde_json::json!([]), &state)
            .await
            .expect("pausepost");
        assert_eq!(result, serde_json::json!(true));
    }

    #[tokio::test]
    async fn dispatch_resumepost_returns_true() {
        let state = AppState::default();
        let result = dispatch_rpc("resumepost", &serde_json::json!([]), &state)
            .await
            .expect("resumepost");
        assert_eq!(result, serde_json::json!(true));
    }

    #[tokio::test]
    async fn dispatch_pausescan_returns_true() {
        let state = AppState::default();
        let result = dispatch_rpc("pausescan", &serde_json::json!([]), &state)
            .await
            .expect("pausescan");
        assert_eq!(result, serde_json::json!(true));
    }

    #[tokio::test]
    async fn dispatch_resumescan_returns_true() {
        let state = AppState::default();
        let result = dispatch_rpc("resumescan", &serde_json::json!([]), &state)
            .await
            .expect("resumescan");
        assert_eq!(result, serde_json::json!(true));
    }

    #[tokio::test]
    async fn dispatch_scan_returns_true() {
        let state = AppState::default();
        let result = dispatch_rpc("scan", &serde_json::json!([]), &state)
            .await
            .expect("scan");
        assert_eq!(result, serde_json::json!(true));
    }

    #[tokio::test]
    async fn dispatch_feeds_returns_empty_array() {
        let state = AppState::default();
        let result = dispatch_rpc("feeds", &serde_json::json!([]), &state)
            .await
            .expect("feeds");
        assert_eq!(result, serde_json::json!([]));
    }

    fn state_with_history() -> (
        AppState,
        nzbg_queue::QueueHandle,
        tokio::task::JoinHandle<()>,
    ) {
        let (mut coordinator, handle, _rx, _rate_rx) = QueueCoordinator::new(2, 1);
        coordinator.add_to_history(
            nzbg_core::models::NzbInfo {
                id: 1,
                kind: nzbg_core::models::NzbKind::Nzb,
                name: "completed.nzb".to_string(),
                filename: "completed.nzb".to_string(),
                url: String::new(),
                dest_dir: std::path::PathBuf::new(),
                final_dir: std::path::PathBuf::new(),
                temp_dir: std::path::PathBuf::new(),
                queue_dir: std::path::PathBuf::new(),
                category: "tv".to_string(),
                priority: Priority::Normal,
                dup_key: String::new(),
                dup_mode: nzbg_core::models::DupMode::Score,
                dup_score: 0,
                size: 1000,
                remaining_size: 0,
                paused_size: 0,
                failed_size: 0,
                success_size: 1000,
                current_downloaded_size: 1000,
                par_size: 0,
                par_remaining_size: 0,
                par_current_success_size: 0,
                par_failed_size: 0,
                file_count: 1,
                remaining_file_count: 0,
                remaining_par_count: 0,
                total_article_count: 10,
                success_article_count: 10,
                failed_article_count: 0,
                added_time: std::time::SystemTime::UNIX_EPOCH,
                min_time: None,
                max_time: None,
                download_start_time: None,
                download_sec: 0,
                post_total_sec: 0,
                par_sec: 0,
                repair_sec: 0,
                unpack_sec: 0,
                paused: false,
                deleted: false,
                direct_rename: false,
                force_priority: false,
                reprocess: false,
                par_manual: false,
                clean_up_disk: false,
                par_status: nzbg_core::models::ParStatus::Success,
                unpack_status: nzbg_core::models::UnpackStatus::Success,
                move_status: nzbg_core::models::MoveStatus::Success,
                delete_status: nzbg_core::models::DeleteStatus::None,
                mark_status: nzbg_core::models::MarkStatus::None,
                url_status: nzbg_core::models::UrlStatus::None,
                health: 1000,
                critical_health: 1000,
                files: vec![],
                completed_files: vec![],
                server_stats: vec![],
                parameters: vec![],
                post_info: None,
                message_count: 0,
                cached_message_count: 0,
            },
            nzbg_core::models::HistoryKind::Nzb,
        );
        let coordinator_handle = tokio::spawn(async move { coordinator.run().await });
        let state = AppState::default().with_queue(handle.clone());
        (state, handle, coordinator_handle)
    }

    #[tokio::test]
    async fn dispatch_history_returns_entries() {
        let (state, handle, _coord) = state_with_history();
        let result = dispatch_rpc("history", &serde_json::json!([false]), &state)
            .await
            .expect("history");
        let entries = result.as_array().expect("array");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["NZBID"], 1);
        assert_eq!(entries[0]["Name"], "completed.nzb");
        assert_eq!(entries[0]["Category"], "tv");
        assert_eq!(entries[0]["Kind"], "NZB");
        assert_eq!(entries[0]["Status"], "SUCCESS");
        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn dispatch_editqueue_history_return() {
        let (state, handle, _coord) = state_with_history();
        let params = serde_json::json!(["HistoryReturn", "", [1]]);
        let result = dispatch_rpc("editqueue", &params, &state)
            .await
            .expect("editqueue");
        assert_eq!(result, serde_json::json!(true));

        let history = dispatch_rpc("history", &serde_json::json!([false]), &state)
            .await
            .expect("history");
        assert_eq!(history.as_array().expect("array").len(), 0);

        let groups = dispatch_rpc("listgroups", &serde_json::json!([]), &state)
            .await
            .expect("listgroups");
        assert_eq!(groups.as_array().expect("array").len(), 1);

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn dispatch_editqueue_history_mark_bad() {
        let (state, handle, _coord) = state_with_history();
        let params = serde_json::json!(["HistoryMarkBad", "", [1]]);
        let result = dispatch_rpc("editqueue", &params, &state)
            .await
            .expect("editqueue");
        assert_eq!(result, serde_json::json!(true));

        let history = dispatch_rpc("history", &serde_json::json!([false]), &state)
            .await
            .expect("history");
        let entries = history.as_array().expect("array");
        assert_eq!(entries[0]["Status"], "FAILURE/BAD");

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn dispatch_editqueue_history_delete() {
        let (state, handle, _coord) = state_with_history();
        let params = serde_json::json!(["HistoryDelete", "", [1]]);
        let result = dispatch_rpc("editqueue", &params, &state)
            .await
            .expect("editqueue");
        assert_eq!(result, serde_json::json!(true));

        let history = dispatch_rpc("history", &serde_json::json!([false]), &state)
            .await
            .expect("history");
        assert_eq!(history.as_array().expect("array").len(), 0);

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn dispatch_editqueue_history_redownload() {
        let (state, handle, _coord) = state_with_history();
        let params = serde_json::json!(["HistoryRedownload", "", [1]]);
        let result = dispatch_rpc("editqueue", &params, &state)
            .await
            .expect("editqueue");
        assert_eq!(result, serde_json::json!(true));

        let history = dispatch_rpc("history", &serde_json::json!([false]), &state)
            .await
            .expect("history");
        assert_eq!(history.as_array().expect("array").len(), 0);

        let groups = dispatch_rpc("listgroups", &serde_json::json!([]), &state)
            .await
            .expect("listgroups");
        assert_eq!(groups.as_array().expect("array").len(), 1);

        handle.shutdown().await.expect("shutdown");
    }
}
