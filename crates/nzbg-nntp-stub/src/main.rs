use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

#[derive(Parser, Debug)]
#[command(
    name = "nzbg-nntp-stub",
    about = "Minimal NNTP stub server for integration tests"
)]
struct Args {
    #[arg(long, default_value = "127.0.0.1:3119")]
    bind: SocketAddr,

    #[arg(long, default_value = "fixtures/nntp/fixtures.json")]
    fixtures: PathBuf,

    #[arg(long, default_value_t = false)]
    require_auth: bool,

    #[arg(long, default_value = "test")]
    username: String,

    #[arg(long, default_value = "secret")]
    password: String,

    #[arg(long, default_value_t = 0)]
    disconnect_after: usize,

    #[arg(long, default_value_t = 0)]
    delay_ms: u64,
}

#[derive(Debug, Deserialize, Clone)]
struct FixtureConfig {
    greeting: Option<String>,
    groups: HashMap<String, GroupConfig>,
}

#[derive(Debug, Deserialize, Clone)]
struct GroupConfig {
    article_ids: Vec<String>,
    articles: HashMap<String, String>,
    missing_articles: Option<Vec<String>>,
}

#[derive(Debug)]
struct SessionState {
    authenticated: bool,
    current_group: Option<String>,
    commands_seen: usize,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let fixtures = load_fixtures(&args.fixtures)?;
    let server = Arc::new(ServerState::new(args, fixtures));

    let listener = TcpListener::bind(server.args.bind).await?;
    println!("NNTP stub listening on {}", server.args.bind);

    loop {
        let (stream, _) = listener.accept().await?;
        let server = Arc::clone(&server);
        tokio::spawn(async move {
            if let Err(err) = handle_client(stream, server).await {
                eprintln!("client error: {err}");
            }
        });
    }
}

struct ServerState {
    args: Args,
    fixtures: FixtureConfig,
    auth_attempts: Mutex<usize>,
}

impl ServerState {
    fn new(args: Args, fixtures: FixtureConfig) -> Self {
        Self {
            args,
            fixtures,
            auth_attempts: Mutex::new(0),
        }
    }
}

async fn handle_client(
    stream: TcpStream,
    server: Arc<ServerState>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    let greeting = server
        .fixtures
        .greeting
        .clone()
        .unwrap_or_else(|| "200 nzbg test server ready".to_string());
    writer
        .write_all(format!("{greeting}\r\n").as_bytes())
        .await?;

    let mut state = SessionState {
        authenticated: !server.args.require_auth,
        current_group: None,
        commands_seen: 0,
    };

    loop {
        let mut line = String::new();
        let bytes = reader.read_line(&mut line).await?;
        if bytes == 0 {
            break;
        }

        let command_line = line.trim();
        if command_line.is_empty() {
            continue;
        }

        state.commands_seen += 1;
        maybe_delay(&server.args).await;
        if should_disconnect(&server.args, &state) {
            return Ok(());
        }

        let mut parts = command_line.split_whitespace();
        let command = parts.next().unwrap_or("").to_uppercase();
        match command.as_str() {
            "QUIT" => {
                writer.write_all(b"205 closing connection\r\n").await?;
                break;
            }
            "CAPABILITIES" => {
                writer.write_all(b"101 capability list follows\r\n").await?;
                writer.write_all(b"VERSION 2\r\n").await?;
                writer.write_all(b"READER\r\n").await?;
                writer.write_all(b"STARTTLS\r\n").await?;
                writer.write_all(b".\r\n").await?;
            }
            "AUTHINFO" => {
                handle_authinfo(parts.collect::<Vec<_>>(), &server, &mut state, &mut writer)
                    .await?;
            }
            "GROUP" => {
                let group = parts.next().unwrap_or("");
                handle_group(group, &server, &mut state, &mut writer).await?;
            }
            "STAT" => {
                let message_id = extract_message_id(command_line, "STAT");
                handle_stat(&message_id, &server, &state, &mut writer).await?;
            }
            "BODY" => {
                let message_id = extract_message_id(command_line, "BODY");
                handle_body(&message_id, &server, &state, &mut writer).await?;
            }
            "HELP" => {
                writer.write_all(b"100 help text follows\r\n").await?;
                writer
                    .write_all(b"Supported commands: AUTHINFO, GROUP, STAT, BODY, QUIT\r\n")
                    .await?;
                writer.write_all(b".\r\n").await?;
            }
            _ => {
                writer.write_all(b"500 command not recognized\r\n").await?;
            }
        }
    }

    Ok(())
}

async fn handle_authinfo(
    args: Vec<&str>,
    server: &Arc<ServerState>,
    state: &mut SessionState,
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
) -> Result<(), Box<dyn std::error::Error>> {
    if args.len() < 2 {
        writer.write_all(b"501 syntax error\r\n").await?;
        return Ok(());
    }

    let verb = args[0].to_uppercase();
    let value = args[1];

    match verb.as_str() {
        "USER" => {
            let mut attempts = server.auth_attempts.lock().await;
            *attempts += 1;
            if value == server.args.username {
                writer.write_all(b"381 password required\r\n").await?;
            } else {
                writer.write_all(b"481 authentication rejected\r\n").await?;
            }
        }
        "PASS" => {
            if value == server.args.password {
                state.authenticated = true;
                writer.write_all(b"281 authentication accepted\r\n").await?;
            } else {
                writer.write_all(b"481 authentication rejected\r\n").await?;
            }
        }
        _ => {
            writer.write_all(b"501 syntax error\r\n").await?;
        }
    }

    Ok(())
}

async fn handle_group(
    group: &str,
    server: &Arc<ServerState>,
    state: &mut SessionState,
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
) -> Result<(), Box<dyn std::error::Error>> {
    if !state.authenticated {
        writer.write_all(b"480 authentication required\r\n").await?;
        return Ok(());
    }

    if let Some(config) = server.fixtures.groups.get(group) {
        state.current_group = Some(group.to_string());
        let count = config.article_ids.len();
        let low = 1;
        let high = count.max(1);
        let response = format!("211 {count} {low} {high} {group}\r\n");
        writer.write_all(response.as_bytes()).await?;
    } else {
        writer.write_all(b"411 no such group\r\n").await?;
    }

    Ok(())
}

async fn handle_stat(
    message_id: &str,
    server: &Arc<ServerState>,
    state: &SessionState,
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
) -> Result<(), Box<dyn std::error::Error>> {
    if !state.authenticated {
        writer.write_all(b"480 authentication required\r\n").await?;
        return Ok(());
    }

    let Some(group) = state.current_group.as_ref() else {
        writer.write_all(b"412 no newsgroup selected\r\n").await?;
        return Ok(());
    };

    let config = match server.fixtures.groups.get(group) {
        Some(config) => config,
        None => {
            writer.write_all(b"411 no such group\r\n").await?;
            return Ok(());
        }
    };

    let missing = config
        .missing_articles
        .as_ref()
        .map(|items| items.iter().any(|id| id == message_id))
        .unwrap_or(false);

    if missing || !config.articles.contains_key(message_id) {
        writer.write_all(b"430 no such article\r\n").await?;
    } else {
        writer
            .write_all(format!("223 0 <{message_id}>\r\n").as_bytes())
            .await?;
    }

    Ok(())
}

async fn handle_body(
    message_id: &str,
    server: &Arc<ServerState>,
    state: &SessionState,
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
) -> Result<(), Box<dyn std::error::Error>> {
    if !state.authenticated {
        writer.write_all(b"480 authentication required\r\n").await?;
        return Ok(());
    }

    let Some(group) = state.current_group.as_ref() else {
        writer.write_all(b"412 no newsgroup selected\r\n").await?;
        return Ok(());
    };

    let config = match server.fixtures.groups.get(group) {
        Some(config) => config,
        None => {
            writer.write_all(b"411 no such group\r\n").await?;
            return Ok(());
        }
    };

    let missing = config
        .missing_articles
        .as_ref()
        .map(|items| items.iter().any(|id| id == message_id))
        .unwrap_or(false);

    let Some(body) = config.articles.get(message_id) else {
        writer.write_all(b"430 no such article\r\n").await?;
        return Ok(());
    };

    if missing {
        writer.write_all(b"430 no such article\r\n").await?;
        return Ok(());
    }

    writer
        .write_all(format!("222 0 <{message_id}>\r\n").as_bytes())
        .await?;
    send_body(body, writer).await?;
    Ok(())
}

async fn send_body(
    body: &str,
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
) -> Result<(), Box<dyn std::error::Error>> {
    for line in body.split('\n') {
        let mut line = line.replace('\r', "");
        if line.starts_with('.') {
            line.insert(0, '.');
        }
        writer.write_all(line.as_bytes()).await?;
        writer.write_all(b"\r\n").await?;
    }
    writer.write_all(b".\r\n").await?;
    Ok(())
}

fn extract_message_id(command_line: &str, prefix: &str) -> String {
    let id = command_line[prefix.len()..].trim();
    id.trim_matches(['<', '>']).to_string()
}

fn load_fixtures(path: &Path) -> Result<FixtureConfig, Box<dyn std::error::Error>> {
    let data = std::fs::read_to_string(path)?;
    let fixtures = serde_json::from_str(&data)?;
    Ok(fixtures)
}

async fn maybe_delay(args: &Args) {
    if args.delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(args.delay_ms)).await;
    }
}

fn should_disconnect(args: &Args, state: &SessionState) -> bool {
    args.disconnect_after > 0 && state.commands_seen >= args.disconnect_after
}
