use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

#[derive(Debug, Deserialize, Clone)]
pub struct FixtureConfig {
    pub greeting: Option<String>,
    pub groups: HashMap<String, GroupConfig>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct GroupConfig {
    pub article_ids: Vec<String>,
    pub articles: HashMap<String, String>,
    pub missing_articles: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
pub struct StubConfig {
    pub bind: SocketAddr,
    pub fixtures_path: PathBuf,
    pub require_auth: bool,
    pub username: String,
    pub password: String,
    pub disconnect_after: usize,
    pub delay_ms: u64,
}

#[derive(Debug)]
struct SessionState {
    authenticated: bool,
    current_group: Option<String>,
    commands_seen: usize,
}

#[derive(Clone)]
pub struct StubServer {
    state: Arc<ServerState>,
}

impl StubServer {
    pub fn new(config: StubConfig, fixtures: FixtureConfig) -> Self {
        Self {
            state: Arc::new(ServerState::new(config, fixtures)),
        }
    }

    pub async fn serve(self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let listener = TcpListener::bind(self.state.config.bind).await?;
        loop {
            let (stream, _) = listener.accept().await?;
            let state = Arc::clone(&self.state);
            tokio::spawn(async move {
                if let Err(err) = handle_client(stream, state).await {
                    eprintln!("client error: {err}");
                }
            });
        }
    }

    pub async fn serve_once(self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let listener = TcpListener::bind(self.state.config.bind).await?;
        let (stream, _) = listener.accept().await?;
        handle_client(stream, Arc::clone(&self.state)).await?;
        Ok(())
    }
}

struct ServerState {
    config: StubConfig,
    fixtures: FixtureConfig,
    auth_attempts: Mutex<usize>,
}

impl ServerState {
    fn new(config: StubConfig, fixtures: FixtureConfig) -> Self {
        Self {
            config,
            fixtures,
            auth_attempts: Mutex::new(0),
        }
    }
}

pub fn load_fixtures(
    path: &Path,
) -> Result<FixtureConfig, Box<dyn std::error::Error + Send + Sync>> {
    let data = std::fs::read_to_string(path)?;
    let fixtures = serde_json::from_str(&data)?;
    Ok(fixtures)
}

async fn handle_client(
    stream: TcpStream,
    state: Arc<ServerState>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    let greeting = state
        .fixtures
        .greeting
        .clone()
        .unwrap_or_else(|| "200 nzbg test server ready".to_string());
    writer
        .write_all(format!("{greeting}\r\n").as_bytes())
        .await?;

    let mut session = SessionState {
        authenticated: !state.config.require_auth,
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

        session.commands_seen += 1;
        maybe_delay(&state.config).await;
        if should_disconnect(&state.config, &session) {
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
                handle_authinfo(parts.collect::<Vec<_>>(), &state, &mut session, &mut writer)
                    .await?;
            }
            "GROUP" => {
                let group = parts.next().unwrap_or("");
                handle_group(group, &state, &mut session, &mut writer).await?;
            }
            "STAT" => {
                let message_id = extract_message_id(command_line, "STAT");
                handle_stat(&message_id, &state, &session, &mut writer).await?;
            }
            "BODY" => {
                let message_id = extract_message_id(command_line, "BODY");
                handle_body(&message_id, &state, &session, &mut writer).await?;
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
    state: &Arc<ServerState>,
    session: &mut SessionState,
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if args.len() < 2 {
        writer.write_all(b"501 syntax error\r\n").await?;
        return Ok(());
    }

    let verb = args[0].to_uppercase();
    let value = args[1];

    match verb.as_str() {
        "USER" => {
            let mut attempts = state.auth_attempts.lock().await;
            *attempts += 1;
            if value == state.config.username {
                writer.write_all(b"381 password required\r\n").await?;
            } else {
                writer.write_all(b"481 authentication rejected\r\n").await?;
            }
        }
        "PASS" => {
            if value == state.config.password {
                session.authenticated = true;
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
    state: &Arc<ServerState>,
    session: &mut SessionState,
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if !session.authenticated {
        writer.write_all(b"480 authentication required\r\n").await?;
        return Ok(());
    }

    if let Some(config) = state.fixtures.groups.get(group) {
        session.current_group = Some(group.to_string());
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
    state: &Arc<ServerState>,
    session: &SessionState,
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if !session.authenticated {
        writer.write_all(b"480 authentication required\r\n").await?;
        return Ok(());
    }

    let Some(group) = session.current_group.as_ref() else {
        writer.write_all(b"412 no newsgroup selected\r\n").await?;
        return Ok(());
    };

    let config = match state.fixtures.groups.get(group) {
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
    state: &Arc<ServerState>,
    session: &SessionState,
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if !session.authenticated {
        writer.write_all(b"480 authentication required\r\n").await?;
        return Ok(());
    }

    let Some(group) = session.current_group.as_ref() else {
        writer.write_all(b"412 no newsgroup selected\r\n").await?;
        return Ok(());
    };

    let config = match state.fixtures.groups.get(group) {
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
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
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

async fn maybe_delay(config: &StubConfig) {
    if config.delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(config.delay_ms)).await;
    }
}

fn should_disconnect(config: &StubConfig, session: &SessionState) -> bool {
    config.disconnect_after > 0 && session.commands_seen >= config.disconnect_after
}
