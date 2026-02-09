use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

use bergamot_nntp::{Encryption, NewsServer, NntpError, ServerPool};
use bergamot_nntp_stub::{StubConfig, StubServer, load_fixtures};

fn fixtures_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fixtures")
        .join("nntp")
        .join("fixtures-basic.json")
}

fn test_server(port: u16) -> NewsServer {
    NewsServer {
        id: 1,
        name: "stub".to_string(),
        active: true,
        host: "127.0.0.1".to_string(),
        port,
        username: None,
        password: None,
        encryption: Encryption::None,
        cipher: None,
        connections: 2,
        retention: 0,
        level: 0,
        optional: false,
        group: 0,
        join_group: true,
        ip_version: bergamot_nntp::IpVersion::IPv4Only,
        cert_verification: true,
    }
}

fn test_server_with_auth(port: u16) -> NewsServer {
    NewsServer {
        username: Some("test".to_string()),
        password: Some("secret".to_string()),
        ..test_server(port)
    }
}

fn available_port() -> u16 {
    let socket = std::net::TcpListener::bind("127.0.0.1:0").expect("bind port");
    socket.local_addr().expect("local addr").port()
}

#[tokio::test]
async fn fetches_body_from_stub() {
    let port = available_port();
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

    let server_task = tokio::spawn(async move { server.serve_once().await.expect("serve once") });

    tokio::time::sleep(Duration::from_millis(20)).await;

    let pool = ServerPool::new(vec![test_server(port)]);
    let data = pool
        .fetch_article("segment-1@test", &["alt.test".to_string()])
        .await
        .expect("fetch article");

    let body = String::from_utf8_lossy(&data);
    assert!(body.contains("=ybegin"));
    assert!(body.contains("=yend"));

    pool.cleanup_idle_connections(Duration::from_secs(0)).await;
    tokio::time::timeout(Duration::from_secs(2), server_task)
        .await
        .expect("server task timeout")
        .expect("server task");
}

#[tokio::test]
async fn fetches_body_with_auth() {
    let port = available_port();
    let bind = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    let fixtures = load_fixtures(&fixtures_path()).expect("fixtures load");

    let config = StubConfig {
        bind,
        require_auth: true,
        username: "test".to_string(),
        password: "secret".to_string(),
        disconnect_after: 0,
        delay_ms: 0,
    };
    let server = StubServer::new(config, fixtures);

    let server_task = tokio::spawn(async move { server.serve_once().await.expect("serve once") });

    tokio::time::sleep(Duration::from_millis(20)).await;

    let pool = ServerPool::new(vec![test_server_with_auth(port)]);
    let data = pool
        .fetch_article("segment-1@test", &["alt.test".to_string()])
        .await
        .expect("fetch article");

    let body = String::from_utf8_lossy(&data);
    assert!(body.contains("=ybegin"));
    assert!(body.contains("=yend"));

    pool.cleanup_idle_connections(Duration::from_secs(0)).await;
    tokio::time::timeout(Duration::from_secs(2), server_task)
        .await
        .expect("server task timeout")
        .expect("server task");
}

#[tokio::test]
async fn reports_missing_article() {
    let port = available_port();
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

    let server_task = tokio::spawn(async move { server.serve_once().await.expect("serve once") });

    tokio::time::sleep(Duration::from_millis(20)).await;

    let pool = ServerPool::new(vec![test_server(port)]);
    let err = pool
        .fetch_article("segment-2@test", &["alt.test".to_string()])
        .await
        .expect_err("missing article");

    assert!(matches!(err, NntpError::ArticleNotFound(_)));

    pool.cleanup_idle_connections(Duration::from_secs(0)).await;
    tokio::time::timeout(Duration::from_secs(2), server_task)
        .await
        .expect("server task timeout")
        .expect("server task");
}

#[tokio::test]
async fn handles_disconnects() {
    let port = available_port();
    let bind = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    let fixtures = load_fixtures(&fixtures_path()).expect("fixtures load");

    let config = StubConfig {
        bind,
        require_auth: false,
        username: "test".to_string(),
        password: "secret".to_string(),
        disconnect_after: 2,
        delay_ms: 0,
    };
    let server = StubServer::new(config, fixtures);

    let server_task = tokio::spawn(async move { server.serve_once().await.expect("serve once") });

    tokio::time::sleep(Duration::from_millis(20)).await;

    let pool = ServerPool::new(vec![test_server(port)]);
    let err = pool
        .fetch_article("segment-1@test", &["alt.test".to_string()])
        .await
        .expect_err("disconnect error");

    assert!(matches!(err, NntpError::ProtocolError(_)));

    pool.cleanup_idle_connections(Duration::from_secs(0)).await;
    tokio::time::timeout(Duration::from_secs(2), server_task)
        .await
        .expect("server task timeout")
        .expect("server task");
}

#[tokio::test]
async fn handles_response_delays() {
    let port = available_port();
    let bind = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    let fixtures = load_fixtures(&fixtures_path()).expect("fixtures load");

    let config = StubConfig {
        bind,
        require_auth: false,
        username: "test".to_string(),
        password: "secret".to_string(),
        disconnect_after: 0,
        delay_ms: 50,
    };
    let server = StubServer::new(config, fixtures);

    let server_task = tokio::spawn(async move { server.serve_once().await.expect("serve once") });

    tokio::time::sleep(Duration::from_millis(20)).await;

    let pool = ServerPool::new(vec![test_server(port)]);
    let data = tokio::time::timeout(
        Duration::from_secs(2),
        pool.fetch_article("segment-1@test", &["alt.test".to_string()]),
    )
    .await
    .expect("fetch timeout")
    .expect("fetch article");

    let body = String::from_utf8_lossy(&data);
    assert!(body.contains("=ybegin"));

    pool.cleanup_idle_connections(Duration::from_secs(0)).await;
    tokio::time::timeout(Duration::from_secs(2), server_task)
        .await
        .expect("server task timeout")
        .expect("server task");
}
