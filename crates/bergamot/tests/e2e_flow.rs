use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Once};
use std::time::Duration;

use base64::Engine;
use bergamot::app::{init_tracing, run_with_config_path};
use bergamot::download::NntpPoolFetcher;
use bergamot_config::Config;
use bergamot_nntp::{Encryption, NewsServer, ServerPool};
use bergamot_nntp_stub::{StubConfig, StubServer, load_fixtures};
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

fn fixtures_multifile_path() -> PathBuf {
    fixtures_dir().join("fixtures-multifile.json")
}

fn fixtures_concurrent_path() -> PathBuf {
    fixtures_dir().join("fixtures-concurrent.json")
}

fn fixtures_par2_path() -> PathBuf {
    fixtures_dir().join("fixtures-par2.json")
}

fn fixtures_all_missing_path() -> PathBuf {
    fixtures_dir().join("fixtures-all-missing.json")
}

fn sample_nzb_path() -> PathBuf {
    fixtures_dir().join("sample.nzb")
}

fn multi_nzb_path() -> PathBuf {
    fixtures_dir().join("multi.nzb")
}

fn multifile_nzb_path() -> PathBuf {
    fixtures_dir().join("multifile.nzb")
}

fn par2_nzb_path() -> PathBuf {
    fixtures_dir().join("par2.nzb")
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
    let log_file = root.join("bergamot.log");

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

async fn jsonrpc_call_full(
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
    response.json().await.expect("rpc json")
}

fn init_logging() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let _ = init_tracing("debug");
    });
}

fn build_server_pool(
    config: &Config,
    shared_stats: &Arc<bergamot_scheduler::SharedStatsTracker>,
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
            ip_version: bergamot_nntp::IpVersion::IPv4Only,
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
    for attempt in 0..10 {
        let body = jsonrpc_call_full(
            rpc_addr,
            "nzbget:secret",
            "append",
            serde_json::json!([filename, nzb_base64, "", 0]),
        )
        .await;
        if let Some(id) = body.get("result").and_then(|v| v.as_u64()) {
            return id;
        }
        tracing::warn!(attempt, ?body, "append_nzb did not return an id, retrying");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("append_nzb failed after 10 retries");
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
        if groups
            .as_array()
            .and_then(|entries| {
                entries
                    .iter()
                    .find(|entry| entry.get("NZBID").and_then(|v| v.as_u64()) == Some(nzb_id))
            })
            .is_some()
        {
            tokio::time::sleep(Duration::from_millis(200)).await;
            continue;
        } else {
            return true;
        }
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
    let stats = bergamot_scheduler::StatsTracker::from_config(&config);
    let shared_stats = std::sync::Arc::new(bergamot_scheduler::SharedStatsTracker::new(stats));

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
            ip_version: bergamot_nntp::IpVersion::IPv4Only,
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

    let rpc_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), rpc_port);
    let nzb_id = append_nzb(rpc_addr, &sample_nzb_path()).await;

    let mut completed = false;
    for _ in 0..50 {
        let groups = jsonrpc_call(
            rpc_addr,
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

    let working_dir = inter_dir.join(format!("nzb-{nzb_id}"));
    let completed_content = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            for dir in [&dest_dir, &working_dir] {
                if let Ok(mut entries) = tokio::fs::read_dir(dir).await {
                    while let Ok(Some(entry)) = entries.next_entry().await {
                        if let Ok(content) = tokio::fs::read_to_string(entry.path()).await
                            && content == "ABCDEFGH"
                        {
                            return content;
                        }
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
    let stats = bergamot_scheduler::StatsTracker::from_config(&config);
    let shared_stats = std::sync::Arc::new(bergamot_scheduler::SharedStatsTracker::new(stats));

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
            ip_version: bergamot_nntp::IpVersion::IPv4Only,
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

    let rpc_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), rpc_port);
    let nzb_id = append_nzb(rpc_addr, &sample_nzb_path()).await;

    let mut completed = false;
    for _ in 0..50 {
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
            completed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    assert!(completed, "nzb should complete via fallback server");

    let working_dir = inter_dir.join(format!("nzb-{nzb_id}"));
    let completed_content = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            for dir in [&dest_dir, &working_dir] {
                if let Ok(mut entries) = tokio::fs::read_dir(dir).await {
                    while let Ok(Some(entry)) = entries.next_entry().await {
                        if let Ok(content) = tokio::fs::read_to_string(entry.path()).await
                            && content == "ABCDEFGH"
                        {
                            return content;
                        }
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

    let stats = bergamot_scheduler::StatsTracker::from_config(&config);
    let shared_stats = Arc::new(bergamot_scheduler::SharedStatsTracker::new(stats));
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

    let stats2 = bergamot_scheduler::StatsTracker::from_config(&config2);
    let shared_stats2 = Arc::new(bergamot_scheduler::SharedStatsTracker::new(stats2));
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
    let expected = "ABCDABCDABCDABCDABCDABCD";
    let content = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            for search_dir in [&dest_dir, &inter_dir] {
                if let Ok(mut entries) = tokio::fs::read_dir(search_dir).await {
                    while let Ok(Some(entry)) = entries.next_entry().await {
                        let path = entry.path();
                        if let Ok(c) = tokio::fs::read_to_string(&path).await
                            && c == expected
                        {
                            return c;
                        }
                        if path.is_dir()
                            && let Ok(mut files) = tokio::fs::read_dir(&path).await
                        {
                            while let Ok(Some(f)) = files.next_entry().await {
                                if let Ok(c) = tokio::fs::read_to_string(f.path()).await
                                    && c == expected
                                {
                                    return c;
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
    assert_eq!(content, expected);

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

    let stats = bergamot_scheduler::StatsTracker::from_config(&config);
    let shared_stats = Arc::new(bergamot_scheduler::SharedStatsTracker::new(stats));
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

    let stats = bergamot_scheduler::StatsTracker::from_config(&config);
    let shared_stats = Arc::new(bergamot_scheduler::SharedStatsTracker::new(stats));
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

    let stats = bergamot_scheduler::StatsTracker::from_config(&config);
    let shared_stats = Arc::new(bergamot_scheduler::SharedStatsTracker::new(stats));
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

    let stats = bergamot_scheduler::StatsTracker::from_config(&config);
    let shared_stats = Arc::new(bergamot_scheduler::SharedStatsTracker::new(stats));
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
    assert!(
        !entries.is_empty(),
        "history should have at least one entry"
    );

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

    assert!(obj["NZBID"].is_number(), "NZBID should be a number");
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
    assert!(obj["Health"].is_number(), "Health should be a number");
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

    let stats = bergamot_scheduler::StatsTracker::from_config(&config);
    let shared_stats = Arc::new(bergamot_scheduler::SharedStatsTracker::new(stats));
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
    let resp = jsonrpc_raw(rpc_addr, Some(&format!("Basic {wrong_creds}")), "version").await;
    assert_eq!(
        resp.status().as_u16(),
        401,
        "wrong credentials should return 401"
    );
    assert!(
        resp.headers().get("www-authenticate").is_some(),
        "401 response should include WWW-Authenticate header"
    );

    let resp_no_auth = jsonrpc_raw(rpc_addr, None, "version").await;
    assert_eq!(
        resp_no_auth.status().as_u16(),
        401,
        "missing credentials should return 401"
    );

    let valid_creds = base64::engine::general_purpose::STANDARD.encode("nzbget:secret");
    let resp_ok = jsonrpc_raw(rpc_addr, Some(&format!("Basic {valid_creds}")), "version").await;
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

#[tokio::test]
async fn multifile_nzb_produces_all_output_files() {
    let temp = tempfile::tempdir().expect("tempdir");
    let stub_port = available_port();
    let rpc_port = available_port();
    let rpc_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), rpc_port);

    init_logging();

    let stub_task = start_stub(stub_port, fixtures_multifile_path(), 2).await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    let config = sample_config(temp.path(), rpc_port, &[(stub_port, 0)]);
    let dest_dir = config.dest_dir.clone();
    let inter_dir = config.inter_dir.clone();
    create_dirs(&config).await;

    let stats = bergamot_scheduler::StatsTracker::from_config(&config);
    let shared_stats = Arc::new(bergamot_scheduler::SharedStatsTracker::new(stats));
    let fetcher = build_server_pool(&config, &shared_stats);

    let app_task = tokio::spawn(run_with_config_path(
        config,
        fetcher,
        None,
        None,
        Some(shared_stats),
    ));
    tokio::time::sleep(Duration::from_millis(200)).await;

    let nzb_id = append_nzb(rpc_addr, &multifile_nzb_path()).await;
    let completed = wait_for_completion(rpc_addr, nzb_id, 50).await;
    assert!(completed, "multifile nzb should complete");

    let find_file = |name: &str| {
        let dest = dest_dir.clone();
        let inter = inter_dir.clone();
        let name = name.to_string();
        async move {
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    if let Ok(content) = tokio::fs::read_to_string(dest.join(&name)).await {
                        return content;
                    }
                    if let Ok(mut entries) = tokio::fs::read_dir(&inter).await {
                        while let Ok(Some(entry)) = entries.next_entry().await {
                            let sub = entry.path();
                            if sub.is_dir() {
                                let candidate = sub.join(&name);
                                if let Ok(content) = tokio::fs::read_to_string(&candidate).await {
                                    return content;
                                }
                            }
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            })
            .await
        }
    };

    let alpha = find_file("alpha.txt")
        .await
        .expect("alpha.txt should be produced");
    assert_eq!(alpha, "ABCD", "alpha.txt content mismatch");

    let beta = find_file("beta.txt")
        .await
        .expect("beta.txt should be produced");
    assert_eq!(beta, "MNOP", "beta.txt content mismatch");

    shutdown_app(rpc_addr).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), app_task)
        .await
        .expect("app shutdown timeout");
    let _ = tokio::time::timeout(Duration::from_secs(2), stub_task).await;
}

#[tokio::test]
async fn concurrent_downloads_complete_without_corruption() {
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
        delay_ms: 0,
    };
    let fixtures = load_fixtures(&fixtures_concurrent_path()).expect("fixtures load");
    let stub = StubServer::new(stub_config, fixtures);
    let stub_task = tokio::spawn(async move {
        let _ = stub.serve().await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;

    let config = sample_config(temp.path(), rpc_port, &[(stub_port, 0)]);
    let inter_dir = config.inter_dir.clone();
    let dest_dir = config.dest_dir.clone();
    create_dirs(&config).await;

    let stats = bergamot_scheduler::StatsTracker::from_config(&config);
    let shared_stats = Arc::new(bergamot_scheduler::SharedStatsTracker::new(stats));
    let fetcher = build_server_pool(&config, &shared_stats);

    let app_task = tokio::spawn(run_with_config_path(
        config,
        fetcher,
        None,
        None,
        Some(shared_stats),
    ));
    tokio::time::sleep(Duration::from_millis(200)).await;

    let nzb_id_1 = append_nzb(rpc_addr, &sample_nzb_path()).await;
    let nzb_id_2 = append_nzb(rpc_addr, &multifile_nzb_path()).await;
    assert_ne!(nzb_id_1, nzb_id_2, "NZB IDs should be distinct");

    let completed_1 = wait_for_completion(rpc_addr, nzb_id_1, 60).await;
    let completed_2 = wait_for_completion(rpc_addr, nzb_id_2, 60).await;
    assert!(completed_1, "first NZB should complete");
    assert!(completed_2, "second NZB should complete");

    let collect_files = |inter: PathBuf, dest: PathBuf, nzb_id: u64| async move {
        let working = inter.join(format!("nzb-{nzb_id}"));
        tokio::time::timeout(Duration::from_secs(5), async move {
            loop {
                let mut contents = Vec::new();
                for dir in [&working, &dest] {
                    if let Ok(mut entries) = tokio::fs::read_dir(dir).await {
                        while let Ok(Some(entry)) = entries.next_entry().await {
                            if let Ok(content) = tokio::fs::read_to_string(entry.path()).await
                                && !contents.contains(&content)
                            {
                                contents.push(content);
                            }
                        }
                    }
                }
                if !contents.is_empty() {
                    contents.sort();
                    return contents;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
    };

    let all_files = collect_files(inter_dir.clone(), dest_dir.clone(), nzb_id_1)
        .await
        .expect("files should be produced");
    assert!(
        all_files.contains(&"ABCDEFGH".to_string()),
        "sample.nzb content ABCDEFGH should be present"
    );
    assert!(
        all_files.contains(&"ABCD".to_string()),
        "multifile.nzb should contain alpha content ABCD"
    );
    assert!(
        all_files.contains(&"MNOP".to_string()),
        "multifile.nzb should contain beta content MNOP"
    );

    shutdown_app(rpc_addr).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), app_task)
        .await
        .expect("app shutdown timeout");

    stub_task.abort();
    let _ = stub_task.await;
}

#[tokio::test]
async fn graceful_shutdown_under_load() {
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
        delay_ms: 500,
    };
    let fixtures = load_fixtures(&fixtures_multiseg_path()).expect("fixtures load");
    let stub = StubServer::new(stub_config, fixtures);
    let stub_task = tokio::spawn(async move {
        let _ = stub.serve().await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;

    let config = sample_config(temp.path(), rpc_port, &[(stub_port, 0)]);
    create_dirs(&config).await;

    let stats = bergamot_scheduler::StatsTracker::from_config(&config);
    let shared_stats = Arc::new(bergamot_scheduler::SharedStatsTracker::new(stats));
    let fetcher = build_server_pool(&config, &shared_stats);

    let app_task = tokio::spawn(run_with_config_path(
        config.clone(),
        fetcher,
        None,
        None,
        Some(shared_stats),
    ));
    tokio::time::sleep(Duration::from_millis(200)).await;

    let nzb_id = append_nzb(rpc_addr, &multi_nzb_path()).await;

    tokio::time::sleep(Duration::from_millis(600)).await;

    let groups = jsonrpc_call(
        rpc_addr,
        "nzbget:secret",
        "listgroups",
        serde_json::json!([]),
    )
    .await;
    let still_active = groups.as_array().map(|a| !a.is_empty()).unwrap_or(false);
    assert!(
        still_active,
        "download should still be in progress when shutdown is issued"
    );

    shutdown_app(rpc_addr).await;
    let app_result = tokio::time::timeout(Duration::from_secs(10), app_task)
        .await
        .expect("app should shut down within timeout");
    assert!(app_result.is_ok(), "app task should complete without panic");

    let queue_json = temp.path().join("queue").join("queue.json");
    assert!(
        queue_json.exists(),
        "queue.json should be persisted after graceful shutdown"
    );

    let queue_data = tokio::fs::read_to_string(&queue_json)
        .await
        .expect("read queue.json");
    let queue_val: serde_json::Value =
        serde_json::from_str(&queue_data).expect("queue.json should be valid JSON");
    let nzbs = queue_val
        .get("nzbs")
        .and_then(|v| v.as_array())
        .expect("queue.json should contain nzbs array");
    assert!(
        !nzbs.is_empty(),
        "queue.json should contain the in-progress NZB"
    );

    let rpc_port2 = available_port();
    let rpc_addr2 = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), rpc_port2);

    let config2 = sample_config(temp.path(), rpc_port2, &[(stub_port, 0)]);

    let stats2 = bergamot_scheduler::StatsTracker::from_config(&config2);
    let shared_stats2 = Arc::new(bergamot_scheduler::SharedStatsTracker::new(stats2));
    let fetcher2 = build_server_pool(&config2, &shared_stats2);

    let app_task2 = tokio::spawn(run_with_config_path(
        config2.clone(),
        fetcher2,
        None,
        None,
        Some(shared_stats2),
    ));
    tokio::time::sleep(Duration::from_millis(300)).await;

    let completed = wait_for_completion(rpc_addr2, nzb_id, 80).await;
    assert!(
        completed,
        "NZB should complete after restart from graceful shutdown state"
    );

    let dest_dir = config2.dest_dir.clone();
    let inter_dir = config2.inter_dir.clone();
    let expected = "ABCDABCDABCDABCDABCDABCD";
    let content = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            for search_dir in [&dest_dir, &inter_dir] {
                if let Ok(mut entries) = tokio::fs::read_dir(search_dir).await {
                    while let Ok(Some(entry)) = entries.next_entry().await {
                        let path = entry.path();
                        if let Ok(c) = tokio::fs::read_to_string(&path).await
                            && c == expected
                        {
                            return c;
                        }
                        if path.is_dir()
                            && let Ok(mut files) = tokio::fs::read_dir(&path).await
                        {
                            while let Ok(Some(f)) = files.next_entry().await {
                                if let Ok(c) = tokio::fs::read_to_string(f.path()).await
                                    && c == expected
                                {
                                    return c;
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
    .expect("completed file should appear after restart");
    assert_eq!(content, expected);

    shutdown_app(rpc_addr2).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), app_task2)
        .await
        .expect("second app shutdown timeout");

    stub_task.abort();
    let _ = stub_task.await;
}

#[tokio::test]
async fn post_processing_par2_verify_and_move() {
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
        delay_ms: 0,
    };
    let fixtures = load_fixtures(&fixtures_par2_path()).expect("fixtures load");
    let stub = StubServer::new(stub_config, fixtures);
    let stub_task = tokio::spawn(async move {
        let _ = stub.serve().await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;

    let config = sample_config(temp.path(), rpc_port, &[(stub_port, 0)]);
    let dest_dir = config.dest_dir.clone();
    create_dirs(&config).await;

    let stats = bergamot_scheduler::StatsTracker::from_config(&config);
    let shared_stats = Arc::new(bergamot_scheduler::SharedStatsTracker::new(stats));
    let fetcher = build_server_pool(&config, &shared_stats);

    let app_task = tokio::spawn(run_with_config_path(
        config,
        fetcher,
        None,
        None,
        Some(shared_stats),
    ));
    tokio::time::sleep(Duration::from_millis(200)).await;

    let nzb_id = append_nzb(rpc_addr, &par2_nzb_path()).await;
    let completed = wait_for_completion(rpc_addr, nzb_id, 50).await;
    assert!(completed, "par2 nzb should complete downloading");

    let content = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(data) = tokio::fs::read(dest_dir.join("payload.dat")).await {
                return data;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("payload.dat should appear in dest dir after post-processing");
    assert_eq!(content, b"TESTDATA", "payload.dat content should match");

    assert!(
        dest_dir.join("payload.par2").is_file(),
        "payload.par2 should be moved to dest dir"
    );

    shutdown_app(rpc_addr).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), app_task)
        .await
        .expect("app shutdown timeout");

    stub_task.abort();
    let _ = stub_task.await;
}

#[tokio::test]
async fn rpc_editqueue_pause_resume_delete_move() {
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
        delay_ms: 500,
    };
    let fixtures = load_fixtures(&fixtures_multiseg_path()).expect("fixtures load");
    let stub = StubServer::new(stub_config, fixtures);
    let stub_task = tokio::spawn(async move {
        let _ = stub.serve().await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;

    let config = sample_config(temp.path(), rpc_port, &[(stub_port, 0)]);
    create_dirs(&config).await;

    let stats = bergamot_scheduler::StatsTracker::from_config(&config);
    let shared_stats = Arc::new(bergamot_scheduler::SharedStatsTracker::new(stats));
    let fetcher = build_server_pool(&config, &shared_stats);

    let app_task = tokio::spawn(run_with_config_path(
        config,
        fetcher,
        None,
        None,
        Some(shared_stats),
    ));
    tokio::time::sleep(Duration::from_millis(200)).await;

    let nzb_id_1 = append_nzb(rpc_addr, &multi_nzb_path()).await;
    let nzb_id_2 = append_nzb(rpc_addr, &sample_nzb_path()).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let pause_result = jsonrpc_call(
        rpc_addr,
        "nzbget:secret",
        "editqueue",
        serde_json::json!(["GroupPause", "", [nzb_id_1]]),
    )
    .await;
    assert!(
        pause_result.as_bool().unwrap_or(false),
        "GroupPause should succeed"
    );

    let groups = jsonrpc_call(
        rpc_addr,
        "nzbget:secret",
        "listgroups",
        serde_json::json!([]),
    )
    .await;
    let entries = groups.as_array().expect("listgroups array");
    let paused_entry = entries
        .iter()
        .find(|e| e.get("NZBID").and_then(|v| v.as_u64()) == Some(nzb_id_1))
        .expect("should find nzb_id_1");
    let paused_size = paused_entry
        .get("PausedSizeLo")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let status = paused_entry
        .get("Status")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(
        status == "PAUSED" || paused_size > 0,
        "entry should be paused after GroupPause, status={status} paused_size={paused_size}"
    );

    let resume_result = jsonrpc_call(
        rpc_addr,
        "nzbget:secret",
        "editqueue",
        serde_json::json!(["GroupResume", "", [nzb_id_1]]),
    )
    .await;
    assert!(
        resume_result.as_bool().unwrap_or(false),
        "GroupResume should succeed"
    );

    let groups_after = jsonrpc_call(
        rpc_addr,
        "nzbget:secret",
        "listgroups",
        serde_json::json!([]),
    )
    .await;
    let entries_after = groups_after.as_array().expect("listgroups array");
    if let Some(resumed_entry) = entries_after
        .iter()
        .find(|e| e.get("NZBID").and_then(|v| v.as_u64()) == Some(nzb_id_1))
    {
        let paused_size_after = resumed_entry
            .get("PausedSizeLo")
            .and_then(|v| v.as_u64())
            .unwrap_or(1);
        assert_eq!(paused_size_after, 0, "paused size should be 0 after resume");
    }

    let move_result = jsonrpc_call(
        rpc_addr,
        "nzbget:secret",
        "editqueue",
        serde_json::json!(["GroupMoveTop", "", [nzb_id_2]]),
    )
    .await;
    assert!(
        move_result.as_bool().unwrap_or(false),
        "GroupMoveTop should succeed"
    );

    let groups_moved = jsonrpc_call(
        rpc_addr,
        "nzbget:secret",
        "listgroups",
        serde_json::json!([]),
    )
    .await;
    let moved_entries = groups_moved.as_array().expect("listgroups array");
    if moved_entries.len() >= 2 {
        let first_id = moved_entries[0]
            .get("NZBID")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        assert_eq!(
            first_id, nzb_id_2,
            "nzb_id_2 should be first after GroupMoveTop"
        );
    }

    let delete_result = jsonrpc_call(
        rpc_addr,
        "nzbget:secret",
        "editqueue",
        serde_json::json!(["GroupDelete", "", [nzb_id_1, nzb_id_2]]),
    )
    .await;
    assert!(
        delete_result.as_bool().unwrap_or(false),
        "GroupDelete should succeed"
    );

    let groups_deleted = jsonrpc_call(
        rpc_addr,
        "nzbget:secret",
        "listgroups",
        serde_json::json!([]),
    )
    .await;
    let remaining = groups_deleted.as_array().expect("listgroups array");
    let still_present = remaining.iter().any(|e| {
        let id = e.get("NZBID").and_then(|v| v.as_u64()).unwrap_or(0);
        id == nzb_id_1 || id == nzb_id_2
    });
    assert!(
        !still_present,
        "deleted NZBs should no longer appear in listgroups"
    );

    shutdown_app(rpc_addr).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), app_task)
        .await
        .expect("app shutdown timeout");

    stub_task.abort();
    let _ = stub_task.await;
}

#[tokio::test]
async fn rpc_pausedownload_resumedownload() {
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

    let stats = bergamot_scheduler::StatsTracker::from_config(&config);
    let shared_stats = Arc::new(bergamot_scheduler::SharedStatsTracker::new(stats));
    let fetcher = build_server_pool(&config, &shared_stats);

    let app_task = tokio::spawn(run_with_config_path(
        config,
        fetcher,
        None,
        None,
        Some(shared_stats),
    ));
    tokio::time::sleep(Duration::from_millis(200)).await;

    let pause_result = jsonrpc_call(
        rpc_addr,
        "nzbget:secret",
        "pausedownload",
        serde_json::json!([]),
    )
    .await;
    assert!(
        pause_result.as_bool().unwrap_or(false),
        "pausedownload should succeed"
    );

    let status_paused =
        jsonrpc_call(rpc_addr, "nzbget:secret", "status", serde_json::json!([])).await;
    let download_paused = status_paused
        .get("DownloadPaused")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    assert!(
        download_paused,
        "DownloadPaused should be true after pausedownload"
    );

    let nzb_id = append_nzb(rpc_addr, &multi_nzb_path()).await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    let groups_while_paused = jsonrpc_call(
        rpc_addr,
        "nzbget:secret",
        "listgroups",
        serde_json::json!([]),
    )
    .await;
    let entries = groups_while_paused.as_array().expect("listgroups array");
    let nzb_entry = entries
        .iter()
        .find(|e| e.get("NZBID").and_then(|v| v.as_u64()) == Some(nzb_id));
    assert!(
        nzb_entry.is_some(),
        "NZB should still be in queue while paused"
    );

    let remaining_before = nzb_entry
        .unwrap()
        .get("RemainingSizeLo")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let resume_result = jsonrpc_call(
        rpc_addr,
        "nzbget:secret",
        "resumedownload",
        serde_json::json!([]),
    )
    .await;
    assert!(
        resume_result.as_bool().unwrap_or(false),
        "resumedownload should succeed"
    );

    let status_resumed =
        jsonrpc_call(rpc_addr, "nzbget:secret", "status", serde_json::json!([])).await;
    let download_paused_after = status_resumed
        .get("DownloadPaused")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    assert!(
        !download_paused_after,
        "DownloadPaused should be false after resumedownload"
    );

    let completed = wait_for_completion(rpc_addr, nzb_id, 80).await;
    assert!(completed, "NZB should complete after resumedownload");

    assert!(
        remaining_before > 0,
        "remaining size should have been > 0 while paused"
    );

    shutdown_app(rpc_addr).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), app_task)
        .await
        .expect("app shutdown timeout");

    stub_task.abort();
    let _ = stub_task.await;
}

#[tokio::test]
async fn rpc_rate_speed_limiting() {
    let temp = tempfile::tempdir().expect("tempdir");
    let stub_port = available_port();
    let rpc_port = available_port();
    let rpc_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), rpc_port);

    init_logging();

    let stub_task = start_stub(stub_port, fixtures_complete_path(), 2).await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    let config = sample_config(temp.path(), rpc_port, &[(stub_port, 0)]);
    create_dirs(&config).await;

    let stats = bergamot_scheduler::StatsTracker::from_config(&config);
    let shared_stats = Arc::new(bergamot_scheduler::SharedStatsTracker::new(stats));
    let fetcher = build_server_pool(&config, &shared_stats);

    let app_task = tokio::spawn(run_with_config_path(
        config,
        fetcher,
        None,
        None,
        Some(shared_stats),
    ));
    tokio::time::sleep(Duration::from_millis(200)).await;

    let set_result =
        jsonrpc_call(rpc_addr, "nzbget:secret", "rate", serde_json::json!([100])).await;
    assert!(set_result.as_bool().unwrap_or(false), "rate should succeed");

    tokio::time::sleep(Duration::from_millis(1200)).await;

    let status = jsonrpc_call(rpc_addr, "nzbget:secret", "status", serde_json::json!([])).await;
    let limit = status
        .get("DownloadLimit")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    assert_eq!(
        limit,
        100 * 1024,
        "DownloadLimit should be 100 KB/s in bytes/s"
    );

    let nzb_id = append_nzb(rpc_addr, &sample_nzb_path()).await;
    let completed = wait_for_completion(rpc_addr, nzb_id, 50).await;
    assert!(completed, "download should complete with rate limit set");

    let clear_result =
        jsonrpc_call(rpc_addr, "nzbget:secret", "rate", serde_json::json!([0])).await;
    assert!(
        clear_result.as_bool().unwrap_or(false),
        "clearing rate should succeed"
    );

    tokio::time::sleep(Duration::from_millis(1200)).await;

    let status_after =
        jsonrpc_call(rpc_addr, "nzbget:secret", "status", serde_json::json!([])).await;
    let limit_after = status_after
        .get("DownloadLimit")
        .and_then(|v| v.as_u64())
        .unwrap_or(1);
    assert_eq!(limit_after, 0, "DownloadLimit should be 0 after clearing");

    shutdown_app(rpc_addr).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), app_task)
        .await
        .expect("app shutdown timeout");

    let _ = tokio::time::timeout(Duration::from_secs(2), stub_task).await;
}

#[tokio::test]
async fn error_invalid_nzb_returns_rpc_error() {
    let temp = tempfile::tempdir().expect("tempdir");
    let stub_port = available_port();
    let rpc_port = available_port();
    let rpc_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), rpc_port);

    init_logging();

    let stub_task = start_stub(stub_port, fixtures_complete_path(), 1).await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    let config = sample_config(temp.path(), rpc_port, &[(stub_port, 0)]);
    create_dirs(&config).await;

    let stats = bergamot_scheduler::StatsTracker::from_config(&config);
    let shared_stats = Arc::new(bergamot_scheduler::SharedStatsTracker::new(stats));
    let fetcher = build_server_pool(&config, &shared_stats);

    let app_task = tokio::spawn(run_with_config_path(
        config,
        fetcher,
        None,
        None,
        Some(shared_stats),
    ));
    tokio::time::sleep(Duration::from_millis(200)).await;

    let garbage = base64::engine::general_purpose::STANDARD.encode(b"this is not valid xml at all");
    let response = jsonrpc_call_full(
        rpc_addr,
        "nzbget:secret",
        "append",
        serde_json::json!(["bad.nzb", garbage, "", 0]),
    )
    .await;
    assert!(
        response.get("error").is_some(),
        "invalid NZB should produce a JSON-RPC error, got: {response}"
    );
    assert!(
        response.get("result").is_none() || response["result"].is_null(),
        "invalid NZB should not produce a result"
    );

    let empty_nzb =
        r#"<?xml version="1.0"?><nzb xmlns="http://www.newzbin.com/DTD/2003/nzb"></nzb>"#;
    let empty_b64 = base64::engine::general_purpose::STANDARD.encode(empty_nzb.as_bytes());
    let response2 = jsonrpc_call_full(
        rpc_addr,
        "nzbget:secret",
        "append",
        serde_json::json!(["empty.nzb", empty_b64, "", 0]),
    )
    .await;
    assert!(
        response2.get("error").is_some(),
        "empty NZB (no files) should produce a JSON-RPC error, got: {response2}"
    );

    shutdown_app(rpc_addr).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), app_task)
        .await
        .expect("app shutdown timeout");

    let _ = tokio::time::timeout(Duration::from_secs(2), stub_task).await;
}

#[tokio::test]
async fn error_all_articles_missing_produces_failure_history() {
    let temp = tempfile::tempdir().expect("tempdir");
    let stub_port = available_port();
    let rpc_port = available_port();
    let rpc_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), rpc_port);

    init_logging();

    let stub_task = start_stub(stub_port, fixtures_all_missing_path(), 2).await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    let config = sample_config(temp.path(), rpc_port, &[(stub_port, 0)]);
    create_dirs(&config).await;

    let stats = bergamot_scheduler::StatsTracker::from_config(&config);
    let shared_stats = Arc::new(bergamot_scheduler::SharedStatsTracker::new(stats));
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
    assert!(
        completed,
        "NZB with all missing articles should still complete (move to history)"
    );

    tokio::time::sleep(Duration::from_millis(500)).await;

    let history = jsonrpc_call(
        rpc_addr,
        "nzbget:secret",
        "history",
        serde_json::json!([false]),
    )
    .await;
    let entries = history.as_array().expect("history array");
    let entry = entries
        .iter()
        .find(|e| e.get("NZBID").and_then(|v| v.as_u64()) == Some(nzb_id))
        .expect("should find NZB in history");

    let health = entry.get("Health").and_then(|v| v.as_u64()).unwrap_or(1000);
    assert!(
        health < 1000,
        "health should be below 1000 when articles failed, got {health}"
    );

    shutdown_app(rpc_addr).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), app_task)
        .await
        .expect("app shutdown timeout");

    let _ = tokio::time::timeout(Duration::from_secs(2), stub_task).await;
}

#[tokio::test]
async fn error_all_servers_down_produces_failure() {
    let temp = tempfile::tempdir().expect("tempdir");
    let dead_port = available_port();
    let rpc_port = available_port();
    let rpc_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), rpc_port);

    init_logging();

    let config = sample_config(temp.path(), rpc_port, &[(dead_port, 0)]);
    create_dirs(&config).await;

    let stats = bergamot_scheduler::StatsTracker::from_config(&config);
    let shared_stats = Arc::new(bergamot_scheduler::SharedStatsTracker::new(stats));
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
    let completed = wait_for_completion(rpc_addr, nzb_id, 60).await;
    assert!(
        completed,
        "NZB should eventually complete even when servers are unreachable"
    );

    tokio::time::sleep(Duration::from_millis(500)).await;

    let history = jsonrpc_call(
        rpc_addr,
        "nzbget:secret",
        "history",
        serde_json::json!([false]),
    )
    .await;
    let entries = history.as_array().expect("history array");
    let entry = entries
        .iter()
        .find(|e| e.get("NZBID").and_then(|v| v.as_u64()) == Some(nzb_id))
        .expect("should find NZB in history");

    let health = entry.get("Health").and_then(|v| v.as_u64()).unwrap_or(1000);
    assert!(
        health < 1000,
        "health should be below 1000 when all servers are down, got {health}"
    );

    shutdown_app(rpc_addr).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), app_task)
        .await
        .expect("app shutdown timeout");
}

#[tokio::test]
async fn extension_script_runs_during_post_processing() {
    let temp = tempfile::tempdir().expect("tempdir");
    let stub_port = available_port();
    let rpc_port = available_port();
    let rpc_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), rpc_port);

    init_logging();

    let stub_task = start_stub(stub_port, fixtures_complete_path(), 1).await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    let config = sample_config(temp.path(), rpc_port, &[(stub_port, 0)]);
    let script_dir = config.script_dir.clone();
    create_dirs(&config).await;
    tokio::fs::create_dir_all(&script_dir)
        .await
        .expect("script dir");

    let marker_path = temp.path().join("extension_ran.marker");
    let script_path = script_dir.join("TestExtension.sh");
    let script_content = format!(
        "#!/bin/sh\necho \"[INFO] extension running\"\ntouch \"{}\"\nexit 93\n",
        marker_path.display()
    );
    tokio::fs::write(&script_path, &script_content)
        .await
        .expect("write script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))
            .await
            .expect("chmod script");
    }

    let stats = bergamot_scheduler::StatsTracker::from_config(&config);
    let shared_stats = Arc::new(bergamot_scheduler::SharedStatsTracker::new(stats));
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
    assert!(completed, "NZB should complete downloading");

    let marker_found = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if marker_path.exists() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("extension marker file should appear within timeout");
    assert!(
        marker_found,
        "extension script should have created marker file"
    );

    shutdown_app(rpc_addr).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), app_task)
        .await
        .expect("app shutdown timeout");

    stub_task.abort();
    let _ = stub_task.await;
}

fn sample_config_with_overrides(
    root: &Path,
    rpc_port: u16,
    servers: &[(u16, u32)],
    overrides: &[(&str, &str)],
) -> Config {
    let main_dir = root.join("main");
    let dest_dir = root.join("dest");
    let inter_dir = root.join("intermediate");
    let nzb_dir = root.join("nzb");
    let queue_dir = root.join("queue");
    let temp_dir = root.join("tmp");
    let script_dir = root.join("scripts");
    let log_file = root.join("bergamot.log");

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

    for (key, value) in overrides {
        raw.insert(key.to_string(), value.to_string());
    }

    Config::from_raw(raw)
}

#[tokio::test]
async fn rpc_servervolumes_reports_bytes_after_download() {
    let temp = tempfile::tempdir().expect("tempdir");
    let stub_port = available_port();
    let rpc_port = available_port();
    let rpc_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), rpc_port);

    init_logging();

    let stub_task = start_stub(stub_port, fixtures_complete_path(), 1).await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    let config = sample_config(temp.path(), rpc_port, &[(stub_port, 0)]);
    create_dirs(&config).await;

    let config_path = temp.path().join("bergamot.conf");
    tokio::fs::write(&config_path, "")
        .await
        .expect("write config file");

    let stats = bergamot_scheduler::StatsTracker::from_config(&config);
    let shared_stats = Arc::new(bergamot_scheduler::SharedStatsTracker::new(stats));
    let fetcher = build_server_pool(&config, &shared_stats);

    let app_task = tokio::spawn(run_with_config_path(
        config,
        fetcher,
        Some(config_path),
        None,
        Some(shared_stats),
    ));
    tokio::time::sleep(Duration::from_millis(200)).await;

    let nzb_id = append_nzb(rpc_addr, &sample_nzb_path()).await;
    let completed = wait_for_completion(rpc_addr, nzb_id, 50).await;
    assert!(completed, "NZB should complete downloading");

    tokio::time::sleep(Duration::from_millis(500)).await;

    let volumes = jsonrpc_call(
        rpc_addr,
        "nzbget:secret",
        "servervolumes",
        serde_json::json!([]),
    )
    .await;
    let volumes_arr = volumes
        .as_array()
        .expect("servervolumes should return an array");
    assert!(
        volumes_arr.len() >= 2,
        "servervolumes should have at least 2 entries (aggregate + 1 server), got {}",
        volumes_arr.len()
    );
    assert_eq!(
        volumes_arr[0]["ServerID"].as_u64().unwrap(),
        0,
        "first entry should be the aggregate (ServerID 0)"
    );
    assert_eq!(
        volumes_arr[1]["ServerID"].as_u64().unwrap(),
        1,
        "second entry should be server 1"
    );
    let total_lo = volumes_arr[1]["TotalSizeLo"].as_u64().unwrap_or(0);
    let total_hi = volumes_arr[1]["TotalSizeHi"].as_u64().unwrap_or(0);
    let total_bytes = total_lo + (total_hi << 32);
    assert!(
        total_bytes > 0,
        "server 1 should have transferred bytes, got {total_bytes}"
    );

    shutdown_app(rpc_addr).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), app_task)
        .await
        .expect("app shutdown timeout");

    stub_task.abort();
    let _ = stub_task.await;
}

#[tokio::test]
async fn rpc_postqueue_schema_and_pause_toggle() {
    let temp = tempfile::tempdir().expect("tempdir");
    let stub_port = available_port();
    let rpc_port = available_port();
    let rpc_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), rpc_port);

    init_logging();

    let stub_task = start_stub(stub_port, fixtures_complete_path(), 0).await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    let config = sample_config(temp.path(), rpc_port, &[(stub_port, 0)]);
    create_dirs(&config).await;

    let stats = bergamot_scheduler::StatsTracker::from_config(&config);
    let shared_stats = Arc::new(bergamot_scheduler::SharedStatsTracker::new(stats));
    let fetcher = build_server_pool(&config, &shared_stats);

    let app_task = tokio::spawn(run_with_config_path(
        config,
        fetcher,
        None,
        None,
        Some(shared_stats),
    ));
    tokio::time::sleep(Duration::from_millis(200)).await;

    let pq = jsonrpc_call(
        rpc_addr,
        "nzbget:secret",
        "postqueue",
        serde_json::json!([]),
    )
    .await;
    assert!(
        pq.get("Paused").is_some(),
        "postqueue should have Paused field"
    );
    assert!(pq.get("Jobs").is_some(), "postqueue should have Jobs field");
    assert!(pq["Jobs"].is_array(), "Jobs should be an array");

    let pause_result = jsonrpc_call(
        rpc_addr,
        "nzbget:secret",
        "pausepost",
        serde_json::json!([]),
    )
    .await;
    assert!(
        pause_result.as_bool().unwrap_or(false),
        "pausepost should return true"
    );

    let pq_paused = jsonrpc_call(
        rpc_addr,
        "nzbget:secret",
        "postqueue",
        serde_json::json!([]),
    )
    .await;
    assert_eq!(
        pq_paused["Paused"].as_bool(),
        Some(true),
        "postqueue Paused should be true after pausepost"
    );

    let resume_result = jsonrpc_call(
        rpc_addr,
        "nzbget:secret",
        "resumepost",
        serde_json::json!([]),
    )
    .await;
    assert!(
        resume_result.as_bool().unwrap_or(false),
        "resumepost should return true"
    );

    let pq_resumed = jsonrpc_call(
        rpc_addr,
        "nzbget:secret",
        "postqueue",
        serde_json::json!([]),
    )
    .await;
    assert_eq!(
        pq_resumed["Paused"].as_bool(),
        Some(false),
        "postqueue Paused should be false after resumepost"
    );

    shutdown_app(rpc_addr).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), app_task)
        .await
        .expect("app shutdown timeout");

    stub_task.abort();
    let _ = stub_task.await;
}

#[tokio::test]
async fn scan_nzb_dropped_into_nzbdir_gets_queued() {
    let temp = tempfile::tempdir().expect("tempdir");
    let stub_port = available_port();
    let rpc_port = available_port();
    let rpc_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), rpc_port);

    init_logging();

    let stub_task = start_stub(stub_port, fixtures_complete_path(), 1).await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    let config = sample_config_with_overrides(
        temp.path(),
        rpc_port,
        &[(stub_port, 0)],
        &[("NzbDirFileAge", "0"), ("NzbDirInterval", "1")],
    );
    let nzb_dir = config.nzb_dir.clone();
    create_dirs(&config).await;

    let stats = bergamot_scheduler::StatsTracker::from_config(&config);
    let shared_stats = Arc::new(bergamot_scheduler::SharedStatsTracker::new(stats));
    let fetcher = build_server_pool(&config, &shared_stats);

    let app_task = tokio::spawn(run_with_config_path(
        config,
        fetcher,
        None,
        None,
        Some(shared_stats),
    ));
    tokio::time::sleep(Duration::from_millis(200)).await;

    let src_nzb = tokio::fs::read(sample_nzb_path())
        .await
        .expect("read sample nzb");
    let dropped_path = nzb_dir.join("scan-test.nzb");
    tokio::fs::write(&dropped_path, &src_nzb)
        .await
        .expect("write scan-test.nzb");

    let mut found = false;
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let groups = jsonrpc_call(
            rpc_addr,
            "nzbget:secret",
            "listgroups",
            serde_json::json!([]),
        )
        .await;
        if groups.as_array().is_some_and(|arr| !arr.is_empty()) {
            found = true;
            break;
        }
        let queued_path = nzb_dir.join("scan-test.nzb.queued");
        if queued_path.exists() {
            found = true;
            break;
        }
    }
    assert!(found, "scanner should pick up the NZB dropped into nzb_dir");

    let queued_path = nzb_dir.join("scan-test.nzb.queued");
    let original_gone = !dropped_path.exists() || queued_path.exists();
    assert!(
        original_gone,
        "scan-test.nzb should have been renamed to .nzb.queued"
    );

    shutdown_app(rpc_addr).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), app_task)
        .await
        .expect("app shutdown timeout");

    stub_task.abort();
    let _ = stub_task.await;
}

#[tokio::test]
async fn xmlrpc_version_returns_valid_response() {
    let temp = tempfile::tempdir().expect("tempdir");
    let stub_port = available_port();
    let rpc_port = available_port();
    let rpc_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), rpc_port);

    init_logging();

    let stub_task = start_stub(stub_port, fixtures_complete_path(), 0).await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    let config = sample_config(temp.path(), rpc_port, &[(stub_port, 0)]);
    create_dirs(&config).await;

    let stats = bergamot_scheduler::StatsTracker::from_config(&config);
    let shared_stats = Arc::new(bergamot_scheduler::SharedStatsTracker::new(stats));
    let fetcher = build_server_pool(&config, &shared_stats);

    let app_task = tokio::spawn(run_with_config_path(
        config,
        fetcher,
        None,
        None,
        Some(shared_stats),
    ));
    tokio::time::sleep(Duration::from_millis(200)).await;

    let auth = base64::engine::general_purpose::STANDARD.encode("nzbget:secret");
    let client = reqwest::Client::new();

    let version_xml = r#"<?xml version="1.0"?><methodCall><methodName>version</methodName><params></params></methodCall>"#;
    let resp = timeout(
        Duration::from_secs(3),
        client
            .post(format!("http://{rpc_addr}/xmlrpc"))
            .header("Authorization", format!("Basic {auth}"))
            .header("Content-Type", "text/xml")
            .body(version_xml)
            .send(),
    )
    .await
    .expect("xmlrpc timeout")
    .expect("xmlrpc send");

    assert_eq!(
        resp.status().as_u16(),
        200,
        "xmlrpc version should return 200"
    );
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        content_type.contains("text/xml"),
        "content-type should be text/xml, got {content_type}"
    );
    let body = resp.text().await.expect("xmlrpc body");
    assert!(
        body.contains("<methodResponse>"),
        "response should contain <methodResponse>"
    );
    assert!(
        body.contains("<params>"),
        "response should contain <params>"
    );
    assert!(
        body.contains("<string>"),
        "response should contain <string>"
    );

    let status_xml = r#"<?xml version="1.0"?><methodCall><methodName>status</methodName><params></params></methodCall>"#;
    let resp2 = timeout(
        Duration::from_secs(3),
        client
            .post(format!("http://{rpc_addr}/xmlrpc"))
            .header("Authorization", format!("Basic {auth}"))
            .header("Content-Type", "text/xml")
            .body(status_xml)
            .send(),
    )
    .await
    .expect("xmlrpc status timeout")
    .expect("xmlrpc status send");

    assert_eq!(
        resp2.status().as_u16(),
        200,
        "xmlrpc status should return 200"
    );
    let body2 = resp2.text().await.expect("xmlrpc status body");
    assert!(
        body2.contains("<struct>"),
        "status response should contain <struct>"
    );
    assert!(
        body2.contains("<name>DownloadRate</name>"),
        "status response should contain DownloadRate member"
    );

    shutdown_app(rpc_addr).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), app_task)
        .await
        .expect("app shutdown timeout");

    stub_task.abort();
    let _ = stub_task.await;
}

const TEST_RSS_FEED: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <item>
      <title>Good.Show.S01E01.720p</title>
      <link>https://example.com/1.nzb</link>
    </item>
    <item>
      <title>Bad.Show.S01E01.720p</title>
      <link>https://example.com/2.nzb</link>
    </item>
    <item>
      <title>Good.Show.S01E02.720p</title>
      <link>https://example.com/3.nzb</link>
    </item>
  </channel>
</rss>"#;

async fn start_http_stub(port: u16, body: &'static str) -> JoinHandle<()> {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("bind http stub");
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let body = body;
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = vec![0u8; 4096];
                let _ = stream.read(&mut buf).await;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/rss+xml\r\nContent-Length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    })
}

#[tokio::test]
async fn feed_polling_via_rss_stub() {
    let temp = tempfile::tempdir().expect("tempdir");
    let stub_port = available_port();
    let http_port = available_port();
    let rpc_port = available_port();
    let rpc_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), rpc_port);

    init_logging();

    let stub_task = start_stub(stub_port, fixtures_complete_path(), 0).await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    let http_task = start_http_stub(http_port, TEST_RSS_FEED).await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    let feed_url = format!("http://127.0.0.1:{http_port}/rss");
    let now = chrono::Local::now();
    let task_time = now.format("%H:%M").to_string();
    let next_min = (now + chrono::Duration::minutes(1))
        .format("%H:%M")
        .to_string();
    let config = sample_config_with_overrides(
        temp.path(),
        rpc_port,
        &[(stub_port, 0)],
        &[
            ("Feed1.Name", "Test Feed"),
            ("Feed1.URL", &feed_url),
            ("Feed1.Filter", "A: title(Good.*)"),
            ("Feed1.Interval", "1"),
            ("Feed1.Category", "TV"),
            ("Task1.Time", &task_time),
            ("Task1.WeekDays", "1-7"),
            ("Task1.Command", "FetchFeed"),
            ("Task1.Param", "1"),
            ("Task2.Time", &next_min),
            ("Task2.WeekDays", "1-7"),
            ("Task2.Command", "FetchFeed"),
            ("Task2.Param", "1"),
        ],
    );
    create_dirs(&config).await;

    let stats = bergamot_scheduler::StatsTracker::from_config(&config);
    let shared_stats = Arc::new(bergamot_scheduler::SharedStatsTracker::new(stats));
    let fetcher = build_server_pool(&config, &shared_stats);

    let app_task = tokio::spawn(run_with_config_path(
        config,
        fetcher,
        None,
        None,
        Some(shared_stats),
    ));
    tokio::time::sleep(Duration::from_millis(200)).await;

    let feeds = jsonrpc_call(rpc_addr, "nzbget:secret", "feeds", serde_json::json!([])).await;
    let feeds_arr = feeds.as_array().expect("feeds should return an array");
    assert!(
        !feeds_arr.is_empty(),
        "should have at least one feed configured"
    );
    assert_eq!(
        feeds_arr[0]["Name"].as_str(),
        Some("Test Feed"),
        "feed name should match"
    );

    let mut item_count = 0u64;
    for _ in 0..120 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let feeds = jsonrpc_call(rpc_addr, "nzbget:secret", "feeds", serde_json::json!([])).await;
        if let Some(arr) = feeds.as_array() {
            if let Some(feed) = arr.first() {
                item_count = feed["ItemCount"].as_u64().unwrap_or(0);
                if item_count >= 2 {
                    break;
                }
            }
        }
    }
    assert_eq!(
        item_count, 2,
        "feed should have matched 2 items (Good.Show.*), got {item_count}"
    );

    let feeds_final = jsonrpc_call(rpc_addr, "nzbget:secret", "feeds", serde_json::json!([])).await;
    let feed_entry = &feeds_final.as_array().expect("feeds array")[0];
    assert!(
        feed_entry["Error"].is_null(),
        "feed should have no error, got {:?}",
        feed_entry["Error"]
    );

    shutdown_app(rpc_addr).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), app_task)
        .await
        .expect("app shutdown timeout");

    stub_task.abort();
    let _ = stub_task.await;
    http_task.abort();
    let _ = http_task.await;
}
