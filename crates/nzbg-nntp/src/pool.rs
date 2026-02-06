use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, Semaphore};

use crate::error::NntpError;
use crate::model::NewsServer;
use crate::protocol::NntpConnection;

#[async_trait::async_trait]
pub trait ConnectionFactory: Send + Sync {
    async fn connect(&self, server: &NewsServer) -> Result<NntpConnection, NntpError>;
}

pub struct RealConnectionFactory;

#[async_trait::async_trait]
impl ConnectionFactory for RealConnectionFactory {
    async fn connect(&self, server: &NewsServer) -> Result<NntpConnection, NntpError> {
        NntpConnection::connect(server).await
    }
}

struct ServerState {
    server: NewsServer,
    semaphore: Arc<Semaphore>,
    idle_connections: Mutex<Vec<NntpConnection>>,
    backoff_until: Mutex<Option<Instant>>,
    fail_count: Mutex<u32>,
}

pub struct ServerPool<F: ConnectionFactory = RealConnectionFactory> {
    servers: Vec<Arc<ServerState>>,
    factory: Arc<F>,
}

impl ServerPool<RealConnectionFactory> {
    pub fn new(servers: Vec<NewsServer>) -> Self {
        Self::with_factory(servers, Arc::new(RealConnectionFactory))
    }
}

impl<F: ConnectionFactory> ServerPool<F> {
    pub fn with_factory(servers: Vec<NewsServer>, factory: Arc<F>) -> Self {
        let mut sorted = servers;
        sorted.sort_by_key(|s| (s.level, s.group));

        let server_states = sorted
            .into_iter()
            .filter(|s| s.active)
            .map(|server| {
                let max_conns = server.connections.max(1) as usize;
                Arc::new(ServerState {
                    semaphore: Arc::new(Semaphore::new(max_conns)),
                    idle_connections: Mutex::new(Vec::new()),
                    backoff_until: Mutex::new(None),
                    fail_count: Mutex::new(0),
                    server,
                })
            })
            .collect();

        Self {
            servers: server_states,
            factory,
        }
    }

    pub async fn fetch_article(
        &self,
        message_id: &str,
        groups: &[String],
    ) -> Result<Vec<u8>, NntpError> {
        let mut last_error = NntpError::ProtocolError("no servers available".into());

        for state in &self.servers {
            if self.is_in_backoff(state).await {
                continue;
            }

            let permit = match state.semaphore.clone().try_acquire_owned() {
                Ok(permit) => permit,
                Err(_) => continue,
            };

            match self.try_fetch_from_server(state, message_id, groups).await {
                Ok(data) => {
                    self.reset_backoff(state).await;
                    drop(permit);
                    return Ok(data);
                }
                Err(NntpError::ArticleNotFound(msg)) => {
                    drop(permit);
                    last_error = NntpError::ArticleNotFound(msg);
                    continue;
                }
                Err(err) => {
                    self.record_failure(state).await;
                    drop(permit);
                    last_error = err;
                    continue;
                }
            }
        }

        Err(last_error)
    }

    async fn try_fetch_from_server(
        &self,
        state: &ServerState,
        message_id: &str,
        groups: &[String],
    ) -> Result<Vec<u8>, NntpError> {
        let mut conn = self.acquire_connection(state).await?;

        if state.server.join_group {
            let mut group_joined = false;
            for group in groups {
                match conn.join_group(group).await {
                    Ok(()) => {
                        group_joined = true;
                        break;
                    }
                    Err(NntpError::UnexpectedResponse(411, _)) => continue,
                    Err(err) => return Err(err),
                }
            }
            if !group_joined && !groups.is_empty() {
                return Err(NntpError::ProtocolError(
                    "no group accepted by server".into(),
                ));
            }
        }

        let mut reader = conn.fetch_body(message_id).await?;
        let mut body_lines = Vec::new();
        while let Some(line) = reader.read_line().await? {
            body_lines.push(line.into_bytes());
        }

        self.return_connection(state, conn).await;
        let data = body_lines.join(&b'\n');
        Ok(data)
    }

    async fn acquire_connection(&self, state: &ServerState) -> Result<NntpConnection, NntpError> {
        {
            let mut idle = state.idle_connections.lock().await;
            if let Some(conn) = idle.pop() {
                return Ok(conn);
            }
        }

        let mut conn = self.factory.connect(&state.server).await?;
        if let (Some(user), Some(pass)) = (&state.server.username, &state.server.password)
            && !user.is_empty()
        {
            conn.authenticate(user, pass).await?;
        }
        Ok(conn)
    }

    async fn return_connection(&self, state: &ServerState, conn: NntpConnection) {
        let mut idle = state.idle_connections.lock().await;
        idle.push(conn);
    }

    async fn is_in_backoff(&self, state: &ServerState) -> bool {
        let backoff = state.backoff_until.lock().await;
        backoff.is_some_and(|until| Instant::now() < until)
    }

    async fn record_failure(&self, state: &ServerState) {
        let mut fail_count = state.fail_count.lock().await;
        *fail_count = (*fail_count + 1).min(10);
        let delay = Duration::from_secs(1 << (*fail_count).min(6));
        let mut backoff = state.backoff_until.lock().await;
        *backoff = Some(Instant::now() + delay);
    }

    async fn reset_backoff(&self, state: &ServerState) {
        let mut fail_count = state.fail_count.lock().await;
        *fail_count = 0;
        let mut backoff = state.backoff_until.lock().await;
        *backoff = None;
    }

    pub fn server_count(&self) -> usize {
        self.servers.len()
    }
}

pub struct ServerPoolManager {
    all_servers: tokio::sync::RwLock<Vec<NewsServer>>,
    pool: tokio::sync::RwLock<ServerPool>,
}

impl ServerPoolManager {
    pub fn new(servers: Vec<NewsServer>) -> Self {
        let pool = ServerPool::new(servers.clone());
        Self {
            all_servers: tokio::sync::RwLock::new(servers),
            pool: tokio::sync::RwLock::new(pool),
        }
    }

    pub async fn fetch_article(
        &self,
        message_id: &str,
        groups: &[String],
    ) -> Result<Vec<u8>, NntpError> {
        let pool = self.pool.read().await;
        pool.fetch_article(message_id, groups).await
    }

    pub async fn activate_server(&self, server_id: u32) -> bool {
        self.set_server_active(server_id, true).await
    }

    pub async fn deactivate_server(&self, server_id: u32) -> bool {
        self.set_server_active(server_id, false).await
    }

    async fn set_server_active(&self, server_id: u32, active: bool) -> bool {
        let mut servers = self.all_servers.write().await;
        let found = servers.iter_mut().find(|s| s.id == server_id);
        match found {
            Some(server) => {
                server.active = active;
                let new_pool = ServerPool::new(servers.clone());
                let mut pool = self.pool.write().await;
                *pool = new_pool;
                true
            }
            None => false,
        }
    }

    pub async fn server_count(&self) -> usize {
        let pool = self.pool.read().await;
        pool.server_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Encryption, IpVersion};
    use std::sync::atomic::{AtomicU32, Ordering};
    use tokio::io::{AsyncWriteExt, BufReader, duplex};

    fn test_server(id: u32, level: u32, group: u32, connections: u32) -> NewsServer {
        NewsServer {
            id,
            name: format!("server-{id}"),
            active: true,
            host: "localhost".into(),
            port: 119,
            username: None,
            password: None,
            encryption: Encryption::None,
            cipher: None,
            connections,
            retention: 0,
            level,
            optional: false,
            group,
            join_group: true,
            ip_version: IpVersion::Auto,
            cert_verification: false,
        }
    }

    struct FakeFactory {
        response_body: Vec<u8>,
        connect_count: AtomicU32,
        should_fail: bool,
    }

    impl FakeFactory {
        fn new(response_body: &[u8]) -> Self {
            Self {
                response_body: response_body.to_vec(),
                connect_count: AtomicU32::new(0),
                should_fail: false,
            }
        }

        fn failing() -> Self {
            Self {
                response_body: Vec::new(),
                connect_count: AtomicU32::new(0),
                should_fail: true,
            }
        }
    }

    #[async_trait::async_trait]
    impl ConnectionFactory for FakeFactory {
        async fn connect(&self, server: &NewsServer) -> Result<NntpConnection, NntpError> {
            self.connect_count.fetch_add(1, Ordering::SeqCst);
            if self.should_fail {
                return Err(NntpError::Timeout);
            }

            let (client, mut server_end) = duplex(4096);

            let greeting = b"200 Welcome\r\n";
            let mut response = Vec::new();
            response.extend_from_slice(greeting);

            let body = self.response_body.clone();
            tokio::spawn(async move {
                server_end.write_all(&response).await.unwrap();

                let mut buf = vec![0u8; 1024];
                loop {
                    match tokio::io::AsyncReadExt::read(&mut server_end, &mut buf).await {
                        Ok(0) => break,
                        Ok(n) => {
                            let cmd = String::from_utf8_lossy(&buf[..n]);
                            if cmd.starts_with("GROUP") {
                                server_end.write_all(b"211 0 0 0 group\r\n").await.unwrap();
                            } else if cmd.starts_with("BODY") {
                                server_end.write_all(b"222 body\r\n").await.unwrap();
                                for line in body.split(|&b| b == b'\n') {
                                    server_end.write_all(line).await.unwrap();
                                    server_end.write_all(b"\r\n").await.unwrap();
                                }
                                server_end.write_all(b".\r\n").await.unwrap();
                            } else if cmd.starts_with("QUIT") {
                                server_end.write_all(b"205 bye\r\n").await.unwrap();
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            });

            let reader = BufReader::new(Box::new(client) as Box<dyn crate::protocol::NntpIo>);
            let stream = crate::protocol::NntpStream::Plain(reader);
            NntpConnection::from_stream(server.id, stream).await
        }
    }

    #[tokio::test]
    async fn pool_filters_inactive_servers() {
        let mut inactive = test_server(1, 0, 0, 2);
        inactive.active = false;
        let active = test_server(2, 0, 0, 2);
        let factory = Arc::new(FakeFactory::new(b"data"));
        let pool = ServerPool::with_factory(vec![inactive, active], factory);
        assert_eq!(pool.server_count(), 1);
    }

    #[tokio::test]
    async fn pool_sorts_servers_by_level_then_group() {
        let s1 = test_server(1, 1, 0, 2);
        let s2 = test_server(2, 0, 1, 2);
        let s3 = test_server(3, 0, 0, 2);
        let factory = Arc::new(FakeFactory::new(b"data"));
        let pool = ServerPool::with_factory(vec![s1, s2, s3], factory);
        assert_eq!(pool.servers[0].server.id, 3);
        assert_eq!(pool.servers[1].server.id, 2);
        assert_eq!(pool.servers[2].server.id, 1);
    }

    #[tokio::test]
    async fn fetch_article_returns_body_data() {
        let server = test_server(1, 0, 0, 2);
        let factory = Arc::new(FakeFactory::new(b"line1\nline2"));
        let pool = ServerPool::with_factory(vec![server], factory);

        let result = pool
            .fetch_article("test@example", &["alt.test".into()])
            .await;
        assert!(result.is_ok());
        let data = result.unwrap();
        assert!(!data.is_empty());
    }

    #[tokio::test]
    async fn failover_to_next_server_on_connect_error() {
        let s1 = test_server(1, 0, 0, 2);
        let s2 = test_server(2, 1, 0, 2);

        struct FailFirstFactory {
            fail_server_id: u32,
            response_body: Vec<u8>,
        }

        #[async_trait::async_trait]
        impl ConnectionFactory for FailFirstFactory {
            async fn connect(&self, server: &NewsServer) -> Result<NntpConnection, NntpError> {
                if server.id == self.fail_server_id {
                    return Err(NntpError::Timeout);
                }

                let (client, mut server_end) = duplex(4096);
                let body = self.response_body.clone();
                tokio::spawn(async move {
                    server_end.write_all(b"200 Welcome\r\n").await.unwrap();
                    let mut buf = vec![0u8; 1024];
                    loop {
                        match tokio::io::AsyncReadExt::read(&mut server_end, &mut buf).await {
                            Ok(0) => break,
                            Ok(n) => {
                                let cmd = String::from_utf8_lossy(&buf[..n]);
                                if cmd.starts_with("GROUP") {
                                    server_end.write_all(b"211 0 0 0 group\r\n").await.unwrap();
                                } else if cmd.starts_with("BODY") {
                                    server_end.write_all(b"222 body\r\n").await.unwrap();
                                    for line in body.split(|&b| b == b'\n') {
                                        server_end.write_all(line).await.unwrap();
                                        server_end.write_all(b"\r\n").await.unwrap();
                                    }
                                    server_end.write_all(b".\r\n").await.unwrap();
                                } else if cmd.starts_with("QUIT") {
                                    server_end.write_all(b"205 bye\r\n").await.unwrap();
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                });

                let reader = BufReader::new(Box::new(client) as Box<dyn crate::protocol::NntpIo>);
                let stream = crate::protocol::NntpStream::Plain(reader);
                NntpConnection::from_stream(server.id, stream).await
            }
        }

        let factory = Arc::new(FailFirstFactory {
            fail_server_id: 1,
            response_body: b"fallback-data".to_vec(),
        });
        let pool = ServerPool::with_factory(vec![s1, s2], factory);

        let result = pool
            .fetch_article("test@example", &["alt.test".into()])
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn backoff_is_set_on_failure() {
        let server = test_server(1, 0, 0, 2);
        let factory = Arc::new(FakeFactory::failing());
        let pool = ServerPool::with_factory(vec![server], factory);

        let _ = pool
            .fetch_article("test@example", &["alt.test".into()])
            .await;

        let backoff = pool.servers[0].backoff_until.lock().await;
        assert!(backoff.is_some());
    }

    #[tokio::test]
    async fn backoff_resets_on_success() {
        let server = test_server(1, 0, 0, 2);
        let factory = Arc::new(FakeFactory::new(b"data"));
        let pool = ServerPool::with_factory(vec![server], factory);

        {
            let mut fail_count = pool.servers[0].fail_count.lock().await;
            *fail_count = 3;
            let mut backoff = pool.servers[0].backoff_until.lock().await;
            *backoff = Some(Instant::now() - Duration::from_secs(1));
        }

        let result = pool
            .fetch_article("test@example", &["alt.test".into()])
            .await;
        assert!(result.is_ok());

        let fail_count = pool.servers[0].fail_count.lock().await;
        assert_eq!(*fail_count, 0);
    }

    #[tokio::test]
    async fn respects_semaphore_connection_limit() {
        let server = test_server(1, 0, 0, 1);
        let factory = Arc::new(FakeFactory::new(b"data"));
        let pool = ServerPool::with_factory(vec![server], factory);

        assert_eq!(pool.servers[0].semaphore.available_permits(), 1);
    }

    #[tokio::test]
    async fn no_servers_returns_error() {
        let factory = Arc::new(FakeFactory::new(b"data"));
        let pool: ServerPool<FakeFactory> = ServerPool::with_factory(vec![], factory);

        let result = pool
            .fetch_article("test@example", &["alt.test".into()])
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn manager_deactivate_reduces_server_count() {
        let s1 = test_server(1, 0, 0, 2);
        let s2 = test_server(2, 0, 0, 2);
        let manager = ServerPoolManager::new(vec![s1, s2]);
        assert_eq!(manager.server_count().await, 2);

        assert!(manager.deactivate_server(1).await);
        assert_eq!(manager.server_count().await, 1);
    }

    #[tokio::test]
    async fn manager_activate_restores_server() {
        let mut s1 = test_server(1, 0, 0, 2);
        s1.active = false;
        let s2 = test_server(2, 0, 0, 2);
        let manager = ServerPoolManager::new(vec![s1, s2]);
        assert_eq!(manager.server_count().await, 1);

        assert!(manager.activate_server(1).await);
        assert_eq!(manager.server_count().await, 2);
    }

    #[tokio::test]
    async fn manager_unknown_server_returns_false() {
        let s1 = test_server(1, 0, 0, 2);
        let manager = ServerPoolManager::new(vec![s1]);
        assert!(!manager.activate_server(999).await);
    }
}
