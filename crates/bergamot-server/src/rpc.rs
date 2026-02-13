use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use bergamot_core::models::Priority;

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

pub(crate) fn require_queue(
    state: &AppState,
) -> Result<&bergamot_queue::QueueHandle, JsonRpcError> {
    state.queue_handle().ok_or_else(|| JsonRpcError {
        code: -32000,
        message: "Queue not available".to_string(),
    })
}

pub(crate) fn rpc_error(msg: impl std::fmt::Display) -> JsonRpcError {
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

fn is_url(input: &str) -> bool {
    input.starts_with("http://") || input.starts_with("https://")
}

async fn fetch_nzb_url(url: &str) -> Result<(String, Vec<u8>), JsonRpcError> {
    let resp = reqwest::get(url)
        .await
        .map_err(|e| rpc_error(format!("fetching URL: {e}")))?;
    if !resp.status().is_success() {
        return Err(rpc_error(format!("HTTP {} for {url}", resp.status())));
    }
    let filename = url.rsplit('/').next().unwrap_or("download.nzb").to_string();
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| rpc_error(format!("reading response: {e}")))?;
    Ok((filename, bytes.to_vec()))
}

pub(crate) async fn rpc_append(
    params: &serde_json::Value,
    state: &AppState,
) -> Result<serde_json::Value, JsonRpcError> {
    use base64::Engine;

    let queue = require_queue(state)?;
    let arr = params
        .as_array()
        .ok_or_else(|| rpc_error("params must be an array"))?;

    let nzb_filename = arr.first().and_then(|v| v.as_str()).unwrap_or("");

    let content = arr.get(1).and_then(|v| v.as_str()).unwrap_or("");

    let category = arr.get(2).and_then(|v| v.as_str()).unwrap_or("");
    let category = if category.is_empty() {
        None
    } else {
        Some(category.to_string())
    };

    let priority_val = arr.get(3).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
    let priority = priority_from_i32(priority_val);

    let add_to_top = arr.get(4).and_then(|v| v.as_bool()).unwrap_or(false);
    let add_paused = arr.get(5).and_then(|v| v.as_bool()).unwrap_or(false);
    let dup_key = arr.get(6).and_then(|v| v.as_str()).map(|s| s.to_string());
    let dup_score = arr.get(7).and_then(|v| v.as_i64()).map(|n| n as i32);
    let dup_mode_str = arr.get(8).and_then(|v| v.as_str()).unwrap_or("");
    let dup_mode = match dup_mode_str {
        "ALL" => Some(bergamot_core::models::DupMode::All),
        "FORCE" => Some(bergamot_core::models::DupMode::Force),
        "SCORE" => Some(bergamot_core::models::DupMode::Score),
        _ => None,
    };

    let mut parameters = Vec::new();
    for param in arr.iter().skip(9) {
        if let Some(s) = param.as_str()
            && let Some((key, value)) = s.split_once('=')
        {
            parameters.push((key.to_string(), value.to_string()));
        }
    }

    let options = bergamot_queue::AddNzbOptions {
        add_to_top,
        add_paused,
        dup_key: dup_key.filter(|s| !s.is_empty()),
        dup_score,
        dup_mode,
        parameters,
    };

    let temp_dir = std::env::temp_dir().join("bergamot-downloads");
    std::fs::create_dir_all(&temp_dir).map_err(|e| rpc_error(format!("creating temp dir: {e}")))?;

    let (nzb_path, nzb_bytes) = if is_url(content) {
        let (filename, bytes) = fetch_nzb_url(content).await?;
        let name = if nzb_filename.is_empty() {
            &filename
        } else {
            nzb_filename
        };
        let path = temp_dir.join(name);
        std::fs::write(&path, &bytes).map_err(|e| rpc_error(format!("writing temp NZB: {e}")))?;
        (path, bytes)
    } else if !content.is_empty() {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(content)
            .map_err(|e| rpc_error(format!("decoding base64 NZB content: {e}")))?;
        let name = if nzb_filename.is_empty() {
            "download.nzb"
        } else {
            nzb_filename
        };
        let path = temp_dir.join(name);
        std::fs::write(&path, &bytes).map_err(|e| rpc_error(format!("writing temp NZB: {e}")))?;
        (path, bytes)
    } else if !nzb_filename.is_empty() {
        let bytes =
            std::fs::read(nzb_filename).map_err(|e| rpc_error(format!("reading NZB: {e}")))?;
        (PathBuf::from(nzb_filename), bytes)
    } else {
        return Err(rpc_error("missing NZB content or filename"));
    };

    let id = queue
        .add_nzb_with_options(nzb_path, category, priority, options)
        .await
        .map_err(rpc_error)?;

    if let Some(disk) = state.disk() {
        let _ = disk.save_nzb_file(id, &nzb_bytes);
    }

    Ok(serde_json::json!(id))
}

pub(crate) async fn rpc_listgroups(state: &AppState) -> Result<serde_json::Value, JsonRpcError> {
    let queue = require_queue(state)?;
    let snapshot = queue.get_queue_snapshot().await.map_err(rpc_error)?;

    let entries: Vec<serde_json::Value> = snapshot
        .nzbs
        .into_iter()
        .map(|entry| {
            let remaining_size = entry.total_size.saturating_sub(entry.downloaded_size);
            let dupe_mode_str = match entry.dupe_mode {
                bergamot_core::models::DupMode::Score => "SCORE",
                bergamot_core::models::DupMode::All => "ALL",
                bergamot_core::models::DupMode::Force => "FORCE",
            };

            let status = if let Some(stage) = entry.post_stage {
                match stage {
                    bergamot_core::models::PostStage::Queued => "PP_QUEUED",
                    bergamot_core::models::PostStage::ParLoading => "LOADING_PARS",
                    bergamot_core::models::PostStage::ParRenaming => "RENAMING",
                    bergamot_core::models::PostStage::ParVerifying => "VERIFYING_SOURCES",
                    bergamot_core::models::PostStage::ParRepairing => "REPAIRING",
                    bergamot_core::models::PostStage::Unpacking => "UNPACKING",
                    bergamot_core::models::PostStage::Moving => "MOVING",
                    bergamot_core::models::PostStage::Executing => "EXECUTING_SCRIPT",
                    bergamot_core::models::PostStage::Finished => "PP_FINISHED",
                }
            } else if entry.paused {
                "PAUSED"
            } else if entry.active_downloads > 0 {
                "DOWNLOADING"
            } else {
                "QUEUED"
            };

            let server_stats: Vec<serde_json::Value> = Vec::new();
            let parameters: Vec<serde_json::Value> = entry
                .parameters
                .iter()
                .map(|(name, value)| {
                    serde_json::json!({
                        "Name": name,
                        "Value": value,
                    })
                })
                .collect();

            serde_json::json!({
                "NZBID": entry.id,
                "FirstID": entry.id,
                "LastID": entry.id,
                "NZBName": entry.name,
                "NZBNicename": entry.name,
                "NZBFilename": "",
                "URL": "",
                "DestDir": "",
                "FinalDir": "",
                "Status": status,
                "Category": entry.category,
                "FileSizeLo": (entry.total_size & 0xFFFF_FFFF) as u32,
                "FileSizeHi": (entry.total_size >> 32) as u32,
                "FileSizeMB": entry.total_size / (1024 * 1024),
                "RemainingSizeLo": (remaining_size & 0xFFFF_FFFF) as u32,
                "RemainingSizeHi": (remaining_size >> 32) as u32,
                "RemainingSizeMB": remaining_size / (1024 * 1024),
                "PausedSizeLo": 0,
                "PausedSizeHi": 0,
                "PausedSizeMB": 0,
                "DownloadedSizeLo": (entry.downloaded_size & 0xFFFF_FFFF) as u32,
                "DownloadedSizeHi": (entry.downloaded_size >> 32) as u32,
                "DownloadedSizeMB": entry.downloaded_size / (1024 * 1024),
                "DownloadTimeSec": 0,
                "MaxPriority": entry.priority as i32,
                "MinPriority": entry.priority as i32,
                "DupeKey": entry.dupe_key,
                "DupeScore": entry.dupe_score,
                "DupeMode": dupe_mode_str,
                "MinPostTime": 0,
                "MaxPostTime": 0,
                "ActiveDownloads": entry.active_downloads,
                "Health": entry.health,
                "CriticalHealth": entry.critical_health,
                "Kind": "NZB",
                "FileCount": entry.file_count,
                "RemainingFileCount": entry.remaining_file_count,
                "RemainingParCount": entry.remaining_par_count,
                "TotalArticles": entry.total_article_count,
                "SuccessArticles": entry.success_article_count,
                "FailedArticles": entry.failed_article_count,
                "Deleted": false,
                "DeleteStatus": "NONE",
                "MarkStatus": "NONE",
                "UrlStatus": "NONE",
                "ParStatus": "NONE",
                "UnpackStatus": "NONE",
                "MoveStatus": "NONE",
                "ScriptStatus": "NONE",
                "ExParStatus": "NONE",
                "ExtraParBlocks": 0,
                "MessageCount": 0,
                "PostStageProgress": entry.post_stage_progress,
                "PostStageTimeSec": entry.post_stage_time_sec,
                "PostTotalTimeSec": entry.post_total_time_sec,
                "ParTimeSec": 0,
                "RepairTimeSec": 0,
                "UnpackTimeSec": 0,
                "PostInfoText": entry.post_info_text,
                "ServerStats": server_stats,
                "ScriptStatuses": [],
                "Parameters": parameters,
                "Log": [],
            })
        })
        .collect();

    Ok(serde_json::json!(entries))
}

pub(crate) async fn rpc_editqueue(
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

    let param = arr.get(1).and_then(|v| v.as_str()).unwrap_or("");

    let ids: Vec<u32> = arr
        .get(2)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_u64().map(|n| n as u32))
                .collect()
        })
        .unwrap_or_default();

    let action = parse_edit_command(command, param)?;

    queue.edit_queue(action, ids).await.map_err(rpc_error)?;

    Ok(serde_json::json!(true))
}

fn parse_edit_command(
    command: &str,
    param: &str,
) -> Result<bergamot_queue::EditAction, JsonRpcError> {
    match command {
        "GroupMoveTop" => Ok(bergamot_queue::EditAction::Move(
            bergamot_queue::MovePosition::Top,
        )),
        "GroupMoveBottom" => Ok(bergamot_queue::EditAction::Move(
            bergamot_queue::MovePosition::Bottom,
        )),
        "GroupMoveOffset" => {
            let offset: i32 = param.parse().unwrap_or(0);
            if offset > 0 {
                Ok(bergamot_queue::EditAction::Move(
                    bergamot_queue::MovePosition::Down(offset as u32),
                ))
            } else {
                Ok(bergamot_queue::EditAction::Move(
                    bergamot_queue::MovePosition::Up(offset.unsigned_abs()),
                ))
            }
        }
        "GroupPause" => Ok(bergamot_queue::EditAction::Pause),
        "GroupResume" => Ok(bergamot_queue::EditAction::Resume),
        "GroupDelete" | "GroupDupeDelete" | "GroupFinalDelete" => {
            Ok(bergamot_queue::EditAction::Delete { delete_files: true })
        }
        "GroupParkDelete" => Ok(bergamot_queue::EditAction::Delete {
            delete_files: false,
        }),
        "GroupPauseAllPars" | "GroupPauseExtraPars" => Ok(bergamot_queue::EditAction::Pause),
        "GroupSetPriority" => {
            let val: i32 = param.parse().unwrap_or(0);
            Ok(bergamot_queue::EditAction::SetPriority(priority_from_i32(
                val,
            )))
        }
        "GroupSetCategory" => Ok(bergamot_queue::EditAction::SetCategory(param.to_string())),
        "GroupSetName" => Ok(bergamot_queue::EditAction::SetName(param.to_string())),
        "GroupSetDupeKey" => Ok(bergamot_queue::EditAction::SetDupeKey(param.to_string())),
        "GroupSetDupeScore" => {
            let score: i32 = param.parse().unwrap_or(0);
            Ok(bergamot_queue::EditAction::SetDupeScore(score))
        }
        "GroupSetDupeMode" => {
            let mode = match param {
                "ALL" => bergamot_core::models::DupMode::All,
                "FORCE" => bergamot_core::models::DupMode::Force,
                _ => bergamot_core::models::DupMode::Score,
            };
            Ok(bergamot_queue::EditAction::SetDupeMode(mode))
        }
        "GroupSetParameter" => {
            let (key, value) = param.split_once('=').unwrap_or((param, ""));
            Ok(bergamot_queue::EditAction::SetParameter {
                key: key.to_string(),
                value: value.to_string(),
            })
        }
        "GroupMerge" => {
            let target_id: u32 = param.parse().unwrap_or(0);
            Ok(bergamot_queue::EditAction::Merge { target_id })
        }
        "GroupSplit" => {
            let indices: Vec<u32> = param
                .split(',')
                .filter_map(|s| s.trim().parse().ok())
                .collect();
            Ok(bergamot_queue::EditAction::Split {
                file_indices: indices,
            })
        }
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
                    .history_mark(id, bergamot_core::models::MarkStatus::Good)
                    .await
                    .map_err(rpc_error)?;
            }
            "HistoryMarkBad" => {
                queue
                    .history_mark(id, bergamot_core::models::MarkStatus::Bad)
                    .await
                    .map_err(rpc_error)?;
            }
            "HistoryMarkSuccess" => {
                queue
                    .history_mark(id, bergamot_core::models::MarkStatus::Success)
                    .await
                    .map_err(rpc_error)?;
            }
            _ => return Err(rpc_error(format!("unknown history command: {command}"))),
        }
    }
    Ok(serde_json::json!(true))
}

pub(crate) async fn rpc_shutdown(state: &AppState) -> Result<serde_json::Value, JsonRpcError> {
    if let Some(shutdown) = state.shutdown_handle() {
        shutdown.trigger();
    }
    Ok(serde_json::json!(true))
}

pub(crate) async fn rpc_listfiles(
    params: &serde_json::Value,
    state: &AppState,
) -> Result<serde_json::Value, JsonRpcError> {
    let queue = require_queue(state)?;
    let arr = params.as_array();
    let nzb_id = arr
        .and_then(|a| a.first())
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;

    let files = queue.get_file_list(nzb_id).await.map_err(rpc_error)?;
    let entries: Vec<serde_json::Value> = files
        .into_iter()
        .map(|f| {
            serde_json::json!({
                "ID": f.id,
                "NZBID": f.nzb_id,
                "Filename": f.filename,
                "Subject": f.subject,
                "FileSizeLo": (f.size & 0xFFFF_FFFF) as u32,
                "FileSizeHi": (f.size >> 32) as u32,
                "RemainingSizeLo": (f.remaining_size & 0xFFFF_FFFF) as u32,
                "RemainingSizeHi": (f.remaining_size >> 32) as u32,
                "Paused": f.paused,
                "TotalArticles": f.total_articles,
                "SuccessArticles": f.success_articles,
                "FailedArticles": f.failed_articles,
                "ActiveDownloads": f.active_downloads,
                "PostTime": 0,
            })
        })
        .collect();
    Ok(serde_json::json!(entries))
}

pub(crate) fn rpc_postqueue(_state: &AppState) -> Result<serde_json::Value, JsonRpcError> {
    Ok(serde_json::json!([]))
}

pub(crate) fn rpc_pausepost(state: &AppState) -> Result<serde_json::Value, JsonRpcError> {
    state
        .postproc_paused()
        .store(true, std::sync::atomic::Ordering::Relaxed);
    Ok(serde_json::json!(true))
}

pub(crate) fn rpc_resumepost(state: &AppState) -> Result<serde_json::Value, JsonRpcError> {
    state
        .postproc_paused()
        .store(false, std::sync::atomic::Ordering::Relaxed);
    Ok(serde_json::json!(true))
}

pub(crate) fn rpc_pausescan(state: &AppState) -> Result<serde_json::Value, JsonRpcError> {
    state
        .scan_paused()
        .store(true, std::sync::atomic::Ordering::Relaxed);
    Ok(serde_json::json!(true))
}

pub(crate) fn rpc_resumescan(state: &AppState) -> Result<serde_json::Value, JsonRpcError> {
    state
        .scan_paused()
        .store(false, std::sync::atomic::Ordering::Relaxed);
    Ok(serde_json::json!(true))
}

pub(crate) async fn rpc_scan(state: &AppState) -> Result<serde_json::Value, JsonRpcError> {
    if let Some(tx) = state.scan_trigger() {
        let _ = tx.send(()).await;
    }
    Ok(serde_json::json!(true))
}

pub(crate) async fn rpc_feeds(state: &AppState) -> Result<serde_json::Value, JsonRpcError> {
    let Some(feed_handle) = state.feed_handle() else {
        return Ok(serde_json::json!([]));
    };
    let infos = feed_handle.get_infos().await.map_err(rpc_error)?;
    let entries: Vec<serde_json::Value> = infos
        .into_iter()
        .map(|info| {
            serde_json::json!({
                "ID": info.id,
                "Name": info.name,
                "URL": info.url,
                "Status": format!("{:?}", info.status),
                "LastUpdate": info.last_update.map(|t| t.to_rfc3339()),
                "NextUpdate": info.next_update.map(|t| t.to_rfc3339()),
                "ItemCount": info.item_count,
                "Error": info.error,
            })
        })
        .collect();
    Ok(serde_json::json!(entries))
}

pub(crate) fn rpc_sysinfo(state: &AppState) -> Result<serde_json::Value, JsonRpcError> {
    let uptime_sec = state.start_time().elapsed().as_secs();
    Ok(serde_json::json!({
        "Version": state.version(),
        "UptimeSec": uptime_sec,
        "OS": {
            "Name": std::env::consts::OS,
            "Version": "",
        },
        "CPU": {
            "Model": "",
            "Arch": std::env::consts::ARCH,
        },
        "Network": {
            "PrivateIP": "",
            "PublicIP": "",
        },
        "Tools": [],
        "Libraries": [],
    }))
}

pub(crate) fn rpc_systemhealth(state: &AppState) -> Result<serde_json::Value, JsonRpcError> {
    let queue_available = state.queue_handle().is_some();
    Ok(serde_json::json!({
        "Healthy": queue_available,
        "QueueAvailable": queue_available,
        "Alerts": [],
        "Sections": [],
    }))
}

pub(crate) fn rpc_reload(state: &AppState) -> Result<serde_json::Value, JsonRpcError> {
    let config_arc = state
        .config()
        .ok_or_else(|| rpc_error("Config not available"))?;
    let config_path = state
        .config_path()
        .ok_or_else(|| rpc_error("Config path not available"))?;

    let content = std::fs::read_to_string(config_path)
        .map_err(|e| rpc_error(format!("reading config: {e}")))?;
    let raw = bergamot_config::parse_config(&content)
        .map_err(|e| rpc_error(format!("parsing config: {e}")))?;
    let new_config = bergamot_config::Config::from_raw(raw);

    let mut config = config_arc
        .write()
        .map_err(|_| rpc_error("Config lock poisoned"))?;
    *config = new_config;

    Ok(serde_json::json!(true))
}

pub(crate) fn rpc_editserver(
    params: &serde_json::Value,
    state: &AppState,
) -> Result<serde_json::Value, JsonRpcError> {
    let config_arc = state
        .config()
        .ok_or_else(|| rpc_error("Config not available"))?;
    let config_path = state
        .config_path()
        .ok_or_else(|| rpc_error("Config path not available"))?;

    let arr = params
        .as_array()
        .ok_or_else(|| rpc_error("params must be an array"))?;

    {
        let mut config = config_arc
            .write()
            .map_err(|_| rpc_error("Config lock poisoned"))?;
        for entry in arr {
            if let (Some(name), Some(value)) = (
                entry.get("Name").and_then(|v| v.as_str()),
                entry.get("Value").and_then(|v| v.as_str()),
            ) {
                let _ = config.set_option(name, value);
            }
        }
        config.refresh_servers();
        config
            .save(config_path)
            .map_err(|e| rpc_error(format!("saving config: {e}")))?;
    }

    Ok(serde_json::json!(true))
}

pub(crate) fn rpc_scheduleresume(
    params: &serde_json::Value,
    state: &AppState,
) -> Result<serde_json::Value, JsonRpcError> {
    let arr = params.as_array();
    let seconds = arr
        .and_then(|a| a.first())
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    if seconds == 0 {
        state
            .resume_at()
            .store(0, std::sync::atomic::Ordering::Relaxed);
    } else {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        state
            .resume_at()
            .store(now + seconds, std::sync::atomic::Ordering::Relaxed);
    }
    Ok(serde_json::json!(true))
}

pub(crate) fn rpc_resetservervolume(
    params: &serde_json::Value,
    state: &AppState,
) -> Result<serde_json::Value, JsonRpcError> {
    let arr = params.as_array();
    let server_id = arr
        .and_then(|a| a.first())
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    if let Some(tracker) = state.stats_tracker() {
        tracker.reset_volume(server_id);
    }
    Ok(serde_json::json!(true))
}

pub(crate) fn rpc_loadconfig(state: &AppState) -> Result<serde_json::Value, JsonRpcError> {
    let config = state
        .config()
        .ok_or_else(|| rpc_error("Config not available"))?;
    let config = config
        .read()
        .map_err(|_| rpc_error("Config lock poisoned"))?;
    let entries: Vec<serde_json::Value> = config
        .raw()
        .iter()
        .map(|(k, v)| {
            serde_json::json!({
                "Name": k,
                "Value": v,
            })
        })
        .collect();
    Ok(serde_json::json!(entries))
}

pub(crate) fn rpc_saveconfig(
    params: &serde_json::Value,
    state: &AppState,
) -> Result<serde_json::Value, JsonRpcError> {
    let config_arc = state
        .config()
        .ok_or_else(|| rpc_error("Config not available"))?;
    let config_path = state
        .config_path()
        .ok_or_else(|| rpc_error("Config path not available"))?;

    let arr = params
        .as_array()
        .ok_or_else(|| rpc_error("params must be an array"))?;

    {
        let mut config = config_arc
            .write()
            .map_err(|_| rpc_error("Config lock poisoned"))?;
        for entry in arr {
            if let (Some(name), Some(value)) = (
                entry.get("Name").and_then(|v| v.as_str()),
                entry.get("Value").and_then(|v| v.as_str()),
            ) {
                let _ = config.set_option(name, value);
            }
        }
        config
            .save(config_path)
            .map_err(|e| rpc_error(format!("saving config: {e}")))?;
    }

    Ok(serde_json::json!(true))
}

pub(crate) fn rpc_configtemplates() -> Result<serde_json::Value, JsonRpcError> {
    let template = include_str!("nzbget.conf.template");
    let templates = vec![serde_json::json!({
        "Name": "",
        "DisplayName": "",
        "PostScript": false,
        "ScanScript": false,
        "QueueScript": false,
        "SchedulerScript": false,
        "FeedScript": false,
        "QueueEvents": "",
        "TaskTime": "",
        "Template": template,
    })];
    Ok(serde_json::json!(templates))
}

pub(crate) fn rpc_writelog(
    params: &serde_json::Value,
    state: &AppState,
) -> Result<serde_json::Value, JsonRpcError> {
    let buffer = state
        .log_buffer()
        .ok_or_else(|| rpc_error("Log buffer not available"))?;

    let arr = params
        .as_array()
        .ok_or_else(|| rpc_error("params must be an array"))?;

    let kind_str = arr.first().and_then(|v| v.as_str()).unwrap_or("info");
    let text = arr
        .get(1)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let kind = match kind_str.to_lowercase().as_str() {
        "error" => bergamot_logging::LogLevel::Error,
        "warning" => bergamot_logging::LogLevel::Warning,
        "detail" => bergamot_logging::LogLevel::Detail,
        "debug" => bergamot_logging::LogLevel::Debug,
        _ => bergamot_logging::LogLevel::Info,
    };

    buffer.push(bergamot_logging::LogMessage {
        id: 0,
        kind,
        time: chrono::Utc::now(),
        text,
        nzb_id: None,
    });

    Ok(serde_json::json!(true))
}

fn empty_server_volume(server_id: u32) -> serde_json::Value {
    let zero_size = serde_json::json!({"SizeLo": 0, "SizeHi": 0, "SizeMB": 0});
    let zero_article = serde_json::json!({"Failed": 0, "Success": 0});
    let now = chrono::Utc::now().timestamp();
    let first_day = now / 86400;
    let day_slot = 0i64;
    let seconds: Vec<_> = (0..60).map(|_| zero_size.clone()).collect();
    let minutes: Vec<_> = (0..60).map(|_| zero_size.clone()).collect();
    let hours: Vec<_> = (0..24).map(|_| zero_size.clone()).collect();
    let days = vec![zero_size.clone()];
    let article_days = vec![zero_article.clone()];
    serde_json::json!({
        "ServerID": server_id,
        "DataTime": now,
        "FirstDay": first_day,
        "TotalSizeLo": 0,
        "TotalSizeHi": 0,
        "TotalSizeMB": 0,
        "CustomSizeLo": 0,
        "CustomSizeHi": 0,
        "CustomSizeMB": 0,
        "CustomTime": now,
        "CountersResetTime": now,
        "SecSlot": 0,
        "MinSlot": 0,
        "HourSlot": 0,
        "DaySlot": day_slot,
        "BytesPerSeconds": seconds,
        "BytesPerMinutes": minutes,
        "BytesPerHours": hours,
        "BytesPerDays": days,
        "ArticlesPerDays": article_days,
    })
}

pub(crate) fn rpc_servervolumes(state: &AppState) -> Result<serde_json::Value, JsonRpcError> {
    let (tracked, tracker_date) = state
        .stats_tracker()
        .map(|t| t.snapshot_with_date())
        .unwrap_or_default();

    let mut volumes = vec![empty_server_volume(0)];
    if let Some(config) = state.config()
        && let Ok(config) = config.read()
    {
        for server in &config.servers {
            let vol = if let Some(sv) = tracked.get(&server.id) {
                build_server_volume(server.id, sv, tracker_date)
            } else {
                empty_server_volume(server.id)
            };
            volumes.push(vol);
        }
    }
    Ok(serde_json::json!(volumes))
}

fn build_server_volume(
    server_id: u32,
    sv: &bergamot_scheduler::ServerVolume,
    today: chrono::NaiveDate,
) -> serde_json::Value {
    use chrono::NaiveDate;

    let unix_epoch = NaiveDate::from_ymd_opt(1970, 1, 1).unwrap();
    let today_slot = (today - unix_epoch).num_days();

    let first_day = sv
        .daily_history
        .first()
        .map(|(d, _)| (*d - unix_epoch).num_days())
        .unwrap_or(today_slot);

    let num_days = (today_slot - first_day + 1) as usize;
    let zero_size = serde_json::json!({"SizeLo": 0, "SizeHi": 0, "SizeMB": 0});
    let mut days: Vec<serde_json::Value> = vec![zero_size; num_days];

    for (date, bytes) in &sv.daily_history {
        let slot = (*date - unix_epoch).num_days();
        let idx = (slot - first_day) as usize;
        if idx < days.len() {
            days[idx] = size_json(*bytes);
        }
    }

    let day_slot_idx = (today_slot - first_day) as usize;
    if day_slot_idx < days.len() {
        days[day_slot_idx] = size_json(sv.bytes_today);
    }

    let total_bytes: u64 = sv.daily_history.iter().map(|(_, b)| b).sum::<u64>() + sv.bytes_today;

    let zero_article = serde_json::json!({"Failed": 0, "Success": 0});
    let mut article_days: Vec<serde_json::Value> = vec![zero_article; num_days];

    for (date, success, failed) in &sv.articles_daily_history {
        let slot = (*date - unix_epoch).num_days();
        let idx = (slot - first_day) as usize;
        if idx < article_days.len() {
            article_days[idx] = serde_json::json!({"Success": success, "Failed": failed});
        }
    }

    if day_slot_idx < article_days.len() {
        article_days[day_slot_idx] = serde_json::json!({
            "Success": sv.articles_success_today,
            "Failed": sv.articles_failed_today,
        });
    }

    let now = chrono::Utc::now().timestamp();
    let seconds: Vec<_> = sv.bytes_per_seconds.iter().map(|b| size_json(*b)).collect();
    let minutes: Vec<_> = sv.bytes_per_minutes.iter().map(|b| size_json(*b)).collect();
    let hours: Vec<_> = sv.bytes_per_hours.iter().map(|b| size_json(*b)).collect();

    let sec_slot = (now % 60) as u32;
    let min_slot = ((now / 60) % 60) as u32;
    let hour_slot = ((now / 3600) % 24) as u32;

    serde_json::json!({
        "ServerID": server_id,
        "DataTime": now,
        "FirstDay": first_day,
        "TotalSizeLo": (total_bytes & 0xFFFF_FFFF) as u32,
        "TotalSizeHi": (total_bytes >> 32) as u32,
        "TotalSizeMB": total_bytes / (1024 * 1024),
        "CustomSizeLo": 0,
        "CustomSizeHi": 0,
        "CustomSizeMB": 0,
        "CustomTime": now,
        "CountersResetTime": now,
        "SecSlot": sec_slot,
        "MinSlot": min_slot,
        "HourSlot": hour_slot,
        "DaySlot": day_slot_idx,
        "BytesPerSeconds": seconds,
        "BytesPerMinutes": minutes,
        "BytesPerHours": hours,
        "BytesPerDays": days,
        "ArticlesPerDays": article_days,
    })
}

pub(crate) async fn rpc_schedulerstats(
    state: &AppState,
) -> Result<serde_json::Value, JsonRpcError> {
    let queue = require_queue(state)?;
    let stats = queue.get_scheduler_stats().await.map_err(rpc_error)?;
    let entries: Vec<serde_json::Value> = stats
        .into_iter()
        .map(|s| {
            serde_json::json!({
                "ServerID": s.server_id,
                "ServerName": s.server_name,
                "Level": s.level,
                "MaxConnections": s.max_connections,
                "ActiveCount": s.active_count,
                "PendingBytes": s.pending_bytes,
                "EwmaBytesPerSec": s.ewma_bytes_per_sec,
                "WfqRatio": s.wfq_ratio,
                "InBackoff": s.in_backoff,
                "TotalBytesDownloaded": s.total_bytes_downloaded,
                "TotalArticlesSuccess": s.total_articles_success,
                "TotalArticlesFailed": s.total_articles_failed,
            })
        })
        .collect();
    Ok(serde_json::json!(entries))
}

fn size_json(bytes: u64) -> serde_json::Value {
    serde_json::json!({
        "SizeLo": (bytes & 0xFFFF_FFFF) as u32,
        "SizeHi": (bytes >> 32) as u32,
        "SizeMB": bytes / (1024 * 1024),
    })
}

pub(crate) fn rpc_loadlog(
    params: &serde_json::Value,
    state: &AppState,
) -> Result<serde_json::Value, JsonRpcError> {
    let buffer = state
        .log_buffer()
        .ok_or_else(|| rpc_error("Log buffer not available"))?;

    let arr = params.as_array();
    let id_from = arr
        .and_then(|a| a.first())
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let number_of_entries = arr
        .and_then(|a| a.get(1))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;

    let mut messages = buffer.messages_since(id_from.saturating_sub(1));
    if number_of_entries > 0 && id_from == 0 {
        let len = messages.len();
        if len > number_of_entries {
            messages = messages.split_off(len - number_of_entries);
        }
    }
    let entries: Vec<serde_json::Value> = messages
        .into_iter()
        .map(|m| {
            serde_json::json!({
                "ID": m.id,
                "Kind": match m.kind {
                    bergamot_logging::LogLevel::Debug => "DEBUG",
                    bergamot_logging::LogLevel::Detail => "DETAIL",
                    bergamot_logging::LogLevel::Info => "INFO",
                    bergamot_logging::LogLevel::Warning => "WARNING",
                    bergamot_logging::LogLevel::Error => "ERROR",
                },
                "Time": m.time.timestamp(),
                "Text": m.text,
            })
        })
        .collect();
    Ok(serde_json::json!(entries))
}

pub(crate) async fn rpc_rate(
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

pub(crate) async fn rpc_history(
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
            let file_size_lo = (e.size & 0xFFFF_FFFF) as u32;
            let file_size_hi = (e.size >> 32) as u32;
            let file_size_mb = (e.size / (1024 * 1024)) as u32;
            let kind = match e.kind {
                bergamot_core::models::HistoryKind::Nzb => "NZB",
                bergamot_core::models::HistoryKind::Url => "URL",
                bergamot_core::models::HistoryKind::DupHidden => "DUP",
            };
            let status = format_history_status(&e);
            let par_status_str = format_par_status(e.par_status);
            let unpack_status_str = format_unpack_status(e.unpack_status);
            let move_status_str = format_move_status(e.move_status);
            let delete_status_str = format_delete_status(e.delete_status);
            let mark_status_str = format_mark_status(e.mark_status);

            let mut m = serde_json::Map::new();
            m.insert("ID".into(), serde_json::json!(e.id));
            m.insert("NZBID".into(), serde_json::json!(e.id));
            m.insert("Kind".into(), serde_json::json!(kind));
            m.insert("Name".into(), serde_json::json!(e.name));
            m.insert("NZBName".into(), serde_json::json!(e.name));
            m.insert("NZBNicename".into(), serde_json::json!(e.name));
            m.insert("Status".into(), serde_json::json!(status));
            m.insert("FileSizeMB".into(), serde_json::json!(file_size_mb));
            m.insert("FileSizeLo".into(), serde_json::json!(file_size_lo));
            m.insert("FileSizeHi".into(), serde_json::json!(file_size_hi));
            m.insert("Category".into(), serde_json::json!(e.category));
            m.insert("HistoryTime".into(), serde_json::json!(time_secs));
            m.insert("MinPostTime".into(), serde_json::json!(0));
            m.insert("MaxPostTime".into(), serde_json::json!(0));
            m.insert(
                "Deleted".into(),
                serde_json::json!(e.delete_status != bergamot_core::models::DeleteStatus::None),
            );
            m.insert("DupeKey".into(), serde_json::json!(""));
            m.insert("DupeScore".into(), serde_json::json!(0));
            m.insert("DupeMode".into(), serde_json::json!("SCORE"));
            m.insert("ParStatus".into(), serde_json::json!(par_status_str));
            m.insert("UnpackStatus".into(), serde_json::json!(unpack_status_str));
            m.insert("MoveStatus".into(), serde_json::json!(move_status_str));
            let script_status_str = match e.script_status {
                bergamot_core::models::ScriptStatus::None => "NONE",
                bergamot_core::models::ScriptStatus::Failure => "FAILURE",
                bergamot_core::models::ScriptStatus::Success => "SUCCESS",
            };
            m.insert("ScriptStatus".into(), serde_json::json!(script_status_str));
            m.insert("DeleteStatus".into(), serde_json::json!(delete_status_str));
            m.insert("MarkStatus".into(), serde_json::json!(mark_status_str));
            m.insert("UrlStatus".into(), serde_json::json!("NONE"));
            m.insert("DupStatus".into(), serde_json::json!("NONE"));
            m.insert("ExParStatus".into(), serde_json::json!("NONE"));
            m.insert("ExtraParBlocks".into(), serde_json::json!(0));
            m.insert("Health".into(), serde_json::json!(e.health));
            m.insert("CriticalHealth".into(), serde_json::json!(0));
            m.insert("Parameters".into(), serde_json::json!([]));
            m.insert("ServerStats".into(), serde_json::json!([]));
            m.insert(
                "SuccessArticles".into(),
                serde_json::json!(e.success_article_count),
            );
            m.insert(
                "FailedArticles".into(),
                serde_json::json!(e.failed_article_count),
            );
            m.insert(
                "TotalArticles".into(),
                serde_json::json!(e.total_article_count),
            );
            m.insert("RemainingFileCount".into(), serde_json::json!(0));
            m.insert(
                "RemainingParCount".into(),
                serde_json::json!(e.remaining_par_count),
            );
            m.insert("FileCount".into(), serde_json::json!(e.file_count));
            m.insert("RetryData".into(), serde_json::json!(false));
            m.insert("FinalDir".into(), serde_json::json!(""));
            m.insert("DestDir".into(), serde_json::json!(""));
            m.insert("URL".into(), serde_json::json!(""));
            m.insert("DownloadedSizeMB".into(), serde_json::json!(file_size_mb));
            m.insert("DownloadedSizeLo".into(), serde_json::json!(file_size_lo));
            m.insert("DownloadedSizeHi".into(), serde_json::json!(file_size_hi));
            m.insert(
                "DownloadTimeSec".into(),
                serde_json::json!(e.download_time_sec),
            );
            m.insert(
                "PostTotalTimeSec".into(),
                serde_json::json!(e.post_total_sec),
            );
            m.insert("ParTimeSec".into(), serde_json::json!(e.par_sec));
            m.insert("RepairTimeSec".into(), serde_json::json!(e.repair_sec));
            m.insert("UnpackTimeSec".into(), serde_json::json!(e.unpack_sec));
            m.insert("MessageCount".into(), serde_json::json!(0));
            m.insert("ScriptStatuses".into(), serde_json::json!([]));
            m.insert("NZBFilename".into(), serde_json::json!(""));
            serde_json::Value::Object(m)
        })
        .collect();
    Ok(serde_json::json!(result))
}

fn format_history_status(e: &bergamot_queue::HistoryListEntry) -> String {
    if e.mark_status == bergamot_core::models::MarkStatus::Good {
        return "SUCCESS/GOOD".to_string();
    }
    if e.mark_status == bergamot_core::models::MarkStatus::Bad {
        return "FAILURE/BAD".to_string();
    }
    match e.delete_status {
        bergamot_core::models::DeleteStatus::Manual => return "DELETED/MANUAL".to_string(),
        bergamot_core::models::DeleteStatus::Health => return "DELETED/HEALTH".to_string(),
        bergamot_core::models::DeleteStatus::Dupe => return "DELETED/DUPE".to_string(),
        bergamot_core::models::DeleteStatus::Bad => return "DELETED/BAD".to_string(),
        bergamot_core::models::DeleteStatus::Scan => return "DELETED/SCAN".to_string(),
        bergamot_core::models::DeleteStatus::Copy => return "DELETED/COPY".to_string(),
        _ => {}
    }
    if e.par_status == bergamot_core::models::ParStatus::Failure
        || e.unpack_status == bergamot_core::models::UnpackStatus::Failure
    {
        return "FAILURE".to_string();
    }
    "SUCCESS".to_string()
}

fn format_par_status(s: bergamot_core::models::ParStatus) -> &'static str {
    match s {
        bergamot_core::models::ParStatus::None => "NONE",
        bergamot_core::models::ParStatus::Failure => "FAILURE",
        bergamot_core::models::ParStatus::Success => "SUCCESS",
        bergamot_core::models::ParStatus::RepairPossible => "REPAIR_POSSIBLE",
        bergamot_core::models::ParStatus::Manual => "MANUAL",
    }
}

fn format_unpack_status(s: bergamot_core::models::UnpackStatus) -> &'static str {
    match s {
        bergamot_core::models::UnpackStatus::None => "NONE",
        bergamot_core::models::UnpackStatus::Failure => "FAILURE",
        bergamot_core::models::UnpackStatus::Success => "SUCCESS",
        bergamot_core::models::UnpackStatus::Password => "PASSWORD",
        bergamot_core::models::UnpackStatus::Space => "SPACE",
    }
}

fn format_move_status(s: bergamot_core::models::MoveStatus) -> &'static str {
    match s {
        bergamot_core::models::MoveStatus::None => "NONE",
        bergamot_core::models::MoveStatus::Failure => "FAILURE",
        bergamot_core::models::MoveStatus::Success => "SUCCESS",
    }
}

fn format_delete_status(s: bergamot_core::models::DeleteStatus) -> &'static str {
    match s {
        bergamot_core::models::DeleteStatus::None => "NONE",
        bergamot_core::models::DeleteStatus::Manual => "MANUAL",
        bergamot_core::models::DeleteStatus::Health => "HEALTH",
        bergamot_core::models::DeleteStatus::Dupe => "DUPE",
        bergamot_core::models::DeleteStatus::Bad => "BAD",
        bergamot_core::models::DeleteStatus::Scan => "SCAN",
        bergamot_core::models::DeleteStatus::Copy => "COPY",
    }
}

fn format_mark_status(s: bergamot_core::models::MarkStatus) -> &'static str {
    match s {
        bergamot_core::models::MarkStatus::None => "NONE",
        bergamot_core::models::MarkStatus::Good => "GOOD",
        bergamot_core::models::MarkStatus::Bad => "BAD",
        bergamot_core::models::MarkStatus::Success => "SUCCESS",
    }
}

pub(crate) async fn rpc_testserver(
    params: &serde_json::Value,
) -> Result<serde_json::Value, JsonRpcError> {
    let arr = params
        .as_array()
        .ok_or_else(|| rpc_error("params must be an array"))?;

    let host = arr
        .first()
        .and_then(|v| v.as_str())
        .ok_or_else(|| rpc_error("missing host parameter"))?;
    let port = arr.get(1).and_then(|v| v.as_u64()).unwrap_or(119) as u16;
    let username = arr.get(2).and_then(|v| v.as_str()).unwrap_or("");
    let password = arr.get(3).and_then(|v| v.as_str()).unwrap_or("");
    let encryption_str = arr.get(4).and_then(|v| v.as_str()).unwrap_or("no");
    let encryption = match encryption_str.to_lowercase().as_str() {
        "yes" | "tls" => bergamot_nntp::Encryption::Tls,
        "starttls" => bergamot_nntp::Encryption::StartTls,
        _ => bergamot_nntp::Encryption::None,
    };

    let server = bergamot_nntp::NewsServer {
        id: 0,
        name: "test".to_string(),
        active: true,
        host: host.to_string(),
        port,
        username: if username.is_empty() {
            None
        } else {
            Some(username.to_string())
        },
        password: if password.is_empty() {
            None
        } else {
            Some(password.to_string())
        },
        encryption,
        cipher: None,
        connections: 1,
        retention: 0,
        level: 0,
        optional: false,
        group: 0,
        join_group: false,
        ip_version: bergamot_nntp::IpVersion::Auto,
        cert_verification: true,
    };

    match bergamot_nntp::NntpConnection::connect(&server).await {
        Ok(mut conn) => {
            if !username.is_empty()
                && let Err(e) = conn.authenticate(username, password).await
            {
                return Ok(serde_json::json!(format!("Authentication failed: {e}")));
            }
            let _ = conn.quit().await;
            Ok(serde_json::json!("Connection successful"))
        }
        Err(e) => Ok(serde_json::json!(e.to_string())),
    }
}

pub(crate) async fn rpc_pausedownload(state: &AppState) -> Result<serde_json::Value, JsonRpcError> {
    let queue = require_queue(state)?;
    queue.pause_all().await.map_err(rpc_error)?;
    state
        .download_paused()
        .store(true, std::sync::atomic::Ordering::Relaxed);
    Ok(serde_json::json!(true))
}

pub(crate) async fn rpc_resumedownload(
    state: &AppState,
) -> Result<serde_json::Value, JsonRpcError> {
    let queue = require_queue(state)?;
    queue.resume_all().await.map_err(rpc_error)?;
    state
        .download_paused()
        .store(false, std::sync::atomic::Ordering::Relaxed);
    Ok(serde_json::json!(true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use bergamot_core::models::Priority;
    use bergamot_nntp::StatsRecorder;
    use bergamot_queue::QueueCoordinator;
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

    fn nzb_base64() -> String {
        base64::engine::general_purpose::STANDARD.encode(VALID_NZB.as_bytes())
    }

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
        bergamot_queue::QueueHandle,
        tokio::task::JoinHandle<()>,
    ) {
        let (mut coordinator, handle, _rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );
        let coordinator_handle = tokio::spawn(async move { coordinator.run().await });
        let state = AppState::default().with_queue(handle.clone());
        (state, handle, coordinator_handle)
    }

    #[test]
    fn version_returns_expected() {
        let state = AppState::default();
        assert_eq!(state.version(), "26.0");
    }

    #[tokio::test]
    async fn dispatch_append_adds_nzb_and_returns_id() {
        let (state, handle, _coord) = state_with_queue();
        let params = serde_json::json!(["test.nzb", nzb_base64(), "", 0]);
        let result = rpc_append(&params, &state).await.expect("append");
        assert_eq!(result, serde_json::json!(1));
        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn dispatch_append_saves_nzb_to_disk_state() {
        use bergamot_diskstate::{DiskState, JsonFormat};

        let tmp_disk = tempfile::tempdir().expect("tempdir");
        let disk = std::sync::Arc::new(
            DiskState::new(tmp_disk.path().to_path_buf(), JsonFormat).expect("disk"),
        );

        let (mut coordinator, handle, _rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );
        let coordinator_handle = tokio::spawn(async move { coordinator.run().await });
        let state = AppState::default()
            .with_queue(handle.clone())
            .with_disk(disk.clone());

        let params = serde_json::json!(["test.nzb", nzb_base64(), "", 0]);
        let result = rpc_append(&params, &state).await.expect("append");
        let id = result.as_u64().expect("id") as u32;

        let saved = disk.load_nzb_file(id).expect("load saved nzb");
        assert_eq!(saved, VALID_NZB.as_bytes());

        handle.shutdown().await.expect("shutdown");
        let _ = coordinator_handle.await;
    }

    #[tokio::test]
    async fn dispatch_append_with_category_and_priority() {
        let (state, handle, _coord) = state_with_queue();
        let params = serde_json::json!(["test.nzb", nzb_base64(), "movies", 50]);
        let result = rpc_append(&params, &state).await.expect("append");
        assert_eq!(result, serde_json::json!(1));

        let list = handle.get_nzb_list().await.expect("list");
        assert_eq!(list.len(), 1);
        handle.shutdown().await.expect("shutdown");
    }

    #[test]
    fn is_url_detects_http_and_https() {
        assert!(is_url("http://example.com/file.nzb"));
        assert!(is_url("https://example.com/file.nzb"));
        assert!(!is_url("/path/to/file.nzb"));
        assert!(!is_url("relative/path.nzb"));
    }

    #[tokio::test]
    async fn dispatch_append_url_returns_error_for_unreachable_host() {
        let (state, handle, _coord) = state_with_queue();
        let params = serde_json::json!(["", "http://127.0.0.1:1/nonexistent.nzb", "", 0]);
        let result = rpc_append(&params, &state).await;
        assert!(result.is_err());
        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn dispatch_listgroups_returns_nzb_entries() {
        let (state, handle, _coord) = state_with_queue();
        let _nzb_file = nzb_tempfile();
        handle
            .add_nzb(_nzb_file.path().to_path_buf(), None, Priority::Normal)
            .await
            .expect("add");

        let result = rpc_listgroups(&state).await.expect("listgroups");
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
            .add_nzb(_nzb_file.path().to_path_buf(), None, Priority::Normal)
            .await
            .expect("add");

        let params = serde_json::json!(["GroupPause", "", [id]]);
        let result = rpc_editqueue(&params, &state).await.expect("editqueue");
        assert_eq!(result, serde_json::json!(true));
        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn dispatch_shutdown_triggers_shutdown() {
        let (shutdown_handle, token) = crate::shutdown::ShutdownHandle::new();
        let (mut coordinator, handle, _rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );
        tokio::spawn(async move { coordinator.run().await });
        let state = AppState::default()
            .with_queue(handle.clone())
            .with_shutdown(shutdown_handle);

        let result = rpc_shutdown(&state).await.expect("shutdown");
        assert_eq!(result, serde_json::json!(true));
        assert!(token.is_cancelled());
    }

    #[tokio::test]
    async fn rpc_without_queue_returns_error() {
        let state = AppState::default();
        let result = rpc_listgroups(&state).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn dispatch_listfiles_returns_file_details() {
        let (state, handle, _coord) = state_with_queue();
        let _nzb_file = nzb_tempfile();
        let id = handle
            .add_nzb(_nzb_file.path().to_path_buf(), None, Priority::Normal)
            .await
            .expect("add");

        let result = rpc_listfiles(&serde_json::json!([id]), &state)
            .await
            .expect("listfiles");
        let files = result.as_array().expect("array");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0]["NZBID"], id);
        assert_eq!(files[0]["Filename"], "data.rar");
        assert_eq!(files[0]["TotalArticles"], 1);
        handle.shutdown().await.expect("shutdown");
    }

    #[test]
    fn dispatch_postqueue_returns_empty_array() {
        let state = AppState::default();
        let result = rpc_postqueue(&state).expect("postqueue");
        assert!(result.is_array(), "postqueue should return an array");
        assert_eq!(result.as_array().unwrap().len(), 0);
    }

    fn state_with_log() -> AppState {
        let buffer = std::sync::Arc::new(bergamot_logging::LogBuffer::new(100));
        AppState::default().with_log_buffer(buffer)
    }

    #[test]
    fn dispatch_writelog_adds_to_buffer() {
        let state = state_with_log();
        let params = serde_json::json!(["info", "test message"]);
        let result = rpc_writelog(&params, &state).expect("writelog");
        assert_eq!(result, serde_json::json!(true));

        let messages = state.log_buffer().unwrap().messages_since(0);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].text, "test message");
    }

    #[test]
    fn dispatch_loadlog_returns_written_messages() {
        let state = state_with_log();
        rpc_writelog(&serde_json::json!(["warning", "hello"]), &state).expect("writelog");

        let params = serde_json::json!([0]);
        let result = rpc_loadlog(&params, &state).expect("loadlog");
        let entries = result.as_array().expect("array");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["Text"], "hello");
        assert_eq!(entries[0]["Kind"], "WARNING");
    }

    #[test]
    fn dispatch_log_returns_same_as_loadlog() {
        let state = state_with_log();
        rpc_writelog(&serde_json::json!(["info", "log entry"]), &state).expect("writelog");

        let params = serde_json::json!([0]);
        let result = rpc_loadlog(&params, &state).expect("log");
        let entries = result.as_array().expect("array");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["Text"], "log entry");
    }

    #[test]
    fn dispatch_servervolumes_returns_total_volume() {
        let state = AppState::default();
        let result = rpc_servervolumes(&state).expect("servervolumes");
        let volumes = result.as_array().expect("array");
        assert_eq!(volumes.len(), 1);
        assert_eq!(volumes[0]["ServerID"], 0);
    }

    fn state_with_config() -> (AppState, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_path = tmp.path().join("bergamot.conf");
        std::fs::write(&config_path, "ControlPort=6789\nMainDir=/tmp/bergamot\n").expect("write");
        let raw = bergamot_config::parse_config("ControlPort=6789\nMainDir=/tmp/bergamot\n")
            .expect("parse");
        let config = bergamot_config::Config::from_raw(raw);
        let config = std::sync::Arc::new(std::sync::RwLock::new(config));
        let state = AppState::default().with_config(config, config_path);
        (state, tmp)
    }

    #[test]
    fn dispatch_loadconfig_returns_config_entries() {
        let (state, _tmp) = state_with_config();
        let result = rpc_loadconfig(&state).expect("loadconfig");
        let entries = result.as_array().expect("array");
        assert!(entries.iter().any(|e| e["Name"] == "ControlPort"));
    }

    #[test]
    fn dispatch_config_returns_same_as_loadconfig() {
        let (state, _tmp) = state_with_config();
        let result = rpc_loadconfig(&state).expect("config");
        let entries = result.as_array().expect("array");
        assert!(entries.iter().any(|e| e["Name"] == "MainDir"));
    }

    #[test]
    fn dispatch_saveconfig_persists_changes() {
        let (state, tmp) = state_with_config();
        let params = serde_json::json!([{"Name": "DownloadRate", "Value": "500"}]);
        let result = rpc_saveconfig(&params, &state).expect("saveconfig");
        assert_eq!(result, serde_json::json!(true));

        let saved = std::fs::read_to_string(tmp.path().join("bergamot.conf")).expect("read");
        assert!(saved.contains("DownloadRate=500"));
    }

    #[test]
    fn dispatch_configtemplates_returns_known_options() {
        let result = rpc_configtemplates().expect("configtemplates");
        let entries = result.as_array().expect("array");
        assert!(entries.iter().any(|e| e["Name"] == ""));
        let template = entries[0]["Template"].as_str().expect("template string");
        assert!(template.contains("MainDir"));
        assert!(template.contains("ControlPort"));
    }

    #[tokio::test]
    async fn dispatch_history_returns_empty_array() {
        let (state, handle, _coord) = state_with_queue();
        let params = serde_json::json!([false]);
        let result = rpc_history(&params, &state).await.expect("history");
        assert_eq!(result, serde_json::json!([]));
        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn dispatch_rate_sets_download_rate() {
        let (state, handle, _coord) = state_with_queue();
        let params = serde_json::json!([500]);
        let result = rpc_rate(&params, &state).await.expect("rate");
        assert_eq!(result, serde_json::json!(true));
        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn dispatch_pausedownload_returns_true() {
        let (state, handle, _coord) = state_with_queue();
        let result = rpc_pausedownload(&state).await.expect("pausedownload");
        assert_eq!(result, serde_json::json!(true));
        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn dispatch_resumedownload_returns_true() {
        let (state, handle, _coord) = state_with_queue();
        let result = rpc_resumedownload(&state).await.expect("resumedownload");
        assert_eq!(result, serde_json::json!(true));
        handle.shutdown().await.expect("shutdown");
    }

    #[test]
    fn dispatch_pausepost_sets_paused_state() {
        let state = AppState::default();
        let result = rpc_pausepost(&state).expect("pausepost");
        assert_eq!(result, serde_json::json!(true));
        assert!(
            state
                .postproc_paused()
                .load(std::sync::atomic::Ordering::Relaxed)
        );
    }

    #[test]
    fn dispatch_resumepost_clears_paused_state() {
        let state = AppState::default();
        state
            .postproc_paused()
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let result = rpc_resumepost(&state).expect("resumepost");
        assert_eq!(result, serde_json::json!(true));
        assert!(
            !state
                .postproc_paused()
                .load(std::sync::atomic::Ordering::Relaxed)
        );
    }

    #[test]
    fn dispatch_pausescan_sets_scan_paused() {
        let state = AppState::default();
        let result = rpc_pausescan(&state).expect("pausescan");
        assert_eq!(result, serde_json::json!(true));
        assert!(
            state
                .scan_paused()
                .load(std::sync::atomic::Ordering::Relaxed)
        );
    }

    #[test]
    fn dispatch_resumescan_clears_scan_paused() {
        let state = AppState::default();
        state
            .scan_paused()
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let result = rpc_resumescan(&state).expect("resumescan");
        assert_eq!(result, serde_json::json!(true));
        assert!(
            !state
                .scan_paused()
                .load(std::sync::atomic::Ordering::Relaxed)
        );
    }

    #[tokio::test]
    async fn dispatch_scan_triggers_scan() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let state = AppState::default().with_scan_trigger(tx);
        let result = rpc_scan(&state).await.expect("scan");
        assert_eq!(result, serde_json::json!(true));
        assert!(rx.try_recv().is_ok());
    }

    #[test]
    fn dispatch_sysinfo_returns_version_and_os() {
        let state = AppState::default();
        let result = rpc_sysinfo(&state).expect("sysinfo");
        assert_eq!(result["Version"], "26.0");
        assert!(result["OS"]["Name"].as_str().is_some());
        assert!(result["CPU"]["Arch"].as_str().is_some());
        assert!(result["UptimeSec"].as_u64().is_some());
    }

    #[test]
    fn dispatch_systemhealth_reports_queue_status() {
        let state = AppState::default();
        let result = rpc_systemhealth(&state).expect("systemhealth");
        assert_eq!(result["QueueAvailable"], false);
    }

    #[tokio::test]
    async fn dispatch_systemhealth_reports_queue_available() {
        let (state, handle, _coord) = state_with_queue();
        let result = rpc_systemhealth(&state).expect("systemhealth");
        assert_eq!(result["QueueAvailable"], true);
        assert_eq!(result["Healthy"], true);
        handle.shutdown().await.expect("shutdown");
    }

    #[test]
    fn dispatch_reload_rereads_config_from_disk() {
        let (state, tmp) = state_with_config();
        let config_path = tmp.path().join("bergamot.conf");
        std::fs::write(
            &config_path,
            "ControlPort=6789\nMainDir=/tmp/bergamot\nDownloadRate=999\n",
        )
        .expect("write");

        let result = rpc_reload(&state).expect("reload");
        assert_eq!(result, serde_json::json!(true));

        let config = state.config().unwrap().read().unwrap();
        assert_eq!(config.download_rate, 999);
    }

    #[test]
    fn dispatch_reload_no_config_returns_error() {
        let state = AppState::default();
        let result = rpc_reload(&state);
        assert!(result.is_err());
    }

    #[test]
    fn dispatch_editserver_updates_config() {
        let (state, tmp) = state_with_config();
        let params = serde_json::json!([
            {"Name": "Server1.Host", "Value": "news.example.com"},
            {"Name": "Server1.Port", "Value": "563"},
            {"Name": "Server1.Connections", "Value": "8"}
        ]);
        let result = rpc_editserver(&params, &state).expect("editserver");
        assert_eq!(result, serde_json::json!(true));

        let saved = std::fs::read_to_string(tmp.path().join("bergamot.conf")).expect("read");
        assert!(saved.contains("Server1.Host=news.example.com"));
        assert!(saved.contains("Server1.Port=563"));
    }

    #[test]
    fn dispatch_editserver_no_config_returns_error() {
        let state = AppState::default();
        let params = serde_json::json!([{"Name": "Server1.Host", "Value": "test"}]);
        let result = rpc_editserver(&params, &state);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn dispatch_scheduleresume_sets_resume_time() {
        let (state, handle, _coord) = state_with_queue();
        rpc_pausedownload(&state).await.expect("pause");

        let params = serde_json::json!([60]);
        let result = rpc_scheduleresume(&params, &state).expect("scheduleresume");
        assert_eq!(result, serde_json::json!(true));

        let resume_at = state.resume_at().load(std::sync::atomic::Ordering::Relaxed);
        assert!(
            resume_at > 0,
            "resume_at should be set to a future timestamp"
        );

        handle.shutdown().await.expect("shutdown");
    }

    #[test]
    fn dispatch_scheduleresume_zero_clears_timer() {
        let state = AppState::default();
        state
            .resume_at()
            .store(9999, std::sync::atomic::Ordering::Relaxed);

        let params = serde_json::json!([0]);
        let result = rpc_scheduleresume(&params, &state).expect("scheduleresume");
        assert_eq!(result, serde_json::json!(true));
        assert_eq!(
            state.resume_at().load(std::sync::atomic::Ordering::Relaxed),
            0
        );
    }

    #[test]
    fn dispatch_resetservervolume_returns_true() {
        let state = AppState::default();
        let params = serde_json::json!([0]);
        let result = rpc_resetservervolume(&params, &state).expect("resetservervolume");
        assert_eq!(result, serde_json::json!(true));
    }

    #[test]
    fn dispatch_resetservervolume_clears_tracker() {
        let config_raw = bergamot_config::parse_config("Server1.Host=test\n").expect("parse");
        let config = bergamot_config::Config::from_raw(config_raw);
        let tracker = bergamot_scheduler::StatsTracker::from_config(&config);
        let shared = std::sync::Arc::new(bergamot_scheduler::SharedStatsTracker::new(tracker));
        shared.record_bytes(1, 1024);
        let state = AppState::default().with_stats_tracker(shared.clone());

        let params = serde_json::json!([1]);
        let result = rpc_resetservervolume(&params, &state).expect("resetservervolume");
        assert_eq!(result, serde_json::json!(true));

        let volumes = shared.snapshot_volumes();
        let vol = volumes.get(&1).expect("server 1");
        assert_eq!(vol.bytes_today, 0);
        assert!(vol.daily_history.is_empty());
    }

    #[tokio::test]
    async fn dispatch_testserver_connects_to_unreachable_host() {
        let params = serde_json::json!([
            "unreachable.invalid", // host
            119,                   // port
            "",                    // username
            "",                    // password
            "no",                  // encryption
            1                      // connections
        ]);
        let result = rpc_testserver(&params)
            .await
            .expect("testserver should return Ok with error message");
        let msg = result.as_str().expect("should be a string");
        assert!(!msg.is_empty(), "should contain an error message");
    }

    #[tokio::test]
    async fn dispatch_testserver_missing_params_returns_error() {
        let result = rpc_testserver(&serde_json::json!([])).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn dispatch_feeds_returns_empty_array() {
        let state = AppState::default();
        let result = rpc_feeds(&state).await.expect("feeds");
        assert_eq!(result, serde_json::json!([]));
    }

    fn state_with_history() -> (
        AppState,
        bergamot_queue::QueueHandle,
        tokio::task::JoinHandle<()>,
    ) {
        let (mut coordinator, handle, _rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );
        coordinator.add_to_history(
            bergamot_core::models::NzbInfo {
                id: 1,
                kind: bergamot_core::models::NzbKind::Nzb,
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
                dup_mode: bergamot_core::models::DupMode::Score,
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
                par_total_article_count: 0,
                par_failed_article_count: 0,
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
                par_status: bergamot_core::models::ParStatus::Success,
                unpack_status: bergamot_core::models::UnpackStatus::Success,
                move_status: bergamot_core::models::MoveStatus::Success,
                delete_status: bergamot_core::models::DeleteStatus::None,
                mark_status: bergamot_core::models::MarkStatus::None,
                url_status: bergamot_core::models::UrlStatus::None,
                script_status: bergamot_core::models::ScriptStatus::None,
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
            bergamot_core::models::HistoryKind::Nzb,
        );
        let coordinator_handle = tokio::spawn(async move { coordinator.run().await });
        let state = AppState::default().with_queue(handle.clone());
        (state, handle, coordinator_handle)
    }

    #[tokio::test]
    async fn dispatch_history_returns_entries() {
        let (state, handle, _coord) = state_with_history();
        let params = serde_json::json!([false]);
        let result = rpc_history(&params, &state).await.expect("history");
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
        let result = rpc_editqueue(&params, &state).await.expect("editqueue");
        assert_eq!(result, serde_json::json!(true));

        let history_params = serde_json::json!([false]);
        let history = rpc_history(&history_params, &state).await.expect("history");
        assert_eq!(history.as_array().expect("array").len(), 0);

        let groups = rpc_listgroups(&state).await.expect("listgroups");
        assert_eq!(groups.as_array().expect("array").len(), 1);

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn dispatch_editqueue_history_mark_bad() {
        let (state, handle, _coord) = state_with_history();
        let params = serde_json::json!(["HistoryMarkBad", "", [1]]);
        let result = rpc_editqueue(&params, &state).await.expect("editqueue");
        assert_eq!(result, serde_json::json!(true));

        let history_params = serde_json::json!([false]);
        let history = rpc_history(&history_params, &state).await.expect("history");
        let entries = history.as_array().expect("array");
        assert_eq!(entries[0]["Status"], "FAILURE/BAD");

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn dispatch_editqueue_history_delete() {
        let (state, handle, _coord) = state_with_history();
        let params = serde_json::json!(["HistoryDelete", "", [1]]);
        let result = rpc_editqueue(&params, &state).await.expect("editqueue");
        assert_eq!(result, serde_json::json!(true));

        let history_params = serde_json::json!([false]);
        let history = rpc_history(&history_params, &state).await.expect("history");
        assert_eq!(history.as_array().expect("array").len(), 0);

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn dispatch_editqueue_history_redownload() {
        let (state, handle, _coord) = state_with_history();
        let params = serde_json::json!(["HistoryRedownload", "", [1]]);
        let result = rpc_editqueue(&params, &state).await.expect("editqueue");
        assert_eq!(result, serde_json::json!(true));

        let history_params = serde_json::json!([false]);
        let history = rpc_history(&history_params, &state).await.expect("history");
        assert_eq!(history.as_array().expect("array").len(), 0);

        let groups = rpc_listgroups(&state).await.expect("listgroups");
        assert_eq!(groups.as_array().expect("array").len(), 1);

        handle.shutdown().await.expect("shutdown");
    }

    fn full_config_content() -> &'static str {
        concat!(
            "MainDir=/data\n",
            "DestDir=/data/completed\n",
            "InterDir=/data/intermediate\n",
            "NzbDir=/data/nzb\n",
            "QueueDir=/data/queue\n",
            "TempDir=/data/tmp\n",
            "ScriptDir=/data/scripts\n",
            "Extensions=ext1, ext2\n",
            "ScriptOrder=ext2, ext1\n",
            "LogFile=/data/bergamot.log\n",
            "ControlIP=127.0.0.1\n",
            "ControlPort=6789\n",
            "ControlUsername=admin\n",
            "ControlPassword=secret\n",
            "RestrictedUsername=viewer\n",
            "RestrictedPassword=viewpass\n",
            "AddUsername=adder\n",
            "AddPassword=addpass\n",
            "FormAuth=yes\n",
            "SecureControl=no\n",
            "SecurePort=6791\n",
            "SecureCert=/path/cert.pem\n",
            "SecureKey=/path/key.pem\n",
            "CertCheck=no\n",
            "Server1.Active=yes\n",
            "Server1.Name=Primary\n",
            "Server1.Level=0\n",
            "Server1.Optional=no\n",
            "Server1.Group=0\n",
            "Server1.Host=news.example.com\n",
            "Server1.Port=563\n",
            "Server1.Username=user1\n",
            "Server1.Password=pass1\n",
            "Server1.JoinGroup=no\n",
            "Server1.Encryption=yes\n",
            "Server1.Cipher=AES256\n",
            "Server1.Connections=8\n",
            "Server1.Retention=1200\n",
            "Server1.IpVersion=auto\n",
            "Server1.Notes=primary server\n",
            "Server1.CertVerification=strict\n",
            "Server2.Active=no\n",
            "Server2.Name=Backup\n",
            "Server2.Level=1\n",
            "Server2.Optional=yes\n",
            "Server2.Group=1\n",
            "Server2.Host=backup.example.com\n",
            "Server2.Port=119\n",
            "Server2.Username=user2\n",
            "Server2.Password=pass2\n",
            "Server2.JoinGroup=yes\n",
            "Server2.Encryption=no\n",
            "Server2.Cipher=\n",
            "Server2.Connections=4\n",
            "Server2.Retention=0\n",
            "Server2.IpVersion=ipv4\n",
            "Server2.Notes=\n",
            "Server2.CertVerification=none\n",
            "Category1.Name=Movies\n",
            "Category1.DestDir=/data/movies\n",
            "Category1.Unpack=yes\n",
            "Category1.Extensions=ext1\n",
            "Category1.Aliases=Films, Cinema\n",
            "Category2.Name=TV\n",
            "Category2.DestDir=/data/tv\n",
            "Category2.Unpack=no\n",
            "Category2.Extensions=\n",
            "Category2.Aliases=Television, Series\n",
            "AppendCategoryDir=yes\n",
            "NzbDirInterval=5\n",
            "DupeCheck=yes\n",
            "ContinuePartial=yes\n",
            "ArticleCache=256\n",
            "DirectWrite=yes\n",
            "WriteBuffer=0\n",
            "DiskSpace=250\n",
            "KeepHistory=30\n",
            "DownloadRate=1000\n",
            "ArticleRetries=3\n",
            "ArticleInterval=10\n",
            "ArticleTimeout=60\n",
            "CrcCheck=yes\n",
            "ParCheck=auto\n",
            "ParRepair=yes\n",
            "ParRename=yes\n",
            "Unpack=yes\n",
            "DirectUnpack=no\n",
            "UnpackCleanupDisk=yes\n",
            "UnrarCmd=unrar\n",
            "SevenZipCmd=7z\n",
            "WriteLog=append\n",
            "RotateLog=3\n",
            "InfoTarget=both\n",
            "WarningTarget=both\n",
            "ErrorTarget=both\n",
            "DetailTarget=log\n",
            "DebugTarget=none\n",
            "SystemHealthCheck=yes\n",
            "Task1.Time=08:00\n",
            "Task1.WeekDays=1,2,3,4,5\n",
            "Task1.Command=PauseDownload\n",
            "Task1.Param=\n",
            "Feed1.Name=MyFeed\n",
            "Feed1.URL=https://indexer.example.com/rss\n",
            "Feed1.Backlog=no\n",
            "Feed1.PauseNzb=no\n",
            "Feed1.Filter=size:>100MB\n",
            "Feed1.Interval=15\n",
            "Feed1.Category=TV\n",
            "Feed1.Priority=50\n",
            "Feed1.Extensions=ext1\n",
        )
    }

    fn state_with_full_config() -> (AppState, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_path = tmp.path().join("bergamot.conf");
        std::fs::write(&config_path, full_config_content()).expect("write");
        let raw = bergamot_config::parse_config(full_config_content()).expect("parse");
        let config = bergamot_config::Config::from_raw(raw);
        let config = std::sync::Arc::new(std::sync::RwLock::new(config));
        let state = AppState::default().with_config(config, config_path);
        (state, tmp)
    }

    fn find_entry<'a>(entries: &'a [serde_json::Value], name: &str) -> Option<&'a str> {
        entries
            .iter()
            .find(|e| e["Name"].as_str() == Some(name))
            .and_then(|e| e["Value"].as_str())
    }

    #[test]
    fn config_loadconfig_returns_all_path_options() {
        let (state, _tmp) = state_with_full_config();
        let result = rpc_loadconfig(&state).expect("loadconfig");
        let entries = result.as_array().expect("array");

        assert_eq!(find_entry(entries, "MainDir"), Some("/data"));
        assert_eq!(find_entry(entries, "DestDir"), Some("/data/completed"));
        assert_eq!(find_entry(entries, "InterDir"), Some("/data/intermediate"));
        assert_eq!(find_entry(entries, "NzbDir"), Some("/data/nzb"));
        assert_eq!(find_entry(entries, "QueueDir"), Some("/data/queue"));
        assert_eq!(find_entry(entries, "TempDir"), Some("/data/tmp"));
        assert_eq!(find_entry(entries, "ScriptDir"), Some("/data/scripts"));
        assert_eq!(find_entry(entries, "LogFile"), Some("/data/bergamot.log"));
    }

    #[test]
    fn config_loadconfig_returns_all_security_options() {
        let (state, _tmp) = state_with_full_config();
        let result = rpc_loadconfig(&state).expect("loadconfig");
        let entries = result.as_array().expect("array");

        assert_eq!(find_entry(entries, "ControlIP"), Some("127.0.0.1"));
        assert_eq!(find_entry(entries, "ControlPort"), Some("6789"));
        assert_eq!(find_entry(entries, "ControlUsername"), Some("admin"));
        assert_eq!(find_entry(entries, "ControlPassword"), Some("secret"));
        assert_eq!(find_entry(entries, "RestrictedUsername"), Some("viewer"));
        assert_eq!(find_entry(entries, "RestrictedPassword"), Some("viewpass"));
        assert_eq!(find_entry(entries, "AddUsername"), Some("adder"));
        assert_eq!(find_entry(entries, "AddPassword"), Some("addpass"));
        assert_eq!(find_entry(entries, "FormAuth"), Some("yes"));
        assert_eq!(find_entry(entries, "SecureControl"), Some("no"));
        assert_eq!(find_entry(entries, "SecurePort"), Some("6791"));
        assert_eq!(find_entry(entries, "SecureCert"), Some("/path/cert.pem"));
        assert_eq!(find_entry(entries, "SecureKey"), Some("/path/key.pem"));
        assert_eq!(find_entry(entries, "CertCheck"), Some("no"));
    }

    #[test]
    fn config_loadconfig_returns_all_server_options() {
        let (state, _tmp) = state_with_full_config();
        let result = rpc_loadconfig(&state).expect("loadconfig");
        let entries = result.as_array().expect("array");

        assert_eq!(find_entry(entries, "Server1.Active"), Some("yes"));
        assert_eq!(find_entry(entries, "Server1.Name"), Some("Primary"));
        assert_eq!(find_entry(entries, "Server1.Level"), Some("0"));
        assert_eq!(find_entry(entries, "Server1.Optional"), Some("no"));
        assert_eq!(find_entry(entries, "Server1.Group"), Some("0"));
        assert_eq!(
            find_entry(entries, "Server1.Host"),
            Some("news.example.com")
        );
        assert_eq!(find_entry(entries, "Server1.Port"), Some("563"));
        assert_eq!(find_entry(entries, "Server1.Username"), Some("user1"));
        assert_eq!(find_entry(entries, "Server1.Password"), Some("pass1"));
        assert_eq!(find_entry(entries, "Server1.JoinGroup"), Some("no"));
        assert_eq!(find_entry(entries, "Server1.Encryption"), Some("yes"));
        assert_eq!(find_entry(entries, "Server1.Cipher"), Some("AES256"));
        assert_eq!(find_entry(entries, "Server1.Connections"), Some("8"));
        assert_eq!(find_entry(entries, "Server1.Retention"), Some("1200"));
        assert_eq!(find_entry(entries, "Server1.IpVersion"), Some("auto"));
        assert_eq!(find_entry(entries, "Server1.Notes"), Some("primary server"));
        assert_eq!(
            find_entry(entries, "Server1.CertVerification"),
            Some("strict")
        );

        assert_eq!(find_entry(entries, "Server2.Active"), Some("no"));
        assert_eq!(find_entry(entries, "Server2.Name"), Some("Backup"));
        assert_eq!(find_entry(entries, "Server2.Level"), Some("1"));
        assert_eq!(find_entry(entries, "Server2.Optional"), Some("yes"));
        assert_eq!(find_entry(entries, "Server2.Group"), Some("1"));
        assert_eq!(
            find_entry(entries, "Server2.Host"),
            Some("backup.example.com")
        );
        assert_eq!(find_entry(entries, "Server2.Port"), Some("119"));
        assert_eq!(find_entry(entries, "Server2.Encryption"), Some("no"));
        assert_eq!(find_entry(entries, "Server2.Connections"), Some("4"));
        assert_eq!(find_entry(entries, "Server2.Retention"), Some("0"));
        assert_eq!(find_entry(entries, "Server2.IpVersion"), Some("ipv4"));
        assert_eq!(
            find_entry(entries, "Server2.CertVerification"),
            Some("none")
        );
    }

    #[test]
    fn config_loadconfig_returns_all_category_options() {
        let (state, _tmp) = state_with_full_config();
        let result = rpc_loadconfig(&state).expect("loadconfig");
        let entries = result.as_array().expect("array");

        assert_eq!(find_entry(entries, "Category1.Name"), Some("Movies"));
        assert_eq!(
            find_entry(entries, "Category1.DestDir"),
            Some("/data/movies")
        );
        assert_eq!(find_entry(entries, "Category1.Unpack"), Some("yes"));
        assert_eq!(find_entry(entries, "Category1.Extensions"), Some("ext1"));
        assert_eq!(
            find_entry(entries, "Category1.Aliases"),
            Some("Films, Cinema")
        );
        assert_eq!(find_entry(entries, "Category2.Name"), Some("TV"));
        assert_eq!(find_entry(entries, "Category2.DestDir"), Some("/data/tv"));
        assert_eq!(find_entry(entries, "Category2.Unpack"), Some("no"));
        assert_eq!(
            find_entry(entries, "Category2.Aliases"),
            Some("Television, Series")
        );
    }

    #[test]
    fn config_loadconfig_returns_all_download_options() {
        let (state, _tmp) = state_with_full_config();
        let result = rpc_loadconfig(&state).expect("loadconfig");
        let entries = result.as_array().expect("array");

        assert_eq!(find_entry(entries, "AppendCategoryDir"), Some("yes"));
        assert_eq!(find_entry(entries, "NzbDirInterval"), Some("5"));
        assert_eq!(find_entry(entries, "DupeCheck"), Some("yes"));
        assert_eq!(find_entry(entries, "ContinuePartial"), Some("yes"));
        assert_eq!(find_entry(entries, "ArticleCache"), Some("256"));
        assert_eq!(find_entry(entries, "DirectWrite"), Some("yes"));
        assert_eq!(find_entry(entries, "WriteBuffer"), Some("0"));
        assert_eq!(find_entry(entries, "DiskSpace"), Some("250"));
        assert_eq!(find_entry(entries, "KeepHistory"), Some("30"));
        assert_eq!(find_entry(entries, "DownloadRate"), Some("1000"));
    }

    #[test]
    fn config_loadconfig_returns_all_connection_options() {
        let (state, _tmp) = state_with_full_config();
        let result = rpc_loadconfig(&state).expect("loadconfig");
        let entries = result.as_array().expect("array");

        assert_eq!(find_entry(entries, "ArticleRetries"), Some("3"));
        assert_eq!(find_entry(entries, "ArticleInterval"), Some("10"));
        assert_eq!(find_entry(entries, "ArticleTimeout"), Some("60"));
    }

    #[test]
    fn config_loadconfig_returns_all_par_options() {
        let (state, _tmp) = state_with_full_config();
        let result = rpc_loadconfig(&state).expect("loadconfig");
        let entries = result.as_array().expect("array");

        assert_eq!(find_entry(entries, "CrcCheck"), Some("yes"));
        assert_eq!(find_entry(entries, "ParCheck"), Some("auto"));
        assert_eq!(find_entry(entries, "ParRepair"), Some("yes"));
        assert_eq!(find_entry(entries, "ParRename"), Some("yes"));
    }

    #[test]
    fn config_loadconfig_returns_all_unpack_options() {
        let (state, _tmp) = state_with_full_config();
        let result = rpc_loadconfig(&state).expect("loadconfig");
        let entries = result.as_array().expect("array");

        assert_eq!(find_entry(entries, "Unpack"), Some("yes"));
        assert_eq!(find_entry(entries, "DirectUnpack"), Some("no"));
        assert_eq!(find_entry(entries, "UnpackCleanupDisk"), Some("yes"));
        assert_eq!(find_entry(entries, "UnrarCmd"), Some("unrar"));
        assert_eq!(find_entry(entries, "SevenZipCmd"), Some("7z"));
    }

    #[test]
    fn config_loadconfig_returns_all_logging_options() {
        let (state, _tmp) = state_with_full_config();
        let result = rpc_loadconfig(&state).expect("loadconfig");
        let entries = result.as_array().expect("array");

        assert_eq!(find_entry(entries, "WriteLog"), Some("append"));
        assert_eq!(find_entry(entries, "RotateLog"), Some("3"));
        assert_eq!(find_entry(entries, "InfoTarget"), Some("both"));
        assert_eq!(find_entry(entries, "WarningTarget"), Some("both"));
        assert_eq!(find_entry(entries, "ErrorTarget"), Some("both"));
        assert_eq!(find_entry(entries, "DetailTarget"), Some("log"));
        assert_eq!(find_entry(entries, "DebugTarget"), Some("none"));
        assert_eq!(find_entry(entries, "SystemHealthCheck"), Some("yes"));
    }

    #[test]
    fn config_loadconfig_returns_all_scheduler_options() {
        let (state, _tmp) = state_with_full_config();
        let result = rpc_loadconfig(&state).expect("loadconfig");
        let entries = result.as_array().expect("array");

        assert_eq!(find_entry(entries, "Task1.Time"), Some("08:00"));
        assert_eq!(find_entry(entries, "Task1.WeekDays"), Some("1,2,3,4,5"));
        assert_eq!(find_entry(entries, "Task1.Command"), Some("PauseDownload"));
    }

    #[test]
    fn config_loadconfig_returns_all_feed_options() {
        let (state, _tmp) = state_with_full_config();
        let result = rpc_loadconfig(&state).expect("loadconfig");
        let entries = result.as_array().expect("array");

        assert_eq!(find_entry(entries, "Feed1.Name"), Some("MyFeed"));
        assert_eq!(
            find_entry(entries, "Feed1.URL"),
            Some("https://indexer.example.com/rss")
        );
        assert_eq!(find_entry(entries, "Feed1.Backlog"), Some("no"));
        assert_eq!(find_entry(entries, "Feed1.PauseNzb"), Some("no"));
        assert_eq!(find_entry(entries, "Feed1.Filter"), Some("size:>100MB"));
        assert_eq!(find_entry(entries, "Feed1.Interval"), Some("15"));
        assert_eq!(find_entry(entries, "Feed1.Category"), Some("TV"));
        assert_eq!(find_entry(entries, "Feed1.Priority"), Some("50"));
        assert_eq!(find_entry(entries, "Feed1.Extensions"), Some("ext1"));
    }

    #[test]
    fn config_loadconfig_returns_extension_options() {
        let (state, _tmp) = state_with_full_config();
        let result = rpc_loadconfig(&state).expect("loadconfig");
        let entries = result.as_array().expect("array");

        assert_eq!(find_entry(entries, "Extensions"), Some("ext1, ext2"));
        assert_eq!(find_entry(entries, "ScriptOrder"), Some("ext2, ext1"));
    }

    #[test]
    fn saveconfig_roundtrip_all_path_options() {
        let (state, tmp) = state_with_full_config();
        let save_params = serde_json::json!([[
            {"Name": "MainDir", "Value": "/new"},
            {"Name": "DestDir", "Value": "/new/done"},
            {"Name": "InterDir", "Value": "/new/inter"},
            {"Name": "NzbDir", "Value": "/new/nzb"},
            {"Name": "QueueDir", "Value": "/new/queue"},
            {"Name": "TempDir", "Value": "/new/tmp"},
            {"Name": "ScriptDir", "Value": "/new/scripts"},
            {"Name": "LogFile", "Value": "/new/bergamot.log"},
        ]]);
        let result = rpc_saveconfig(save_params.as_array().unwrap().first().unwrap(), &state)
            .expect("saveconfig");
        assert_eq!(result, serde_json::json!(true));

        let saved = std::fs::read_to_string(tmp.path().join("bergamot.conf")).expect("read");
        assert!(saved.contains("MainDir=/new\n"));
        assert!(saved.contains("DestDir=/new/done\n"));
        assert!(saved.contains("InterDir=/new/inter\n"));
        assert!(saved.contains("NzbDir=/new/nzb\n"));
        assert!(saved.contains("QueueDir=/new/queue\n"));
        assert!(saved.contains("TempDir=/new/tmp\n"));
        assert!(saved.contains("ScriptDir=/new/scripts\n"));
        assert!(saved.contains("LogFile=/new/bergamot.log\n"));

        let reload_result = rpc_loadconfig(&state).expect("loadconfig after save");
        let entries = reload_result.as_array().expect("array");
        assert_eq!(find_entry(entries, "MainDir"), Some("/new"));
        assert_eq!(find_entry(entries, "DestDir"), Some("/new/done"));
        assert_eq!(find_entry(entries, "InterDir"), Some("/new/inter"));
        assert_eq!(find_entry(entries, "NzbDir"), Some("/new/nzb"));
        assert_eq!(find_entry(entries, "QueueDir"), Some("/new/queue"));
        assert_eq!(find_entry(entries, "TempDir"), Some("/new/tmp"));
        assert_eq!(find_entry(entries, "ScriptDir"), Some("/new/scripts"));
        assert_eq!(find_entry(entries, "LogFile"), Some("/new/bergamot.log"));
    }

    #[test]
    fn saveconfig_roundtrip_all_security_options() {
        let (state, tmp) = state_with_full_config();
        let params = serde_json::json!([
            {"Name": "ControlIP", "Value": "0.0.0.0"},
            {"Name": "ControlPort", "Value": "7890"},
            {"Name": "ControlUsername", "Value": "newadmin"},
            {"Name": "ControlPassword", "Value": "newpass"},
            {"Name": "RestrictedUsername", "Value": "restricted"},
            {"Name": "RestrictedPassword", "Value": "rpass"},
            {"Name": "AddUsername", "Value": "newadder"},
            {"Name": "AddPassword", "Value": "apass"},
            {"Name": "FormAuth", "Value": "no"},
            {"Name": "SecureControl", "Value": "yes"},
            {"Name": "SecurePort", "Value": "6792"},
            {"Name": "SecureCert", "Value": "/new/cert.pem"},
            {"Name": "SecureKey", "Value": "/new/key.pem"},
            {"Name": "CertCheck", "Value": "yes"},
        ]);
        let result = rpc_saveconfig(&params, &state).expect("saveconfig");
        assert_eq!(result, serde_json::json!(true));

        let saved = std::fs::read_to_string(tmp.path().join("bergamot.conf")).expect("read");
        assert!(saved.contains("ControlIP=0.0.0.0\n"));
        assert!(saved.contains("ControlPort=7890\n"));
        assert!(saved.contains("ControlUsername=newadmin\n"));
        assert!(saved.contains("ControlPassword=newpass\n"));
        assert!(saved.contains("FormAuth=no\n"));
        assert!(saved.contains("SecureControl=yes\n"));
        assert!(saved.contains("SecurePort=6792\n"));
        assert!(saved.contains("CertCheck=yes\n"));

        let reload_result = rpc_loadconfig(&state).expect("loadconfig after save");
        let entries = reload_result.as_array().expect("array");
        assert_eq!(find_entry(entries, "ControlIP"), Some("0.0.0.0"));
        assert_eq!(find_entry(entries, "ControlPort"), Some("7890"));
        assert_eq!(find_entry(entries, "ControlUsername"), Some("newadmin"));
        assert_eq!(find_entry(entries, "ControlPassword"), Some("newpass"));
        assert_eq!(find_entry(entries, "FormAuth"), Some("no"));
        assert_eq!(find_entry(entries, "SecureControl"), Some("yes"));
        assert_eq!(find_entry(entries, "SecurePort"), Some("6792"));
        assert_eq!(find_entry(entries, "CertCheck"), Some("yes"));
    }

    #[test]
    fn saveconfig_roundtrip_all_server_options() {
        let (state, tmp) = state_with_full_config();
        let params = serde_json::json!([
            {"Name": "Server1.Active", "Value": "no"},
            {"Name": "Server1.Name", "Value": "NewPrimary"},
            {"Name": "Server1.Level", "Value": "2"},
            {"Name": "Server1.Optional", "Value": "yes"},
            {"Name": "Server1.Group", "Value": "5"},
            {"Name": "Server1.Host", "Value": "new.example.com"},
            {"Name": "Server1.Port", "Value": "443"},
            {"Name": "Server1.Username", "Value": "newuser"},
            {"Name": "Server1.Password", "Value": "newpass"},
            {"Name": "Server1.JoinGroup", "Value": "yes"},
            {"Name": "Server1.Encryption", "Value": "no"},
            {"Name": "Server1.Cipher", "Value": "RC4"},
            {"Name": "Server1.Connections", "Value": "16"},
            {"Name": "Server1.Retention", "Value": "3000"},
            {"Name": "Server1.IpVersion", "Value": "ipv6"},
            {"Name": "Server1.Notes", "Value": "updated notes"},
            {"Name": "Server1.CertVerification", "Value": "none"},
        ]);
        let result = rpc_saveconfig(&params, &state).expect("saveconfig");
        assert_eq!(result, serde_json::json!(true));

        let saved = std::fs::read_to_string(tmp.path().join("bergamot.conf")).expect("read");
        assert!(saved.contains("Server1.Active=no\n"));
        assert!(saved.contains("Server1.Name=NewPrimary\n"));
        assert!(saved.contains("Server1.Host=new.example.com\n"));
        assert!(saved.contains("Server1.Port=443\n"));
        assert!(saved.contains("Server1.Connections=16\n"));
        assert!(saved.contains("Server1.Retention=3000\n"));
        assert!(saved.contains("Server1.IpVersion=ipv6\n"));

        let reload_result = rpc_loadconfig(&state).expect("loadconfig after save");
        let entries = reload_result.as_array().expect("array");
        assert_eq!(find_entry(entries, "Server1.Active"), Some("no"));
        assert_eq!(find_entry(entries, "Server1.Name"), Some("NewPrimary"));
        assert_eq!(find_entry(entries, "Server1.Level"), Some("2"));
        assert_eq!(find_entry(entries, "Server1.Optional"), Some("yes"));
        assert_eq!(find_entry(entries, "Server1.Group"), Some("5"));
        assert_eq!(find_entry(entries, "Server1.Host"), Some("new.example.com"));
        assert_eq!(find_entry(entries, "Server1.Port"), Some("443"));
        assert_eq!(find_entry(entries, "Server1.Username"), Some("newuser"));
        assert_eq!(find_entry(entries, "Server1.Password"), Some("newpass"));
        assert_eq!(find_entry(entries, "Server1.JoinGroup"), Some("yes"));
        assert_eq!(find_entry(entries, "Server1.Encryption"), Some("no"));
        assert_eq!(find_entry(entries, "Server1.Cipher"), Some("RC4"));
        assert_eq!(find_entry(entries, "Server1.Connections"), Some("16"));
        assert_eq!(find_entry(entries, "Server1.Retention"), Some("3000"));
        assert_eq!(find_entry(entries, "Server1.IpVersion"), Some("ipv6"));
        assert_eq!(find_entry(entries, "Server1.Notes"), Some("updated notes"));
        assert_eq!(
            find_entry(entries, "Server1.CertVerification"),
            Some("none")
        );
    }

    #[test]
    fn saveconfig_roundtrip_all_category_options() {
        let (state, tmp) = state_with_full_config();
        let params = serde_json::json!([
            {"Name": "Category1.Name", "Value": "Films"},
            {"Name": "Category1.DestDir", "Value": "/new/films"},
            {"Name": "Category1.Unpack", "Value": "no"},
            {"Name": "Category1.Extensions", "Value": "ext2, ext3"},
            {"Name": "Category1.Aliases", "Value": "Movie, Flick"},
        ]);
        let result = rpc_saveconfig(&params, &state).expect("saveconfig");
        assert_eq!(result, serde_json::json!(true));

        let saved = std::fs::read_to_string(tmp.path().join("bergamot.conf")).expect("read");
        assert!(saved.contains("Category1.Name=Films\n"));
        assert!(saved.contains("Category1.DestDir=/new/films\n"));
        assert!(saved.contains("Category1.Unpack=no\n"));

        let reload_result = rpc_loadconfig(&state).expect("loadconfig after save");
        let entries = reload_result.as_array().expect("array");
        assert_eq!(find_entry(entries, "Category1.Name"), Some("Films"));
        assert_eq!(find_entry(entries, "Category1.DestDir"), Some("/new/films"));
        assert_eq!(find_entry(entries, "Category1.Unpack"), Some("no"));
        assert_eq!(
            find_entry(entries, "Category1.Extensions"),
            Some("ext2, ext3")
        );
        assert_eq!(
            find_entry(entries, "Category1.Aliases"),
            Some("Movie, Flick")
        );
    }

    #[test]
    fn saveconfig_roundtrip_all_download_options() {
        let (state, tmp) = state_with_full_config();
        let params = serde_json::json!([
            {"Name": "AppendCategoryDir", "Value": "no"},
            {"Name": "NzbDirInterval", "Value": "10"},
            {"Name": "DupeCheck", "Value": "no"},
            {"Name": "ContinuePartial", "Value": "no"},
            {"Name": "ArticleCache", "Value": "512"},
            {"Name": "DirectWrite", "Value": "no"},
            {"Name": "WriteBuffer", "Value": "4096"},
            {"Name": "DiskSpace", "Value": "500"},
            {"Name": "KeepHistory", "Value": "60"},
            {"Name": "DownloadRate", "Value": "2000"},
        ]);
        let result = rpc_saveconfig(&params, &state).expect("saveconfig");
        assert_eq!(result, serde_json::json!(true));

        let saved = std::fs::read_to_string(tmp.path().join("bergamot.conf")).expect("read");
        assert!(saved.contains("AppendCategoryDir=no\n"));
        assert!(saved.contains("ArticleCache=512\n"));
        assert!(saved.contains("DiskSpace=500\n"));
        assert!(saved.contains("DownloadRate=2000\n"));
        assert!(saved.contains("KeepHistory=60\n"));

        let reload_result = rpc_loadconfig(&state).expect("loadconfig after save");
        let entries = reload_result.as_array().expect("array");
        assert_eq!(find_entry(entries, "AppendCategoryDir"), Some("no"));
        assert_eq!(find_entry(entries, "NzbDirInterval"), Some("10"));
        assert_eq!(find_entry(entries, "DupeCheck"), Some("no"));
        assert_eq!(find_entry(entries, "ContinuePartial"), Some("no"));
        assert_eq!(find_entry(entries, "ArticleCache"), Some("512"));
        assert_eq!(find_entry(entries, "DirectWrite"), Some("no"));
        assert_eq!(find_entry(entries, "WriteBuffer"), Some("4096"));
        assert_eq!(find_entry(entries, "DiskSpace"), Some("500"));
        assert_eq!(find_entry(entries, "KeepHistory"), Some("60"));
        assert_eq!(find_entry(entries, "DownloadRate"), Some("2000"));
    }

    #[test]
    fn saveconfig_roundtrip_all_connection_options() {
        let (state, tmp) = state_with_full_config();
        let params = serde_json::json!([
            {"Name": "ArticleRetries", "Value": "5"},
            {"Name": "ArticleInterval", "Value": "20"},
            {"Name": "ArticleTimeout", "Value": "120"},
        ]);
        let result = rpc_saveconfig(&params, &state).expect("saveconfig");
        assert_eq!(result, serde_json::json!(true));

        let saved = std::fs::read_to_string(tmp.path().join("bergamot.conf")).expect("read");
        assert!(saved.contains("ArticleRetries=5\n"));
        assert!(saved.contains("ArticleInterval=20\n"));
        assert!(saved.contains("ArticleTimeout=120\n"));

        let reload_result = rpc_loadconfig(&state).expect("loadconfig after save");
        let entries = reload_result.as_array().expect("array");
        assert_eq!(find_entry(entries, "ArticleRetries"), Some("5"));
        assert_eq!(find_entry(entries, "ArticleInterval"), Some("20"));
        assert_eq!(find_entry(entries, "ArticleTimeout"), Some("120"));
    }

    #[test]
    fn saveconfig_roundtrip_all_par_options() {
        let (state, tmp) = state_with_full_config();
        let params = serde_json::json!([
            {"Name": "CrcCheck", "Value": "no"},
            {"Name": "ParCheck", "Value": "always"},
            {"Name": "ParRepair", "Value": "no"},
            {"Name": "ParRename", "Value": "no"},
        ]);
        let result = rpc_saveconfig(&params, &state).expect("saveconfig");
        assert_eq!(result, serde_json::json!(true));

        let saved = std::fs::read_to_string(tmp.path().join("bergamot.conf")).expect("read");
        assert!(saved.contains("CrcCheck=no\n"));
        assert!(saved.contains("ParCheck=always\n"));
        assert!(saved.contains("ParRepair=no\n"));
        assert!(saved.contains("ParRename=no\n"));

        let reload_result = rpc_loadconfig(&state).expect("loadconfig after save");
        let entries = reload_result.as_array().expect("array");
        assert_eq!(find_entry(entries, "CrcCheck"), Some("no"));
        assert_eq!(find_entry(entries, "ParCheck"), Some("always"));
        assert_eq!(find_entry(entries, "ParRepair"), Some("no"));
        assert_eq!(find_entry(entries, "ParRename"), Some("no"));
    }

    #[test]
    fn saveconfig_roundtrip_all_unpack_options() {
        let (state, tmp) = state_with_full_config();
        let params = serde_json::json!([
            {"Name": "Unpack", "Value": "no"},
            {"Name": "DirectUnpack", "Value": "yes"},
            {"Name": "UnpackCleanupDisk", "Value": "no"},
            {"Name": "UnrarCmd", "Value": "/usr/bin/unrar"},
            {"Name": "SevenZipCmd", "Value": "/usr/bin/7z"},
        ]);
        let result = rpc_saveconfig(&params, &state).expect("saveconfig");
        assert_eq!(result, serde_json::json!(true));

        let saved = std::fs::read_to_string(tmp.path().join("bergamot.conf")).expect("read");
        assert!(saved.contains("Unpack=no\n"));
        assert!(saved.contains("DirectUnpack=yes\n"));
        assert!(saved.contains("UnpackCleanupDisk=no\n"));
        assert!(saved.contains("UnrarCmd=/usr/bin/unrar\n"));
        assert!(saved.contains("SevenZipCmd=/usr/bin/7z\n"));

        let reload_result = rpc_loadconfig(&state).expect("loadconfig after save");
        let entries = reload_result.as_array().expect("array");
        assert_eq!(find_entry(entries, "Unpack"), Some("no"));
        assert_eq!(find_entry(entries, "DirectUnpack"), Some("yes"));
        assert_eq!(find_entry(entries, "UnpackCleanupDisk"), Some("no"));
        assert_eq!(find_entry(entries, "UnrarCmd"), Some("/usr/bin/unrar"));
        assert_eq!(find_entry(entries, "SevenZipCmd"), Some("/usr/bin/7z"));
    }

    #[test]
    fn saveconfig_roundtrip_all_logging_options() {
        let (state, tmp) = state_with_full_config();
        let params = serde_json::json!([
            {"Name": "WriteLog", "Value": "reset"},
            {"Name": "RotateLog", "Value": "5"},
            {"Name": "InfoTarget", "Value": "screen"},
            {"Name": "WarningTarget", "Value": "log"},
            {"Name": "ErrorTarget", "Value": "none"},
            {"Name": "DetailTarget", "Value": "both"},
            {"Name": "DebugTarget", "Value": "log"},
            {"Name": "SystemHealthCheck", "Value": "no"},
        ]);
        let result = rpc_saveconfig(&params, &state).expect("saveconfig");
        assert_eq!(result, serde_json::json!(true));

        let saved = std::fs::read_to_string(tmp.path().join("bergamot.conf")).expect("read");
        assert!(saved.contains("WriteLog=reset\n"));
        assert!(saved.contains("RotateLog=5\n"));
        assert!(saved.contains("InfoTarget=screen\n"));
        assert!(saved.contains("ErrorTarget=none\n"));

        let reload_result = rpc_loadconfig(&state).expect("loadconfig after save");
        let entries = reload_result.as_array().expect("array");
        assert_eq!(find_entry(entries, "WriteLog"), Some("reset"));
        assert_eq!(find_entry(entries, "RotateLog"), Some("5"));
        assert_eq!(find_entry(entries, "InfoTarget"), Some("screen"));
        assert_eq!(find_entry(entries, "WarningTarget"), Some("log"));
        assert_eq!(find_entry(entries, "ErrorTarget"), Some("none"));
        assert_eq!(find_entry(entries, "DetailTarget"), Some("both"));
        assert_eq!(find_entry(entries, "DebugTarget"), Some("log"));
        assert_eq!(find_entry(entries, "SystemHealthCheck"), Some("no"));
    }

    #[test]
    fn saveconfig_roundtrip_all_scheduler_options() {
        let (state, tmp) = state_with_full_config();
        let params = serde_json::json!([
            {"Name": "Task1.Time", "Value": "22:30"},
            {"Name": "Task1.WeekDays", "Value": "6,7"},
            {"Name": "Task1.Command", "Value": "UnpauseDownload"},
            {"Name": "Task1.Param", "Value": "1000"},
        ]);
        let result = rpc_saveconfig(&params, &state).expect("saveconfig");
        assert_eq!(result, serde_json::json!(true));

        let saved = std::fs::read_to_string(tmp.path().join("bergamot.conf")).expect("read");
        assert!(saved.contains("Task1.Time=22:30\n"));
        assert!(saved.contains("Task1.WeekDays=6,7\n"));
        assert!(saved.contains("Task1.Command=UnpauseDownload\n"));
        assert!(saved.contains("Task1.Param=1000\n"));

        let reload_result = rpc_loadconfig(&state).expect("loadconfig after save");
        let entries = reload_result.as_array().expect("array");
        assert_eq!(find_entry(entries, "Task1.Time"), Some("22:30"));
        assert_eq!(find_entry(entries, "Task1.WeekDays"), Some("6,7"));
        assert_eq!(
            find_entry(entries, "Task1.Command"),
            Some("UnpauseDownload")
        );
        assert_eq!(find_entry(entries, "Task1.Param"), Some("1000"));
    }

    #[test]
    fn saveconfig_roundtrip_all_feed_options() {
        let (state, tmp) = state_with_full_config();
        let params = serde_json::json!([
            {"Name": "Feed1.Name", "Value": "NewFeed"},
            {"Name": "Feed1.URL", "Value": "https://new.example.com/rss"},
            {"Name": "Feed1.Backlog", "Value": "yes"},
            {"Name": "Feed1.PauseNzb", "Value": "yes"},
            {"Name": "Feed1.Filter", "Value": "title:.*Linux.*"},
            {"Name": "Feed1.Interval", "Value": "30"},
            {"Name": "Feed1.Category", "Value": "Software"},
            {"Name": "Feed1.Priority", "Value": "100"},
            {"Name": "Feed1.Extensions", "Value": "ext2, ext3"},
        ]);
        let result = rpc_saveconfig(&params, &state).expect("saveconfig");
        assert_eq!(result, serde_json::json!(true));

        let saved = std::fs::read_to_string(tmp.path().join("bergamot.conf")).expect("read");
        assert!(saved.contains("Feed1.Name=NewFeed\n"));
        assert!(saved.contains("Feed1.URL=https://new.example.com/rss\n"));
        assert!(saved.contains("Feed1.Interval=30\n"));

        let reload_result = rpc_loadconfig(&state).expect("loadconfig after save");
        let entries = reload_result.as_array().expect("array");
        assert_eq!(find_entry(entries, "Feed1.Name"), Some("NewFeed"));
        assert_eq!(
            find_entry(entries, "Feed1.URL"),
            Some("https://new.example.com/rss")
        );
        assert_eq!(find_entry(entries, "Feed1.Backlog"), Some("yes"));
        assert_eq!(find_entry(entries, "Feed1.PauseNzb"), Some("yes"));
        assert_eq!(find_entry(entries, "Feed1.Filter"), Some("title:.*Linux.*"));
        assert_eq!(find_entry(entries, "Feed1.Interval"), Some("30"));
        assert_eq!(find_entry(entries, "Feed1.Category"), Some("Software"));
        assert_eq!(find_entry(entries, "Feed1.Priority"), Some("100"));
        assert_eq!(find_entry(entries, "Feed1.Extensions"), Some("ext2, ext3"));
    }

    #[test]
    fn saveconfig_roundtrip_extension_options() {
        let (state, tmp) = state_with_full_config();
        let params = serde_json::json!([
            {"Name": "Extensions", "Value": "ext3, ext4"},
            {"Name": "ScriptOrder", "Value": "ext4, ext3"},
        ]);
        let result = rpc_saveconfig(&params, &state).expect("saveconfig");
        assert_eq!(result, serde_json::json!(true));

        let saved = std::fs::read_to_string(tmp.path().join("bergamot.conf")).expect("read");
        assert!(saved.contains("Extensions=ext3, ext4\n"));
        assert!(saved.contains("ScriptOrder=ext4, ext3\n"));

        let reload_result = rpc_loadconfig(&state).expect("loadconfig after save");
        let entries = reload_result.as_array().expect("array");
        assert_eq!(find_entry(entries, "Extensions"), Some("ext3, ext4"));
        assert_eq!(find_entry(entries, "ScriptOrder"), Some("ext4, ext3"));
    }

    #[test]
    fn saveconfig_reload_reflects_saved_structural_changes() {
        let (state, _tmp) = state_with_full_config();
        let params = serde_json::json!([
            {"Name": "DownloadRate", "Value": "5000"},
            {"Name": "DiskSpace", "Value": "100"},
        ]);
        rpc_saveconfig(&params, &state).expect("saveconfig");

        rpc_reload(&state).expect("reload");

        let config = state.config().unwrap().read().unwrap();
        assert_eq!(config.download_rate, 5000);
        assert_eq!(config.disk_space, 100);
    }

    #[test]
    fn saveconfig_reload_reflects_server_changes() {
        let (state, _tmp) = state_with_full_config();
        let params = serde_json::json!([
            {"Name": "Server1.Host", "Value": "changed.example.com"},
            {"Name": "Server1.Port", "Value": "443"},
            {"Name": "Server1.Connections", "Value": "20"},
        ]);
        rpc_saveconfig(&params, &state).expect("saveconfig");

        rpc_reload(&state).expect("reload");

        let config = state.config().unwrap().read().unwrap();
        assert_eq!(config.servers[0].host, "changed.example.com");
        assert_eq!(config.servers[0].port, 443);
        assert_eq!(config.servers[0].connections, 20);
    }

    #[test]
    fn saveconfig_reload_reflects_category_changes() {
        let (state, _tmp) = state_with_full_config();
        let params = serde_json::json!([
            {"Name": "Category1.Name", "Value": "Anime"},
            {"Name": "Category1.DestDir", "Value": "/data/anime"},
        ]);
        rpc_saveconfig(&params, &state).expect("saveconfig");

        rpc_reload(&state).expect("reload");

        let config = state.config().unwrap().read().unwrap();
        assert_eq!(config.categories[0].name, "Anime");
        assert_eq!(
            config.categories[0].dest_dir,
            std::path::PathBuf::from("/data/anime")
        );
    }

    #[test]
    fn saveconfig_reload_reflects_boolean_option_changes() {
        let (state, _tmp) = state_with_full_config();
        let params = serde_json::json!([
            {"Name": "AppendCategoryDir", "Value": "no"},
            {"Name": "UnpackCleanupDisk", "Value": "no"},
            {"Name": "FormAuth", "Value": "no"},
            {"Name": "SecureControl", "Value": "yes"},
        ]);
        rpc_saveconfig(&params, &state).expect("saveconfig");

        rpc_reload(&state).expect("reload");

        let config = state.config().unwrap().read().unwrap();
        assert!(!config.append_category_dir);
        assert!(!config.unpack_cleanup_disk);
        assert!(!config.form_auth);
        assert!(config.secure_control);
    }

    #[test]
    fn saveconfig_adds_new_server() {
        let (state, _tmp) = state_with_full_config();
        let params = serde_json::json!([
            {"Name": "Server3.Active", "Value": "yes"},
            {"Name": "Server3.Host", "Value": "third.example.com"},
            {"Name": "Server3.Port", "Value": "563"},
            {"Name": "Server3.Connections", "Value": "2"},
            {"Name": "Server3.Level", "Value": "2"},
        ]);
        rpc_saveconfig(&params, &state).expect("saveconfig");

        rpc_reload(&state).expect("reload");

        let config = state.config().unwrap().read().unwrap();
        assert_eq!(config.servers.len(), 3);
        assert_eq!(config.servers[2].host, "third.example.com");
        assert_eq!(config.servers[2].level, 2);
    }

    #[test]
    fn saveconfig_adds_new_category() {
        let (state, _tmp) = state_with_full_config();
        let params = serde_json::json!([
            {"Name": "Category3.Name", "Value": "Music"},
            {"Name": "Category3.DestDir", "Value": "/data/music"},
            {"Name": "Category3.Unpack", "Value": "no"},
        ]);
        rpc_saveconfig(&params, &state).expect("saveconfig");

        rpc_reload(&state).expect("reload");

        let config = state.config().unwrap().read().unwrap();
        assert_eq!(config.categories.len(), 3);
        assert_eq!(config.categories[2].name, "Music");
        assert!(!config.categories[2].unpack);
    }

    #[test]
    fn configtemplates_contains_all_option_sections() {
        let result = rpc_configtemplates().expect("configtemplates");
        let entries = result.as_array().expect("array");
        let template = entries[0]["Template"].as_str().expect("template string");

        let expected_options = [
            "MainDir",
            "DestDir",
            "InterDir",
            "NzbDir",
            "QueueDir",
            "TempDir",
            "ScriptDir",
            "Extensions",
            "ScriptOrder",
            "LogFile",
            "ControlIP",
            "ControlPort",
            "ControlUsername",
            "ControlPassword",
            "RestrictedUsername",
            "RestrictedPassword",
            "AddUsername",
            "AddPassword",
            "FormAuth",
            "SecureControl",
            "SecurePort",
            "SecureCert",
            "SecureKey",
            "CertCheck",
            "Server1.Active",
            "Server1.Name",
            "Server1.Level",
            "Server1.Optional",
            "Server1.Group",
            "Server1.Host",
            "Server1.Port",
            "Server1.Username",
            "Server1.Password",
            "Server1.JoinGroup",
            "Server1.Encryption",
            "Server1.Cipher",
            "Server1.Connections",
            "Server1.Retention",
            "Server1.IpVersion",
            "Server1.Notes",
            "Server1.CertVerification",
            "Category1.Name",
            "Category1.DestDir",
            "Category1.Unpack",
            "Category1.Extensions",
            "Category1.Aliases",
            "AppendCategoryDir",
            "NzbDirInterval",
            "DupeCheck",
            "ContinuePartial",
            "ArticleCache",
            "DirectWrite",
            "WriteBuffer",
            "DiskSpace",
            "KeepHistory",
            "DownloadRate",
            "ArticleRetries",
            "ArticleInterval",
            "ArticleTimeout",
            "CrcCheck",
            "ParCheck",
            "ParRepair",
            "ParRename",
            "Unpack",
            "DirectUnpack",
            "UnpackCleanupDisk",
            "UnrarCmd",
            "SevenZipCmd",
            "WriteLog",
            "RotateLog",
            "InfoTarget",
            "WarningTarget",
            "ErrorTarget",
            "DetailTarget",
            "DebugTarget",
            "SystemHealthCheck",
            "Task1.Time",
            "Task1.WeekDays",
            "Task1.Command",
            "Task1.Param",
            "Feed1.Name",
            "Feed1.URL",
            "Feed1.Backlog",
            "Feed1.PauseNzb",
            "Feed1.Filter",
            "Feed1.Interval",
            "Feed1.Category",
            "Feed1.Priority",
            "Feed1.Extensions",
        ];

        for option in &expected_options {
            assert!(
                template.contains(option),
                "configtemplates should contain option: {option}"
            );
        }
    }

    #[test]
    fn config_and_loadconfig_return_identical_results() {
        let (state, _tmp) = state_with_full_config();
        let config_result = rpc_loadconfig(&state).expect("config");
        let loadconfig_result = rpc_loadconfig(&state).expect("loadconfig");
        assert_eq!(config_result, loadconfig_result);
    }

    #[test]
    fn saveconfig_no_config_returns_error() {
        let state = AppState::default();
        let params = serde_json::json!([{"Name": "DownloadRate", "Value": "100"}]);
        let result = rpc_saveconfig(&params, &state);
        assert!(result.is_err());
    }

    #[test]
    fn loadconfig_no_config_returns_error() {
        let state = AppState::default();
        let result = rpc_loadconfig(&state);
        assert!(result.is_err());
    }
}
