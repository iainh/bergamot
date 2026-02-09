use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use futures::stream::{FuturesUnordered, StreamExt};
use tokio::sync::{Mutex, Semaphore};

use crate::error::NntpError;
use crate::model::NewsServer;
use crate::protocol::NntpConnection;

pub trait StatsRecorder: Send + Sync {
    fn record_bytes(&self, server_id: u32, bytes: u64);
    fn record_article_success(&self, server_id: u32);
    fn record_article_failure(&self, server_id: u32);
}

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
    idle_connections: Mutex<Vec<(NntpConnection, Instant)>>,
    backoff_until_ms: AtomicU64,
    fail_count: AtomicU32,
    epoch: Instant,
}

pub struct ServerPool<F: ConnectionFactory = RealConnectionFactory> {
    servers: Vec<Arc<ServerState>>,
    factory: Arc<F>,
    stats: Option<Arc<dyn StatsRecorder>>,
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

        let epoch = Instant::now();
        let server_states = sorted
            .into_iter()
            .filter(|s| s.active)
            .map(|server| {
                let max_conns = server.connections.max(1) as usize;
                Arc::new(ServerState {
                    semaphore: Arc::new(Semaphore::new(max_conns)),
                    idle_connections: Mutex::new(Vec::new()),
                    backoff_until_ms: AtomicU64::new(0),
                    fail_count: AtomicU32::new(0),
                    epoch,
                    server,
                })
            })
            .collect();

        Self {
            servers: server_states,
            factory,
            stats: None,
        }
    }

    pub fn with_stats(mut self, stats: Arc<dyn StatsRecorder>) -> Self {
        self.stats = Some(stats);
        self
    }

    pub async fn fetch_article(
        &self,
        message_id: &str,
        groups: &[String],
    ) -> Result<Vec<u8>, NntpError> {
        self.fetch_article_targeted(message_id, groups, None).await
    }

    /// Fetch an article, optionally targeting a specific server first.
    ///
    /// # Server Selection Strategy
    ///
    /// When `target_server_id` is `Some(id)`:
    ///   1. Try the targeted server first (chosen by the WFQ scheduler based
    ///      on throughput-weighted queue depth). This avoids wasting connections
    ///      on speculative requests to other servers.
    ///   2. If the target fails with ArticleNotFound, fall back sequentially
    ///      through remaining servers (ordered by level, then group). Sequential
    ///      fallback with per-server timeouts prevents a single slow server
    ///      from blocking the pipeline while still providing redundancy.
    ///
    /// When `target_server_id` is `None`:
    ///   Falls back to the original parallel-blast strategy (try all servers
    ///   concurrently via FuturesUnordered). This preserves backward
    ///   compatibility when no scheduler is configured.
    pub async fn fetch_article_targeted(
        &self,
        message_id: &str,
        groups: &[String],
        target_server_id: Option<u32>,
    ) -> Result<Vec<u8>, NntpError> {
        if self.servers.is_empty() {
            return Err(NntpError::NoServersConfigured);
        }

        let available: Vec<_> = self
            .servers
            .iter()
            .filter(|s| !self.is_in_backoff(s))
            .collect();

        if available.is_empty() {
            return Err(NntpError::ProtocolError("all servers in backoff".into()));
        }

        // When a target server is specified, use sequential fetching:
        // try the target first, then fall back through remaining servers.
        // This is more efficient than parallel-blast because:
        // - The scheduler already picked the optimal server
        // - Most articles exist on the primary server
        // - We don't waste connection permits on speculative requests
        if let Some(target_id) = target_server_id {
            return self
                .fetch_article_sequential(message_id, groups, target_id, &available)
                .await;
        }

        // Original parallel-blast path: try all servers concurrently.
        // Used when no server scheduler is configured.
        let futs: FuturesUnordered<_> = available
            .iter()
            .enumerate()
            .map(|(i, state)| {
                let sem = state.semaphore.clone();
                async move {
                    let permit = match sem.acquire_owned().await {
                        Ok(p) => p,
                        Err(_) => {
                            return (i, Err(NntpError::ProtocolError("semaphore closed".into())));
                        }
                    };
                    let result = self.try_fetch_from_server(state, message_id, groups).await;
                    drop(permit);
                    (i, result)
                }
            })
            .collect();

        futures::pin_mut!(futs);

        let mut last_error = NntpError::ProtocolError("no servers available".into());

        while let Some((server_idx, result)) = futs.next().await {
            let state = available[server_idx];
            match result {
                Ok(data) => {
                    self.reset_backoff(state);
                    return Ok(data);
                }
                Err(NntpError::ArticleNotFound(msg)) => {
                    if let Some(stats) = &self.stats {
                        stats.record_article_failure(state.server.id);
                    }
                    last_error = NntpError::ArticleNotFound(msg);
                }
                Err(err) => {
                    if let Some(stats) = &self.stats {
                        stats.record_article_failure(state.server.id);
                    }
                    self.record_failure(state);
                    last_error = err;
                }
            }
        }

        Err(last_error)
    }

    /// Sequential fetch: try the target server first, then fall through
    /// remaining servers in priority order. Each attempt has a timeout to
    /// prevent a single slow server from stalling the pipeline.
    ///
    /// This implements the "try primary first, failover sequentially" pattern
    /// which is optimal when the scheduler has already identified the best
    /// server. It avoids the connection waste of parallel-blast while still
    /// providing full redundancy through ordered fallback.
    async fn fetch_article_sequential(
        &self,
        message_id: &str,
        groups: &[String],
        target_id: u32,
        available: &[&Arc<ServerState>],
    ) -> Result<Vec<u8>, NntpError> {
        const ARTICLE_TIMEOUT: Duration = Duration::from_secs(60);

        // Build ordered list: target server first, then remaining by level.
        let mut ordered: Vec<&Arc<ServerState>> = Vec::with_capacity(available.len());
        if let Some(target) = available.iter().find(|s| s.server.id == target_id) {
            ordered.push(target);
        }
        for s in available {
            if s.server.id != target_id {
                ordered.push(s);
            }
        }

        let mut last_error = NntpError::ProtocolError("no servers available".into());

        for state in ordered {
            let permit = match state.semaphore.clone().try_acquire_owned() {
                Ok(p) => p,
                Err(_) => continue,
            };

            let result = tokio::time::timeout(
                ARTICLE_TIMEOUT,
                self.try_fetch_from_server(state, message_id, groups),
            )
            .await;

            drop(permit);

            match result {
                Ok(Ok(data)) => {
                    self.reset_backoff(state);
                    if let Some(stats) = &self.stats {
                        stats.record_bytes(state.server.id, data.len() as u64);
                        stats.record_article_success(state.server.id);
                    }
                    return Ok(data);
                }
                Ok(Err(NntpError::ArticleNotFound(msg))) => {
                    if let Some(stats) = &self.stats {
                        stats.record_article_failure(state.server.id);
                    }
                    last_error = NntpError::ArticleNotFound(msg);
                }
                Ok(Err(err)) => {
                    if let Some(stats) = &self.stats {
                        stats.record_article_failure(state.server.id);
                    }
                    self.record_failure(state);
                    last_error = err;
                }
                Err(_timeout) => {
                    self.record_failure(state);
                    last_error = NntpError::Timeout;
                    tracing::debug!(
                        server_id = state.server.id,
                        message_id,
                        "article fetch timed out, trying next server"
                    );
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
            body_lines.push(line);
        }

        self.return_connection(state, conn).await;
        let data = body_lines.join(&b'\n');
        if let Some(stats) = &self.stats {
            stats.record_bytes(state.server.id, data.len() as u64);
            stats.record_article_success(state.server.id);
        }
        Ok(data)
    }

    async fn acquire_connection(&self, state: &ServerState) -> Result<NntpConnection, NntpError> {
        {
            let mut idle = state.idle_connections.lock().await;
            let max_idle = Duration::from_secs(60);
            while let Some((conn, last_used)) = idle.pop() {
                if last_used.elapsed() < max_idle {
                    return Ok(conn);
                }
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
        let max_pool = state.server.connections.max(1) as usize;
        if idle.len() < max_pool {
            idle.push((conn, Instant::now()));
        }
    }

    pub async fn cleanup_idle_connections(&self, max_idle: Duration) -> usize {
        let mut total_closed = 0;
        for state in &self.servers {
            total_closed += Self::cleanup_idle(state, max_idle).await;
        }
        total_closed
    }

    async fn cleanup_idle(state: &ServerState, max_idle: Duration) -> usize {
        let mut idle = state.idle_connections.lock().await;
        let mut kept = Vec::new();
        let mut closed = 0;
        for (mut conn, last_used) in idle.drain(..) {
            if last_used.elapsed() >= max_idle {
                let _ = conn.quit().await;
                closed += 1;
            } else {
                kept.push((conn, last_used));
            }
        }
        *idle = kept;
        closed
    }

    fn is_in_backoff(&self, state: &ServerState) -> bool {
        let deadline_ms = state.backoff_until_ms.load(Ordering::Acquire);
        if deadline_ms == 0 {
            return false;
        }
        let now_ms = state.epoch.elapsed().as_millis() as u64;
        now_ms < deadline_ms
    }

    fn record_failure(&self, state: &ServerState) {
        let count = state.fail_count.fetch_add(1, Ordering::Relaxed).min(9) + 1;
        let capped = count.min(10);
        state.fail_count.store(capped, Ordering::Relaxed);
        let delay = Duration::from_secs(1 << capped.min(6));
        let deadline_ms = (state.epoch.elapsed() + delay).as_millis() as u64;
        state.backoff_until_ms.store(deadline_ms, Ordering::Release);
    }

    fn reset_backoff(&self, state: &ServerState) {
        state.fail_count.store(0, Ordering::Relaxed);
        state.backoff_until_ms.store(0, Ordering::Release);
    }

    pub fn server_count(&self) -> usize {
        self.servers.len()
    }
}

pub struct ServerPoolManager {
    all_servers: tokio::sync::RwLock<Vec<NewsServer>>,
    pool: tokio::sync::RwLock<ServerPool>,
    stats: Option<Arc<dyn StatsRecorder>>,
}

impl ServerPoolManager {
    pub fn new(servers: Vec<NewsServer>) -> Self {
        let pool = ServerPool::new(servers.clone());
        Self {
            all_servers: tokio::sync::RwLock::new(servers),
            pool: tokio::sync::RwLock::new(pool),
            stats: None,
        }
    }

    pub fn with_stats(mut self, stats: Arc<dyn StatsRecorder>) -> Self {
        self.stats = Some(stats);
        let all_servers = self.all_servers.get_mut();
        let new_pool = ServerPool::new(all_servers.clone());
        let new_pool = if let Some(s) = &self.stats {
            new_pool.with_stats(Arc::clone(s))
        } else {
            new_pool
        };
        *self.pool.get_mut() = new_pool;
        self
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
                let mut new_pool = ServerPool::new(servers.clone());
                if let Some(s) = &self.stats {
                    new_pool = new_pool.with_stats(Arc::clone(s));
                }
                let mut pool = self.pool.write().await;
                *pool = new_pool;
                true
            }
            None => false,
        }
    }

    pub async fn cleanup_idle_connections(&self, max_idle: Duration) -> usize {
        let pool = self.pool.read().await;
        pool.cleanup_idle_connections(max_idle).await
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

        let deadline = pool.servers[0].backoff_until_ms.load(Ordering::Acquire);
        assert!(deadline > 0);
    }

    #[tokio::test]
    async fn backoff_resets_on_success() {
        let server = test_server(1, 0, 0, 2);
        let factory = Arc::new(FakeFactory::new(b"data"));
        let pool = ServerPool::with_factory(vec![server], factory);

        pool.servers[0].fail_count.store(3, Ordering::Relaxed);
        let past_ms = pool.servers[0]
            .epoch
            .elapsed()
            .saturating_sub(Duration::from_secs(1))
            .as_millis() as u64;
        pool.servers[0]
            .backoff_until_ms
            .store(past_ms, Ordering::Release);

        let result = pool
            .fetch_article("test@example", &["alt.test".into()])
            .await;
        assert!(result.is_ok());

        let fail_count = pool.servers[0].fail_count.load(Ordering::Relaxed);
        assert_eq!(fail_count, 0);
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

    #[tokio::test]
    async fn idle_connection_reused_within_timeout() {
        let server = test_server(1, 0, 0, 2);
        let factory = Arc::new(FakeFactory::new(b"data"));
        let pool = ServerPool::with_factory(vec![server], factory.clone());

        let _ = pool
            .fetch_article("test@example", &["alt.test".into()])
            .await
            .unwrap();
        assert_eq!(factory.connect_count.load(Ordering::SeqCst), 1);

        let _ = pool
            .fetch_article("test@example", &["alt.test".into()])
            .await
            .unwrap();
        assert_eq!(factory.connect_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn stale_idle_connection_is_dropped() {
        let server = test_server(1, 0, 0, 2);
        let factory = Arc::new(FakeFactory::new(b"data"));
        let pool = ServerPool::with_factory(vec![server], factory.clone());

        {
            let conn = factory.connect(&pool.servers[0].server).await.unwrap();
            let mut idle = pool.servers[0].idle_connections.lock().await;
            idle.push((conn, Instant::now() - Duration::from_secs(120)));
        }

        let _ = pool
            .fetch_article("test@example", &["alt.test".into()])
            .await
            .unwrap();
        assert_eq!(factory.connect_count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn cleanup_idle_closes_old_connections() {
        let server = test_server(1, 0, 0, 4);
        let factory = Arc::new(FakeFactory::new(b"data"));
        let pool = ServerPool::with_factory(vec![server], factory.clone());

        for _ in 0..3 {
            let conn = factory.connect(&pool.servers[0].server).await.unwrap();
            let mut idle = pool.servers[0].idle_connections.lock().await;
            idle.push((conn, Instant::now() - Duration::from_secs(120)));
        }

        let closed = pool.cleanup_idle_connections(Duration::from_secs(60)).await;
        assert_eq!(closed, 3);

        let idle = pool.servers[0].idle_connections.lock().await;
        assert!(idle.is_empty());
    }

    #[tokio::test]
    async fn stats_recorder_called_on_successful_fetch() {
        use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

        struct MockStats {
            recorded_server_id: std::sync::atomic::AtomicU32,
            recorded_bytes: AtomicU64,
        }

        impl StatsRecorder for MockStats {
            fn record_bytes(&self, server_id: u32, bytes: u64) {
                self.recorded_server_id
                    .store(server_id, AtomicOrdering::SeqCst);
                self.recorded_bytes.store(bytes, AtomicOrdering::SeqCst);
            }
            fn record_article_success(&self, _server_id: u32) {}
            fn record_article_failure(&self, _server_id: u32) {}
        }

        let server = test_server(1, 0, 0, 2);
        let factory = Arc::new(FakeFactory::new(b"hello"));
        let stats = Arc::new(MockStats {
            recorded_server_id: std::sync::atomic::AtomicU32::new(0),
            recorded_bytes: AtomicU64::new(0),
        });
        let pool = ServerPool::with_factory(vec![server], factory)
            .with_stats(stats.clone() as Arc<dyn StatsRecorder>);

        let result = pool
            .fetch_article("test@example", &["alt.test".into()])
            .await;
        assert!(result.is_ok());
        assert_eq!(stats.recorded_server_id.load(AtomicOrdering::SeqCst), 1);
        assert!(stats.recorded_bytes.load(AtomicOrdering::SeqCst) > 0);
    }

    #[tokio::test]
    async fn stats_recorder_not_called_on_failed_fetch() {
        use std::sync::atomic::Ordering as AtomicOrdering;

        struct MockStats {
            call_count: std::sync::atomic::AtomicU32,
        }

        impl StatsRecorder for MockStats {
            fn record_bytes(&self, _server_id: u32, _bytes: u64) {
                self.call_count.fetch_add(1, AtomicOrdering::SeqCst);
            }
            fn record_article_success(&self, _server_id: u32) {}
            fn record_article_failure(&self, _server_id: u32) {}
        }

        let server = test_server(1, 0, 0, 2);
        let factory = Arc::new(FakeFactory::failing());
        let stats = Arc::new(MockStats {
            call_count: std::sync::atomic::AtomicU32::new(0),
        });
        let pool = ServerPool::with_factory(vec![server], factory)
            .with_stats(stats.clone() as Arc<dyn StatsRecorder>);

        let result = pool
            .fetch_article("test@example", &["alt.test".into()])
            .await;
        assert!(result.is_err());
        assert_eq!(stats.call_count.load(AtomicOrdering::SeqCst), 0);
    }

    #[tokio::test]
    async fn parallel_fetch_succeeds_when_one_server_has_article() {
        let s1 = test_server(1, 0, 0, 2);
        let s2 = test_server(2, 0, 0, 2);

        struct NotFoundThenSuccessFactory {
            not_found_server_id: u32,
            response_body: Vec<u8>,
        }

        #[async_trait::async_trait]
        impl ConnectionFactory for NotFoundThenSuccessFactory {
            async fn connect(&self, server: &NewsServer) -> Result<NntpConnection, NntpError> {
                let (client, mut server_end) = duplex(4096);
                let not_found = server.id == self.not_found_server_id;
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
                                    if not_found {
                                        server_end
                                            .write_all(b"430 No Such Article\r\n")
                                            .await
                                            .unwrap();
                                    } else {
                                        server_end.write_all(b"222 body\r\n").await.unwrap();
                                        for line in body.split(|&b| b == b'\n') {
                                            server_end.write_all(line).await.unwrap();
                                            server_end.write_all(b"\r\n").await.unwrap();
                                        }
                                        server_end.write_all(b".\r\n").await.unwrap();
                                    }
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

        let factory = Arc::new(NotFoundThenSuccessFactory {
            not_found_server_id: 1,
            response_body: b"found-on-server-2".to_vec(),
        });
        let pool = ServerPool::with_factory(vec![s1, s2], factory);

        let result = pool
            .fetch_article("test@example", &["alt.test".into()])
            .await;
        assert!(result.is_ok());
        let data = result.unwrap();
        assert!(!data.is_empty());
    }

    #[tokio::test]
    async fn parallel_fetch_returns_not_found_when_all_servers_miss() {
        let s1 = test_server(1, 0, 0, 2);
        let s2 = test_server(2, 0, 0, 2);

        struct AllNotFoundFactory;

        #[async_trait::async_trait]
        impl ConnectionFactory for AllNotFoundFactory {
            async fn connect(&self, server: &NewsServer) -> Result<NntpConnection, NntpError> {
                let (client, mut server_end) = duplex(4096);
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
                                    server_end
                                        .write_all(b"430 No Such Article\r\n")
                                        .await
                                        .unwrap();
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

        let factory = Arc::new(AllNotFoundFactory);
        let pool = ServerPool::with_factory(vec![s1, s2], factory);

        let result = pool
            .fetch_article("test@example", &["alt.test".into()])
            .await;
        assert!(result.is_err());
        assert!(
            matches!(result, Err(NntpError::ArticleNotFound(_))),
            "expected ArticleNotFound"
        );
    }

    #[tokio::test]
    async fn parallel_fetch_is_concurrent_not_sequential() {
        use std::sync::atomic::AtomicBool;

        let s1 = test_server(1, 0, 0, 2);
        let s2 = test_server(2, 0, 0, 2);

        struct SlowNotFoundFactory {
            slow_server_id: u32,
            response_body: Vec<u8>,
        }

        #[async_trait::async_trait]
        impl ConnectionFactory for SlowNotFoundFactory {
            async fn connect(&self, server: &NewsServer) -> Result<NntpConnection, NntpError> {
                let (client, mut server_end) = duplex(4096);
                let is_slow = server.id == self.slow_server_id;
                let body = self.response_body.clone();
                let slow_started = if is_slow {
                    Some(Arc::new(AtomicBool::new(false)))
                } else {
                    None
                };
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
                                    if is_slow {
                                        if let Some(ref flag) = slow_started {
                                            flag.store(true, Ordering::SeqCst);
                                        }
                                        tokio::time::sleep(Duration::from_millis(500)).await;
                                        server_end
                                            .write_all(b"430 No Such Article\r\n")
                                            .await
                                            .unwrap();
                                    } else {
                                        server_end.write_all(b"222 body\r\n").await.unwrap();
                                        for line in body.split(|&b| b == b'\n') {
                                            server_end.write_all(line).await.unwrap();
                                            server_end.write_all(b"\r\n").await.unwrap();
                                        }
                                        server_end.write_all(b".\r\n").await.unwrap();
                                    }
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

        let factory = Arc::new(SlowNotFoundFactory {
            slow_server_id: 1,
            response_body: b"fast-data".to_vec(),
        });
        let pool = ServerPool::with_factory(vec![s1, s2], factory);

        let start = Instant::now();
        let result = pool
            .fetch_article("test@example", &["alt.test".into()])
            .await;
        let elapsed = start.elapsed();

        assert!(result.is_ok());
        assert!(
            elapsed < Duration::from_millis(200),
            "parallel fetch should complete in <200ms (server 2 is fast), took {:?}",
            elapsed
        );
    }

    #[tokio::test]
    async fn return_connection_caps_pool_size() {
        let server = test_server(1, 0, 0, 2);
        let factory = Arc::new(FakeFactory::new(b"data"));
        let pool = ServerPool::with_factory(vec![server], factory.clone());

        for _ in 0..5 {
            let conn = factory.connect(&pool.servers[0].server).await.unwrap();
            pool.return_connection(&pool.servers[0], conn).await;
        }

        let idle = pool.servers[0].idle_connections.lock().await;
        assert_eq!(idle.len(), 2);
    }
}
