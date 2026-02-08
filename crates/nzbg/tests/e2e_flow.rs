use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Once};
use std::time::Duration;

use base64::Engine;
use nzbg::app::{init_tracing, run_with_config_path};
use nzbg::download::NntpPoolFetcher;
use nzbg_config::Config;
use nzbg_nntp::{Encryption, NewsServer, ServerPool};
use nzbg_nntp_stub::{StubConfig, StubServer, load_fixtures};
use tokio::task::JoinHandle;
use tokio::time::timeout;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fixtures")
        .join("nntp")
}

fn fixtures_complete_path() -> PathBuf {
    fixtures_dir().join("fixtures-complete.json")
}

fn fixtures_basic_path() -> PathBuf {
    fixtures_dir().join("fixtures-basic.json")
}

fn fixtures_multiseg_path() -> PathBuf {
    fixtures_dir().join("fixtures-multiseg.json")
}

fn sample_nzb_path() -> PathBuf {
    fixtures_dir().join("sample.nzb")
}

fn multi_nzb_path() -> PathBuf {
    fixtures_dir().join("multi.nzb")
}

fn available_port() -> u16 {
    let socket = std::net::TcpListener::bind("127.0.0.1:0").expect("bind port");
    socket.local_addr().expect("local addr").port()
}

async fn start_stub(port: u16, fixtures_path: PathBuf, max_connections: usize) -> JoinHandle<()> {
    let bind = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    let fixtures = load_fixtures(&fixtures_path).expect("fixtures load");
    let config = StubConfig {
        bind,
        require_auth: false,
        username: "test".to_string(),
        password: "secret".to_string(),
        disconnect_after: 0,
        delay_ms: 0,
    };
    let server = StubServer::new(config, fixtures);
    tokio::spawn(async move {
        server.serve_for(max_connections).await.expect("serve for");
    })
}

fn sample_config(root: &Path, rpc_port: u16, servers: &[(u16, u32)]) -> Config {
    let main_dir = root.join("main");
    let dest_dir = root.join("dest");
    let inter_dir = root.join("intermediate");
    let nzb_dir = root.join("nzb");
    let queue_dir = root.join("queue");
    let temp_dir = root.join("tmp");
    let script_dir = root.join("scripts");
    let log_file = root.join("nzbg.log");

    let mut raw = std::collections::HashMap::new();
    raw.insert("MainDir".to_string(), main_dir.display().to_string());
    raw.insert("DestDir".to_string(), dest_dir.display().to_string());
    raw.insert("InterDir".to_string(), inter_dir.display().to_string());
    raw.insert("NzbDir".to_string(), nzb_dir.display().to_string());
    raw.insert("QueueDir".to_string(), queue_dir.display().to_string());
    raw.insert("TempDir".to_string(), temp_dir.display().to_string());
    raw.insert("ScriptDir".to_string(), script_dir.display().to_string());
    raw.insert("LogFile".to_string(), log_file.display().to_string());
    raw.insert("ControlIP".to_string(), "127.0.0.1".to_string());
    raw.insert("ControlPort".to_string(), rpc_port.to_string());
    raw.insert("ControlUsername".to_string(), "nzbget".to_string());
    raw.insert("ControlPassword".to_string(), "secret".to_string());
    for (idx, (port, level)) in servers.iter().enumerate() {
        let id = idx + 1;
        raw.insert(format!("Server{id}.Active"), "yes".to_string());
        raw.insert(format!("Server{id}.Name"), format!("stub-{id}"));
        raw.insert(format!("Server{id}.Host"), "127.0.0.1".to_string());
        raw.insert(format!("Server{id}.Port"), port.to_string());
        raw.insert(format!("Server{id}.Encryption"), "no".to_string());
        raw.insert(format!("Server{id}.Connections"), "1".to_string());
        raw.insert(format!("Server{id}.Retention"), "0".to_string());
        raw.insert(format!("Server{id}.Level"), level.to_string());
        raw.insert(format!("Server{id}.Optional"), "no".to_string());
    }
    raw.insert("ArticleCache".to_string(), "0".to_string());
    raw.insert("AppendCategoryDir".to_string(), "no".to_string());
    raw.insert("UnpackCleanupDisk".to_string(), "no".to_string());

    Config::from_raw(raw)
}

async fn jsonrpc_call(
    addr: SocketAddr,
    credentials: &str,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
        "id": 1
    });
    let auth = base64::engine::general_purpose::STANDARD.encode(credentials);

    let response = timeout(
        Duration::from_secs(3),
        reqwest::Client::new()
            .post(format!("http://{addr}/jsonrpc"))
            .header("Authorization", format!("Basic {auth}"))
            .json(&payload)
            .send(),
    )
    .await
    .expect("rpc timeout")
    .expect("rpc send");
    let body: serde_json::Value = response.json().await.expect("rpc json");
    body["result"].clone()
}

fn init_logging() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let _ = init_tracing("debug");
    });
}

fn build_server_pool(
    config: &Config,
    shared_stats: &Arc<nzbg_scheduler::SharedStatsTracker>,
) -> Arc<NntpPoolFetcher> {
    let servers: Vec<NewsServer> = config
        .servers
        .iter()
        .map(|s| NewsServer {
            id: s.id,
            name: s.name.clone(),
            active: s.active,
            host: s.host.clone(),
            port: s.port,
            username: None,
            password: None,
            encryption: if s.encryption {
                Encryption::Tls
            } else {
                Encryption::None
            },
            cipher: None,
            connections: s.connections,
            retention: s.retention,
            level: s.level,
            optional: s.optional,
            group: s.group,
            join_group: true,
            ip_version: nzbg_nntp::IpVersion::IPv4Only,
            cert_verification: s.cert_verification,
        })
        .collect();

    let pool = ServerPool::new(servers).with_stats(shared_stats.clone());
    Arc::new(NntpPoolFetcher::new(pool))
}

async fn create_dirs(config: &Config) {
    tokio::fs::create_dir_all(&config.dest_dir)
        .await
        .expect("dest dir");
    tokio::fs::create_dir_all(&config.nzb_dir)
        .await
        .expect("nzb dir");
    tokio::fs::create_dir_all(&config.inter_dir)
        .await
        .expect("inter dir");
    tokio::fs::create_dir_all(&config.temp_dir)
        .await
        .expect("temp dir");
}

async fn append_nzb(rpc_addr: SocketAddr, nzb_path: &Path) -> u64 {
    let nzb_bytes = tokio::fs::read(nzb_path).await.expect("nzb read");
    let nzb_base64 = base64::engine::general_purpose::STANDARD.encode(&nzb_bytes);
    let filename = nzb_path.file_name().unwrap().to_string_lossy().to_string();
    let result = jsonrpc_call(
        rpc_addr,
        "nzbget:secret",
        "append",
        serde_json::json!([filename, nzb_base64, "", 0]),
    )
    .await;
    result.as_u64().expect("nzb id")
}

async fn wait_for_completion(rpc_addr: SocketAddr, nzb_id: u64, max_polls: usize) -> bool {
    for _ in 0..max_polls {
        let groups = jsonrpc_call(
            rpc_addr,
            "nzbget:secret",
            "listgroups",
            serde_json::json!([]),
        )
        .await;
        if let Some(entry) = groups.as_array().and_then(|entries| {
            entries
                .iter()
                .find(|entry| entry.get("NZBID").and_then(|v| v.as_u64()) == Some(nzb_id))
        }) {
            let status = entry.get("Status").and_then(|v| v.as_str()).unwrap_or("");
            if status == "QUEUED" || status == "DOWNLOADING" {
                tokio::time::sleep(Duration::from_millis(200)).await;
                continue;
            }
        } else {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    false
}

async fn shutdown_app(rpc_addr: SocketAddr) {
    let result = jsonrpc_call(rpc_addr, "nzbget:secret", "shutdown", serde_json::json!([])).await;
    assert!(result.as_bool().unwrap_or(false));
}

#[tokio::test]
async fn end_to_end_append_download_flow() {
    let temp = tempfile::tempdir().expect("tempdir");
    let stub_port = available_port();
    let rpc_port = available_port();

    init_logging();

    let stub_task = start_stub(stub_port, fixtures_complete_path(), 1).await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    let config = sample_config(temp.path(), rpc_port, &[(stub_port, 0)]);
    let inter_dir = config.inter_dir.clone();
    let dest_dir = config.dest_dir.clone();
    tokio::fs::create_dir_all(&config.dest_dir)
        .await
        .expect("dest dir");
    tokio::fs::create_dir_all(&config.nzb_dir)
        .await
        .expect("nzb dir");
    tokio::fs::create_dir_all(&config.inter_dir)
        .await
        .expect("inter dir");
    tokio::fs::create_dir_all(&config.temp_dir)
        .await
        .expect("temp dir");
    let stats = nzbg_scheduler::StatsTracker::from_config(&config);
    let shared_stats = std::sync::Arc::new(nzbg_scheduler::SharedStatsTracker::new(stats));

    let servers: Vec<NewsServer> = config
        .servers
        .iter()
        .map(|s| NewsServer {
            id: s.id,
            name: s.name.clone(),
            active: s.active,
            host: s.host.clone(),
            port: s.port,
            username: None,
            password: None,
            encryption: if s.encryption {
                Encryption::Tls
            } else {
                Encryption::None
            },
            cipher: None,
            connections: s.connections,
            retention: s.retention,
            level: s.level,
            optional: s.optional,
            group: s.group,
            join_group: true,
            ip_version: nzbg_nntp::IpVersion::IPv4Only,
            cert_verification: s.cert_verification,
        })
        .collect();

    let pool = ServerPool::new(servers).with_stats(shared_stats.clone());
    let fetcher = std::sync::Arc::new(NntpPoolFetcher::new(pool));

    let app_task = tokio::spawn(run_with_config_path(
        config,
        fetcher,
        None,
        None,
        Some(shared_stats),
    ));

    tokio::time::sleep(Duration::from_millis(200)).await;

    let nzb_bytes = tokio::fs::read(sample_nzb_path()).await.expect("nzb read");
    let nzb_base64 = base64::engine::general_purpose::STANDARD.encode(&nzb_bytes);

    let result = jsonrpc_call(
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), rpc_port),
        "nzbget:secret",
        "append",
        serde_json::json!(["sample.nzb", nzb_base64, "", 0]),
    )
    .await;
    let nzb_id = result.as_u64().expect("nzb id");

    let mut completed = false;
    for _ in 0..50 {
        let groups = jsonrpc_call(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), rpc_port),
            "nzbget:secret",
            "listgroups",
            serde_json::json!([]),
        )
        .await;
        let groups_str = groups.to_string();
        tracing::debug!("listgroups response: {groups_str}");
        if let Some(entry) = groups.as_array().and_then(|entries| {
            entries
                .iter()
                .find(|entry| entry.get("NZBID").and_then(|v| v.as_u64()) == Some(nzb_id))
        }) {
            let status = entry.get("Status").and_then(|v| v.as_str()).unwrap_or("");
            tracing::info!("nzb status: {status}");
            if status == "QUEUED" || status == "DOWNLOADING" {
                tokio::time::sleep(Duration::from_millis(200)).await;
                continue;
            }
        } else {
            completed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    assert!(completed, "nzb should complete and leave the queue");

    let dest_path = dest_dir.join("sample.txt");
    let working_dir = inter_dir.join(format!("nzb-{nzb_id}"));
    let completed_content = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Ok(content) = tokio::fs::read_to_string(&dest_path).await {
                return content;
            }
            if let Ok(mut entries) = tokio::fs::read_dir(&working_dir).await {
                if let Ok(Some(entry)) = entries.next_entry().await {
                    if let Ok(content) = tokio::fs::read_to_string(entry.path()).await {
                        return content;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("completed file timeout");
    assert_eq!(completed_content, "ABCDEFGH");

    let shutdown_result = jsonrpc_call(
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), rpc_port),
        "nzbget:secret",
        "shutdown",
        serde_json::json!([]),
    )
    .await;
    assert!(shutdown_result.as_bool().unwrap_or(false));

    let _ = tokio::time::timeout(Duration::from_secs(5), app_task)
        .await
        .expect("app shutdown timeout");
    let _ = tokio::time::timeout(Duration::from_secs(2), stub_task)
        .await
        .expect("stub shutdown timeout");
}

#[tokio::test]
async fn missing_article_falls_back_to_second_server() {
    let temp = tempfile::tempdir().expect("tempdir");
    let primary_port = available_port();
    let backup_port = available_port();
    let rpc_port = available_port();

    init_logging();

    let primary_task = start_stub(primary_port, fixtures_basic_path(), 2).await;
    let backup_task = start_stub(backup_port, fixtures_complete_path(), 2).await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    let config = sample_config(
        temp.path(),
        rpc_port,
        &[(primary_port, 0), (backup_port, 1)],
    );
    let inter_dir = config.inter_dir.clone();
    let dest_dir = config.dest_dir.clone();
    tokio::fs::create_dir_all(&config.dest_dir)
        .await
        .expect("dest dir");
    tokio::fs::create_dir_all(&config.nzb_dir)
        .await
        .expect("nzb dir");
    tokio::fs::create_dir_all(&config.inter_dir)
        .await
        .expect("inter dir");
    tokio::fs::create_dir_all(&config.temp_dir)
        .await
        .expect("temp dir");
    let stats = nzbg_scheduler::StatsTracker::from_config(&config);
    let shared_stats = std::sync::Arc::new(nzbg_scheduler::SharedStatsTracker::new(stats));

    let servers: Vec<NewsServer> = config
        .servers
        .iter()
        .map(|s| NewsServer {
            id: s.id,
            name: s.name.clone(),
            active: s.active,
            host: s.host.clone(),
            port: s.port,
            username: None,
            password: None,
            encryption: if s.encryption {
                Encryption::Tls
            } else {
                Encryption::None
            },
            cipher: None,
            connections: s.connections,
            retention: s.retention,
            level: s.level,
            optional: s.optional,
            group: s.group,
            join_group: true,
            ip_version: nzbg_nntp::IpVersion::IPv4Only,
            cert_verification: s.cert_verification,
        })
        .collect();

    let pool = ServerPool::new(servers).with_stats(shared_stats.clone());
    let fetcher = std::sync::Arc::new(NntpPoolFetcher::new(pool));

    let app_task = tokio::spawn(run_with_config_path(
        config,
        fetcher,
        None,
        None,
        Some(shared_stats),
    ));

    tokio::time::sleep(Duration::from_millis(200)).await;

    let nzb_bytes = tokio::fs::read(sample_nzb_path()).await.expect("nzb read");
    let nzb_base64 = base64::engine::general_purpose::STANDARD.encode(&nzb_bytes);

    let result = jsonrpc_call(
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), rpc_port),
        "nzbget:secret",
        "append",
        serde_json::json!(["sample.nzb", nzb_base64, "", 0]),
    )
    .await;
    let nzb_id = result.as_u64().expect("nzb id");

    let mut completed = false;
    for _ in 0..50 {
        let groups = jsonrpc_call(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), rpc_port),
            "nzbget:secret",
            "listgroups",
            serde_json::json!([]),
        )
        .await;
        if let Some(entry) = groups.as_array().and_then(|entries| {
            entries
                .iter()
                .find(|entry| entry.get("NZBID").and_then(|v| v.as_u64()) == Some(nzb_id))
        }) {
            let status = entry.get("Status").and_then(|v| v.as_str()).unwrap_or("");
            if status == "QUEUED" || status == "DOWNLOADING" {
                tokio::time::sleep(Duration::from_millis(200)).await;
                continue;
            }
        } else {
            completed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    assert!(completed, "nzb should complete via fallback server");

    let dest_path = dest_dir.join("sample.txt");
    let working_dir = inter_dir.join(format!("nzb-{nzb_id}"));
    let completed_content = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Ok(content) = tokio::fs::read_to_string(&dest_path).await {
                return content;
            }
            if let Ok(mut entries) = tokio::fs::read_dir(&working_dir).await {
                if let Ok(Some(entry)) = entries.next_entry().await {
                    if let Ok(content) = tokio::fs::read_to_string(entry.path()).await {
                        return content;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("completed file timeout");
    assert_eq!(completed_content, "ABCDEFGH");

    let shutdown_result = jsonrpc_call(
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), rpc_port),
        "nzbget:secret",
        "shutdown",
        serde_json::json!([]),
    )
    .await;
    assert!(shutdown_result.as_bool().unwrap_or(false));

    let _ = tokio::time::timeout(Duration::from_secs(5), app_task)
        .await
        .expect("app shutdown timeout");
    let _ = tokio::time::timeout(Duration::from_secs(2), primary_task).await;
    let _ = tokio::time::timeout(Duration::from_secs(2), backup_task).await;
}

#[tokio::test]
async fn crash_recovery_resumes_download() {
    let temp = tempfile::tempdir().expect("tempdir");
    let stub_port = available_port();
    let rpc_port = available_port();
    let rpc_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), rpc_port);

    init_logging();

    let stub_config = StubConfig {
        bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), stub_port),
        require_auth: false,
        username: "test".to_string(),
        password: "secret".to_string(),
        disconnect_after: 0,
        delay_ms: 200,
    };
    let fixtures = load_fixtures(&fixtures_multiseg_path()).expect("fixtures load");
    let stub = StubServer::new(stub_config, fixtures);
    let stub_task = tokio::spawn(async move {
        let _ = stub.serve().await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;

    let config = sample_config(temp.path(), rpc_port, &[(stub_port, 0)]);
    create_dirs(&config).await;

    let stats = nzbg_scheduler::StatsTracker::from_config(&config);
    let shared_stats = Arc::new(nzbg_scheduler::SharedStatsTracker::new(stats));
    let fetcher = build_server_pool(&config, &shared_stats);

    let app_task = tokio::spawn(run_with_config_path(
        config.clone(),
        fetcher,
        None,
        None,
        Some(shared_stats),
    ));

    tokio::time::sleep(Duration::from_millis(300)).await;

    let nzb_id = append_nzb(rpc_addr, &multi_nzb_path()).await;
    tracing::info!("appended NZB with id {nzb_id}");

    tokio::time::sleep(Duration::from_millis(800)).await;

    shutdown_app(rpc_addr).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), app_task)
        .await
        .expect("first app shutdown timeout");

    tracing::info!("first instance shut down, verifying disk state exists");

    let queue_json = temp.path().join("queue").join("queue.json");
    assert!(
        queue_json.exists(),
        "queue.json should exist after shutdown"
    );

    let rpc_port2 = available_port();
    let rpc_addr2 = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), rpc_port2);

    let config2 = sample_config(temp.path(), rpc_port2, &[(stub_port, 0)]);

    let stats2 = nzbg_scheduler::StatsTracker::from_config(&config2);
    let shared_stats2 = Arc::new(nzbg_scheduler::SharedStatsTracker::new(stats2));
    let fetcher2 = build_server_pool(&config2, &shared_stats2);

    let app_task2 = tokio::spawn(run_with_config_path(
        config2.clone(),
        fetcher2,
        None,
        None,
        Some(shared_stats2),
    ));

    tokio::time::sleep(Duration::from_millis(300)).await;

    let completed = wait_for_completion(rpc_addr2, nzb_id, 60).await;
    assert!(completed, "NZB should complete after restart");

    let dest_dir = config2.dest_dir.clone();
    let inter_dir = config2.inter_dir.clone();
    let content = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let dest_path = dest_dir.join("multi.txt");
            if let Ok(content) = tokio::fs::read_to_string(&dest_path).await {
                return content;
            }
            if let Ok(mut entries) = tokio::fs::read_dir(&inter_dir).await {
                while let Ok(Some(entry)) = entries.next_entry().await {
                    let sub = entry.path();
                    if sub.is_dir() {
                        if let Ok(mut files) = tokio::fs::read_dir(&sub).await {
                            while let Ok(Some(f)) = files.next_entry().await {
                                if let Ok(content) = tokio::fs::read_to_string(f.path()).await {
                                    if content.contains("ABCD") {
                                        return content;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("completed file timeout");
    assert_eq!(content, "ABCDABCDABCDABCDABCDABCD");

    shutdown_app(rpc_addr2).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), app_task2)
        .await
        .expect("second app shutdown timeout");

    stub_task.abort();
    let _ = stub_task.await;
}

#[tokio::test]
async fn rpc_status_schema_conformance() {
    let temp = tempfile::tempdir().expect("tempdir");
    let stub_port = available_port();
    let rpc_port = available_port();
    let rpc_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), rpc_port);

    init_logging();

    let stub_task = start_stub(stub_port, fixtures_complete_path(), 1).await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    let config = sample_config(temp.path(), rpc_port, &[(stub_port, 0)]);
    create_dirs(&config).await;

    let stats = nzbg_scheduler::StatsTracker::from_config(&config);
    let shared_stats = Arc::new(nzbg_scheduler::SharedStatsTracker::new(stats));
    let fetcher = build_server_pool(&config, &shared_stats);

    let app_task = tokio::spawn(run_with_config_path(
        config,
        fetcher,
        None,
        None,
        Some(shared_stats),
    ));
    tokio::time::sleep(Duration::from_millis(200)).await;

    let status = jsonrpc_call(rpc_addr, "nzbget:secret", "status", serde_json::json!([])).await;

    let expected_fields = [
        "RemainingSizeLo",
        "RemainingSizeHi",
        "RemainingSizeMB",
        "ForcedSizeLo",
        "ForcedSizeHi",
        "ForcedSizeMB",
        "DownloadedSizeLo",
        "DownloadedSizeHi",
        "DownloadedSizeMB",
        "ArticleCacheLo",
        "ArticleCacheHi",
        "ArticleCacheMB",
        "DownloadRate",
        "AverageDownloadRate",
        "DownloadLimit",
        "ThreadCount",
        "PostJobCount",
        "UrlCount",
        "UpTimeSec",
        "DownloadTimeSec",
        "ServerPaused",
        "DownloadPaused",
        "Download2Paused",
        "ServerStandBy",
        "PostPaused",
        "ScanPaused",
        "QuotaReached",
        "FreeDiskSpaceLo",
        "FreeDiskSpaceHi",
        "FreeDiskSpaceMB",
        "ServerTime",
        "ResumeTime",
        "FeedActive",
        "QueueScriptCount",
        "NewsServers",
    ];

    let status_obj = status.as_object().expect("status should be an object");
    for field in &expected_fields {
        assert!(
            status_obj.contains_key(*field),
            "status response missing required field: {field}"
        );
    }

    let news_servers = status_obj
        .get("NewsServers")
        .expect("NewsServers")
        .as_array()
        .expect("NewsServers should be an array");
    if !news_servers.is_empty() {
        let server = &news_servers[0];
        for field in ["ID", "Active"] {
            assert!(
                server.as_object().unwrap().contains_key(field),
                "NewsServer entry missing field: {field}"
            );
        }
    }

    shutdown_app(rpc_addr).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), app_task)
        .await
        .expect("app shutdown timeout");
    let _ = tokio::time::timeout(Duration::from_secs(2), stub_task).await;
}

#[tokio::test]
async fn rpc_version_reports_compatibility() {
    let temp = tempfile::tempdir().expect("tempdir");
    let stub_port = available_port();
    let rpc_port = available_port();
    let rpc_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), rpc_port);

    init_logging();

    let stub_task = start_stub(stub_port, fixtures_complete_path(), 1).await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    let config = sample_config(temp.path(), rpc_port, &[(stub_port, 0)]);
    create_dirs(&config).await;

    let stats = nzbg_scheduler::StatsTracker::from_config(&config);
    let shared_stats = Arc::new(nzbg_scheduler::SharedStatsTracker::new(stats));
    let fetcher = build_server_pool(&config, &shared_stats);

    let app_task = tokio::spawn(run_with_config_path(
        config,
        fetcher,
        None,
        None,
        Some(shared_stats),
    ));
    tokio::time::sleep(Duration::from_millis(200)).await;

    let version = jsonrpc_call(rpc_addr, "nzbget:secret", "version", serde_json::json!([])).await;
    let version_str = version.as_str().expect("version should be a string");
    let major: u32 = version_str
        .split('.')
        .next()
        .unwrap()
        .parse()
        .expect("major version number");
    assert!(
        major >= 26,
        "version major should be >= 26 for Radarr/Sonarr compatibility, got {version_str}"
    );

    shutdown_app(rpc_addr).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), app_task)
        .await
        .expect("app shutdown timeout");
    let _ = tokio::time::timeout(Duration::from_secs(2), stub_task).await;
}

#[tokio::test]
async fn rpc_listgroups_schema_during_download() {
    let temp = tempfile::tempdir().expect("tempdir");
    let stub_port = available_port();
    let rpc_port = available_port();
    let rpc_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), rpc_port);

    init_logging();

    let stub_config = StubConfig {
        bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), stub_port),
        require_auth: false,
        username: "test".to_string(),
        password: "secret".to_string(),
        disconnect_after: 0,
        delay_ms: 300,
    };
    let fixtures = load_fixtures(&fixtures_multiseg_path()).expect("fixtures load");
    let stub = StubServer::new(stub_config, fixtures);
    let stub_task = tokio::spawn(async move {
        let _ = stub.serve().await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;

    let config = sample_config(temp.path(), rpc_port, &[(stub_port, 0)]);
    create_dirs(&config).await;

    let stats = nzbg_scheduler::StatsTracker::from_config(&config);
    let shared_stats = Arc::new(nzbg_scheduler::SharedStatsTracker::new(stats));
    let fetcher = build_server_pool(&config, &shared_stats);

    let app_task = tokio::spawn(run_with_config_path(
        config,
        fetcher,
        None,
        None,
        Some(shared_stats),
    ));
    tokio::time::sleep(Duration::from_millis(200)).await;

    let _nzb_id = append_nzb(rpc_addr, &multi_nzb_path()).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let groups = jsonrpc_call(
        rpc_addr,
        "nzbget:secret",
        "listgroups",
        serde_json::json!([]),
    )
    .await;

    let entries = groups.as_array().expect("listgroups should be an array");
    assert!(!entries.is_empty(), "should have at least one entry");

    let entry = &entries[0];
    let expected_fields = [
        "NZBID",
        "NZBName",
        "Kind",
        "Status",
        "Category",
        "FileSizeLo",
        "FileSizeMB",
        "RemainingSizeLo",
        "RemainingSizeMB",
        "PausedSizeLo",
        "PausedSizeMB",
        "ActiveDownloads",
        "MinPostTime",
        "MaxPriority",
        "Health",
        "CriticalHealth",
        "DupeKey",
        "DupeScore",
        "DupeMode",
        "SuccessArticles",
        "FailedArticles",
        "ServerStats",
        "Parameters",
        "PostTotalTimeSec",
    ];

    let obj = entry.as_object().expect("entry should be an object");
    for field in &expected_fields {
        assert!(
            obj.contains_key(*field),
            "listgroups entry missing required field: {field}"
        );
    }

    shutdown_app(rpc_addr).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), app_task)
        .await
        .expect("app shutdown timeout");

    stub_task.abort();
    let _ = stub_task.await;
}

#[tokio::test]
async fn rpc_history_schema_conformance() {
    let temp = tempfile::tempdir().expect("tempdir");
    let stub_port = available_port();
    let rpc_port = available_port();
    let rpc_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), rpc_port);

    init_logging();

    let stub_task = start_stub(stub_port, fixtures_complete_path(), 2).await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    let config = sample_config(temp.path(), rpc_port, &[(stub_port, 0)]);
    create_dirs(&config).await;

    let stats = nzbg_scheduler::StatsTracker::from_config(&config);
    let shared_stats = Arc::new(nzbg_scheduler::SharedStatsTracker::new(stats));
    let fetcher = build_server_pool(&config, &shared_stats);

    let app_task = tokio::spawn(run_with_config_path(
        config,
        fetcher,
        None,
        None,
        Some(shared_stats),
    ));
    tokio::time::sleep(Duration::from_millis(200)).await;

    let nzb_id = append_nzb(rpc_addr, &sample_nzb_path()).await;
    let completed = wait_for_completion(rpc_addr, nzb_id, 50).await;
    assert!(completed, "nzb should complete before checking history");

    tokio::time::sleep(Duration::from_millis(500)).await;

    let history = jsonrpc_call(
        rpc_addr,
        "nzbget:secret",
        "history",
        serde_json::json!([false]),
    )
    .await;

    let entries = history.as_array().expect("history should be an array");
    assert!(!entries.is_empty(), "history should have at least one entry");

    let entry = entries
        .iter()
        .find(|e| e.get("NZBID").and_then(|v| v.as_u64()) == Some(nzb_id))
        .expect("should find our NZB in history");

    let obj = entry.as_object().expect("entry should be an object");

    let required_fields = [
        "NZBID",
        "ID",
        "Kind",
        "NZBFilename",
        "Name",
        "NZBName",
        "URL",
        "RetryData",
        "HistoryTime",
        "DestDir",
        "FinalDir",
        "Category",
        "FileSizeLo",
        "FileSizeHi",
        "FileSizeMB",
        "FileCount",
        "RemainingFileCount",
        "MinPostTime",
        "MaxPostTime",
        "TotalArticles",
        "SuccessArticles",
        "FailedArticles",
        "Health",
        "CriticalHealth",
        "DownloadedSizeLo",
        "DownloadedSizeHi",
        "DownloadedSizeMB",
        "DownloadTimeSec",
        "PostTotalTimeSec",
        "ParTimeSec",
        "RepairTimeSec",
        "UnpackTimeSec",
        "MessageCount",
        "DupeKey",
        "DupeScore",
        "DupeMode",
        "Status",
        "ParStatus",
        "UnpackStatus",
        "MoveStatus",
        "ScriptStatus",
        "DeleteStatus",
        "MarkStatus",
        "UrlStatus",
        "ExtraParBlocks",
        "Parameters",
        "ServerStats",
        "ScriptStatuses",
    ];

    for field in &required_fields {
        assert!(
            obj.contains_key(*field),
            "history entry missing required field: {field}"
        );
    }

    let kind = obj["Kind"].as_str().expect("Kind should be a string");
    assert_eq!(kind, "NZB", "completed download should have Kind=NZB");

    let status = obj["Status"].as_str().expect("Status should be a string");
    assert!(
        status.starts_with("SUCCESS"),
        "completed download should have SUCCESS status, got: {status}"
    );

    let string_status_fields = [
        "ParStatus",
        "UnpackStatus",
        "MoveStatus",
        "ScriptStatus",
        "DeleteStatus",
        "MarkStatus",
        "UrlStatus",
        "DupeMode",
    ];
    for field in &string_status_fields {
        assert!(
            obj[*field].is_string(),
            "field {field} should be a string per NZBGet API spec, got: {}",
            obj[*field]
        );
    }

    assert!(
        obj["NZBID"].is_number(),
        "NZBID should be a number"
    );
    assert!(
        obj["HistoryTime"].is_number(),
        "HistoryTime should be a number"
    );
    assert!(
        obj["FileSizeLo"].is_number(),
        "FileSizeLo should be a number"
    );
    assert!(
        obj["FileSizeHi"].is_number(),
        "FileSizeHi should be a number"
    );
    assert!(
        obj["FileSizeMB"].is_number(),
        "FileSizeMB should be a number"
    );
    assert!(
        obj["Health"].is_number(),
        "Health should be a number"
    );
    assert!(
        obj["RetryData"].is_boolean(),
        "RetryData should be a boolean"
    );
    assert!(
        obj["Parameters"].is_array(),
        "Parameters should be an array"
    );
    assert!(
        obj["ServerStats"].is_array(),
        "ServerStats should be an array"
    );
    assert!(
        obj["ScriptStatuses"].is_array(),
        "ScriptStatuses should be an array"
    );

    shutdown_app(rpc_addr).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), app_task)
        .await
        .expect("app shutdown timeout");
    let _ = tokio::time::timeout(Duration::from_secs(2), stub_task).await;
}

async fn jsonrpc_raw(
    addr: SocketAddr,
    auth_header: Option<&str>,
    method: &str,
) -> reqwest::Response {
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": [],
        "id": 1
    });
    let mut req = reqwest::Client::new()
        .post(format!("http://{addr}/jsonrpc"))
        .json(&payload);
    if let Some(auth) = auth_header {
        req = req.header("Authorization", auth);
    }
    timeout(Duration::from_secs(3), req.send())
        .await
        .expect("rpc timeout")
        .expect("rpc send")
}

#[tokio::test]
async fn rpc_authentication_rejection() {
    let temp = tempfile::tempdir().expect("tempdir");
    let stub_port = available_port();
    let rpc_port = available_port();
    let rpc_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), rpc_port);

    init_logging();

    let stub_task = start_stub(stub_port, fixtures_complete_path(), 1).await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    let config = sample_config(temp.path(), rpc_port, &[(stub_port, 0)]);
    create_dirs(&config).await;

    let stats = nzbg_scheduler::StatsTracker::from_config(&config);
    let shared_stats = Arc::new(nzbg_scheduler::SharedStatsTracker::new(stats));
    let fetcher = build_server_pool(&config, &shared_stats);

    let app_task = tokio::spawn(run_with_config_path(
        config,
        fetcher,
        None,
        None,
        Some(shared_stats),
    ));
    tokio::time::sleep(Duration::from_millis(200)).await;

    let wrong_creds = base64::engine::general_purpose::STANDARD.encode("nzbget:wrongpassword");
    let resp = jsonrpc_raw(
        rpc_addr,
        Some(&format!("Basic {wrong_creds}")),
        "version",
    )
    .await;
    assert_eq!(
        resp.status().as_u16(),
        401,
        "wrong credentials should return 401"
    );
    assert!(
        resp.headers()
            .get("www-authenticate")
            .is_some(),
        "401 response should include WWW-Authenticate header"
    );

    let resp_no_auth = jsonrpc_raw(rpc_addr, None, "version").await;
    assert_eq!(
        resp_no_auth.status().as_u16(),
        401,
        "missing credentials should return 401"
    );

    let valid_creds = base64::engine::general_purpose::STANDARD.encode("nzbget:secret");
    let resp_ok = jsonrpc_raw(
        rpc_addr,
        Some(&format!("Basic {valid_creds}")),
        "version",
    )
    .await;
    assert_eq!(
        resp_ok.status().as_u16(),
        200,
        "correct credentials should return 200"
    );

    shutdown_app(rpc_addr).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), app_task)
        .await
        .expect("app shutdown timeout");
    let _ = tokio::time::timeout(Duration::from_secs(2), stub_task).await;
}
