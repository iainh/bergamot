use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine;
use nzbg::app::{init_tracing, run_with_config_path};
use nzbg::download::NntpPoolFetcher;
use nzbg_config::Config;
use nzbg_nntp::{Encryption, NewsServer, ServerPool};
use nzbg_nntp_stub::{StubConfig, StubServer, load_fixtures};
use tokio::task::JoinHandle;
use tokio::time::timeout;

fn fixtures_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fixtures")
        .join("nntp")
        .join("fixtures-complete.json")
}

fn sample_nzb_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fixtures")
        .join("nntp")
        .join("sample.nzb")
}

fn available_port() -> u16 {
    let socket = std::net::TcpListener::bind("127.0.0.1:0").expect("bind port");
    socket.local_addr().expect("local addr").port()
}

async fn start_stub(port: u16) -> JoinHandle<()> {
    let bind = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    let fixtures = load_fixtures(&fixtures_path()).expect("fixtures load");
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
        server.serve_once().await.expect("serve once");
    })
}

fn sample_config(root: &Path, stub_port: u16, rpc_port: u16) -> Config {
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
    raw.insert("Server1.Active".to_string(), "yes".to_string());
    raw.insert("Server1.Name".to_string(), "stub".to_string());
    raw.insert("Server1.Host".to_string(), "127.0.0.1".to_string());
    raw.insert("Server1.Port".to_string(), stub_port.to_string());
    raw.insert("Server1.Encryption".to_string(), "no".to_string());
    raw.insert("Server1.Connections".to_string(), "1".to_string());
    raw.insert("Server1.Retention".to_string(), "0".to_string());
    raw.insert("Server1.Level".to_string(), "0".to_string());
    raw.insert("Server1.Optional".to_string(), "no".to_string());
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

#[tokio::test]
async fn end_to_end_append_download_flow() {
    let temp = tempfile::tempdir().expect("tempdir");
    let stub_port = available_port();
    let rpc_port = available_port();

    let stub_task = start_stub(stub_port).await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    let config = sample_config(temp.path(), stub_port, rpc_port);
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

    let _log_buffer = init_tracing("debug");
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
