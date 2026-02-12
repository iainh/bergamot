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
        .add_nzb(nzb_path, category, priority)
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
            let file_size_mb = entry.total_size / (1024 * 1024);
            let file_size_lo = (entry.total_size & 0xFFFF_FFFF) as u32;
            let remaining_size_mb = remaining_size / (1024 * 1024);
            let remaining_size_lo = (remaining_size & 0xFFFF_FFFF) as u32;

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
                "NZBName": entry.name,
                "Status": status,
                "Category": entry.category,
                "FileSizeMB": file_size_mb,
                "FileSizeLo": file_size_lo,
                "RemainingSizeMB": remaining_size_mb,
                "RemainingSizeLo": remaining_size_lo,
                "PausedSizeMB": 0,
                "PausedSizeLo": 0,
                "MaxPriority": entry.priority as i32,
                "MinPriority": entry.priority as i32,
                "DupeKey": entry.dupe_key,
                "DupeScore": entry.dupe_score,
                "DupeMode": entry.dupe_mode as u32,
                "MinPostTime": 0,
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
                "PostStageProgress": entry.post_stage_progress,
                "PostStageTimeSec": entry.post_stage_time_sec,
                "PostTotalTimeSec": entry.post_total_time_sec,
                "PostInfoText": entry.post_info_text,
                "ServerStats": server_stats,
                "Parameters": parameters,
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

fn parse_edit_command(command: &str) -> Result<bergamot_queue::EditAction, JsonRpcError> {
    match command {
        "GroupMoveTop" => Ok(bergamot_queue::EditAction::Move(
            bergamot_queue::MovePosition::Top,
        )),
        "GroupMoveBottom" => Ok(bergamot_queue::EditAction::Move(
            bergamot_queue::MovePosition::Bottom,
        )),
        "GroupPause" => Ok(bergamot_queue::EditAction::Pause),
        "GroupResume" => Ok(bergamot_queue::EditAction::Resume),
        "GroupDelete" | "GroupDupeDelete" | "GroupFinalDelete" => {
            Ok(bergamot_queue::EditAction::Delete { delete_files: true })
        }
        "GroupParkDelete" => Ok(bergamot_queue::EditAction::Delete {
            delete_files: false,
        }),
        "GroupPauseAllPars" | "GroupPauseExtraPars" => Ok(bergamot_queue::EditAction::Pause),
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

pub(crate) fn rpc_postqueue(state: &AppState) -> Result<serde_json::Value, JsonRpcError> {
    let paused = state
        .postproc_paused()
        .load(std::sync::atomic::Ordering::Relaxed);
    Ok(serde_json::json!({
        "Paused": paused,
        "Jobs": [],
    }))
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
    fn dispatch_postqueue_returns_paused_status() {
        let state = AppState::default();
        let result = rpc_postqueue(&state).expect("postqueue");
        assert_eq!(result["Paused"], false);
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

        let pq = rpc_postqueue(&state).expect("postqueue");
        assert_eq!(pq["Paused"], true);
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
}
