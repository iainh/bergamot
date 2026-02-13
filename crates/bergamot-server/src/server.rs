use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router, middleware};
use rust_embed::Embed;
use tokio::net::TcpListener;
use tower_http::compression::CompressionLayer;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

use jsonrpsee::Methods;

use crate::auth::{
    AccessLevel, AuthState, auth_middleware, authenticate, extract_access, required_access,
    unauthorized_response,
};
use crate::config::ServerConfig;
use crate::error::{JsonRpcError, JsonRpcErrorBody};
use crate::rpc::JsonRpcResponse;
use crate::rpc_module::build_rpc_module;
use crate::shutdown::ShutdownHandle;
use crate::status::StatusResponse;
use crate::xmlrpc;

#[derive(Embed)]
#[folder = "webui/"]
struct WebUiAssets;

#[derive(Debug, Clone)]
pub struct AppState {
    version: String,
    download_rate: Arc<AtomicU64>,
    remaining_bytes: Arc<AtomicU64>,
    downloaded_bytes: Arc<AtomicU64>,
    speed_limit: Arc<AtomicU64>,
    start_time: std::time::Instant,
    queue: Option<bergamot_queue::QueueHandle>,
    shutdown: Option<ShutdownHandle>,
    disk: Option<std::sync::Arc<bergamot_diskstate::DiskState<bergamot_diskstate::JsonFormat>>>,
    log_buffer: Option<std::sync::Arc<bergamot_logging::LogBuffer>>,
    config: Option<std::sync::Arc<std::sync::RwLock<bergamot_config::Config>>>,
    config_path: Option<std::path::PathBuf>,
    download_paused: std::sync::Arc<std::sync::atomic::AtomicBool>,
    postproc_paused: std::sync::Arc<std::sync::atomic::AtomicBool>,
    scan_paused: std::sync::Arc<std::sync::atomic::AtomicBool>,
    resume_at: std::sync::Arc<AtomicU64>,
    scan_trigger: Option<tokio::sync::mpsc::Sender<()>>,
    feed_handle: Option<bergamot_feed::FeedHandle>,
    stats_tracker: Option<std::sync::Arc<bergamot_scheduler::SharedStatsTracker>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            version: "26.0".to_string(),
            download_rate: Arc::new(AtomicU64::new(0)),
            remaining_bytes: Arc::new(AtomicU64::new(0)),
            downloaded_bytes: Arc::new(AtomicU64::new(0)),
            speed_limit: Arc::new(AtomicU64::new(0)),
            start_time: std::time::Instant::now(),
            queue: None,
            shutdown: None,
            disk: None,
            log_buffer: None,
            config: None,
            config_path: None,
            download_paused: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            postproc_paused: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            scan_paused: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            resume_at: Arc::new(AtomicU64::new(0)),
            scan_trigger: None,
            feed_handle: None,
            stats_tracker: None,
        }
    }
}

impl AppState {
    pub fn with_queue(mut self, queue: bergamot_queue::QueueHandle) -> Self {
        self.queue = Some(queue);
        self
    }

    pub fn with_shutdown(mut self, shutdown: ShutdownHandle) -> Self {
        self.shutdown = Some(shutdown);
        self
    }

    pub fn with_disk(
        mut self,
        disk: std::sync::Arc<bergamot_diskstate::DiskState<bergamot_diskstate::JsonFormat>>,
    ) -> Self {
        self.disk = Some(disk);
        self
    }

    pub fn with_log_buffer(
        mut self,
        log_buffer: std::sync::Arc<bergamot_logging::LogBuffer>,
    ) -> Self {
        self.log_buffer = Some(log_buffer);
        self
    }

    pub fn log_buffer(&self) -> Option<&std::sync::Arc<bergamot_logging::LogBuffer>> {
        self.log_buffer.as_ref()
    }

    pub fn with_config(
        mut self,
        config: std::sync::Arc<std::sync::RwLock<bergamot_config::Config>>,
        config_path: std::path::PathBuf,
    ) -> Self {
        self.config = Some(config);
        self.config_path = Some(config_path);
        self
    }

    pub fn config(&self) -> Option<&std::sync::Arc<std::sync::RwLock<bergamot_config::Config>>> {
        self.config.as_ref()
    }

    pub fn config_path(&self) -> Option<&std::path::Path> {
        self.config_path.as_deref()
    }

    pub fn with_scan_trigger(mut self, tx: tokio::sync::mpsc::Sender<()>) -> Self {
        self.scan_trigger = Some(tx);
        self
    }

    pub fn download_paused(&self) -> &std::sync::Arc<std::sync::atomic::AtomicBool> {
        &self.download_paused
    }

    pub fn postproc_paused(&self) -> &std::sync::Arc<std::sync::atomic::AtomicBool> {
        &self.postproc_paused
    }

    pub fn scan_paused(&self) -> &std::sync::Arc<std::sync::atomic::AtomicBool> {
        &self.scan_paused
    }

    pub fn resume_at(&self) -> &Arc<AtomicU64> {
        &self.resume_at
    }

    pub fn scan_trigger(&self) -> Option<&tokio::sync::mpsc::Sender<()>> {
        self.scan_trigger.as_ref()
    }

    pub fn with_feed_handle(mut self, feed_handle: bergamot_feed::FeedHandle) -> Self {
        self.feed_handle = Some(feed_handle);
        self
    }

    pub fn feed_handle(&self) -> Option<&bergamot_feed::FeedHandle> {
        self.feed_handle.as_ref()
    }

    pub fn with_stats_tracker(
        mut self,
        tracker: std::sync::Arc<bergamot_scheduler::SharedStatsTracker>,
    ) -> Self {
        self.stats_tracker = Some(tracker);
        self
    }

    pub fn stats_tracker(&self) -> Option<&std::sync::Arc<bergamot_scheduler::SharedStatsTracker>> {
        self.stats_tracker.as_ref()
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn start_time(&self) -> std::time::Instant {
        self.start_time
    }

    pub fn queue_handle(&self) -> Option<&bergamot_queue::QueueHandle> {
        self.queue.as_ref()
    }

    pub fn shutdown_handle(&self) -> Option<&ShutdownHandle> {
        self.shutdown.as_ref()
    }

    pub fn disk(
        &self,
    ) -> Option<&std::sync::Arc<bergamot_diskstate::DiskState<bergamot_diskstate::JsonFormat>>>
    {
        self.disk.as_ref()
    }

    pub fn download_rate_ref(&self) -> &Arc<AtomicU64> {
        &self.download_rate
    }

    pub fn remaining_bytes_ref(&self) -> &Arc<AtomicU64> {
        &self.remaining_bytes
    }

    pub fn downloaded_bytes_ref(&self) -> &Arc<AtomicU64> {
        &self.downloaded_bytes
    }

    pub fn speed_limit_ref(&self) -> &Arc<AtomicU64> {
        &self.speed_limit
    }

    pub fn status(&self) -> StatusResponse {
        let remaining = self.remaining_bytes.load(Ordering::Relaxed);
        let remaining_fields = crate::status::SizeFields::from(remaining);
        let downloaded = self.downloaded_bytes.load(Ordering::Relaxed);
        let downloaded_fields = crate::status::SizeFields::from(downloaded);
        let download_rate = self.download_rate.load(Ordering::Relaxed);
        let rate_fields = crate::status::SizeFields::from(download_rate);
        let download_limit = self.speed_limit.load(Ordering::Relaxed);
        let download_paused = self.download_paused.load(Ordering::Relaxed);
        let post_paused = self.postproc_paused.load(Ordering::Relaxed);
        let scan_paused = self.scan_paused.load(Ordering::Relaxed);

        let (dest_dir, inter_dir) = self
            .config
            .as_ref()
            .map(|c| {
                let cfg = c.read().unwrap();
                (cfg.dest_dir.clone(), cfg.inter_dir.clone())
            })
            .unwrap_or_default();

        let (dest_free, dest_total) = disk_space(&dest_dir);
        let (inter_free, inter_total) = disk_space(&inter_dir);

        let dest_free_fields = crate::status::SizeFields::from(dest_free);
        let dest_total_fields = crate::status::SizeFields::from(dest_total);
        let inter_free_fields = crate::status::SizeFields::from(inter_free);
        let inter_total_fields = crate::status::SizeFields::from(inter_total);

        StatusResponse {
            remaining_size_lo: remaining_fields.lo,
            remaining_size_hi: remaining_fields.hi,
            remaining_size_mb: remaining_fields.mb,
            forced_size_lo: 0,
            forced_size_hi: 0,
            forced_size_mb: 0,
            downloaded_size_lo: downloaded_fields.lo,
            downloaded_size_hi: downloaded_fields.hi,
            downloaded_size_mb: downloaded_fields.mb,
            month_size_lo: 0,
            month_size_hi: 0,
            month_size_mb: 0,
            day_size_lo: 0,
            day_size_hi: 0,
            day_size_mb: 0,
            article_cache_lo: 0,
            article_cache_hi: 0,
            article_cache_mb: 0,
            download_rate,
            download_rate_lo: rate_fields.lo,
            download_rate_hi: rate_fields.hi,
            average_download_rate: 0,
            average_download_rate_lo: 0,
            average_download_rate_hi: 0,
            download_limit,
            thread_count: 0,
            post_job_count: 0,
            par_job_count: 0,
            url_count: 0,
            queue_script_count: 0,
            up_time_sec: self.start_time.elapsed().as_secs(),
            download_time_sec: 0,
            server_time: chrono::Utc::now().timestamp(),
            resume_time: self.resume_at.load(Ordering::Relaxed) as i64,
            download_paused,
            server_paused: false,
            download2_paused: false,
            post_paused,
            scan_paused,
            server_stand_by: download_rate == 0,
            quota_reached: false,
            feed_active: false,
            free_disk_space_lo: dest_free_fields.lo,
            free_disk_space_hi: dest_free_fields.hi,
            free_disk_space_mb: dest_free_fields.mb,
            total_disk_space_lo: dest_total_fields.lo,
            total_disk_space_hi: dest_total_fields.hi,
            total_disk_space_mb: dest_total_fields.mb,
            free_inter_disk_space_lo: inter_free_fields.lo,
            free_inter_disk_space_hi: inter_free_fields.hi,
            free_inter_disk_space_mb: inter_free_fields.mb,
            total_inter_disk_space_lo: inter_total_fields.lo,
            total_inter_disk_space_hi: inter_total_fields.hi,
            total_inter_disk_space_mb: inter_total_fields.mb,
            news_servers: self
                .config
                .as_ref()
                .and_then(|c| c.read().ok())
                .map(|cfg| {
                    cfg.servers
                        .iter()
                        .map(|s| crate::status::NewsServerStatus {
                            id: s.id,
                            active: s.active,
                        })
                        .collect()
                })
                .unwrap_or_default(),
        }
    }
}

fn disk_space(path: &std::path::Path) -> (u64, u64) {
    let free = fs2::available_space(path).unwrap_or(0);
    let total = fs2::total_space(path).unwrap_or(0);
    (free, total)
}

pub fn spawn_stats_updater(
    state: Arc<AppState>,
    queue: bergamot_queue::QueueHandle,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
        let mut prev_downloaded: u64 = 0;
        loop {
            interval.tick().await;
            match queue.get_status().await {
                Ok(status) => {
                    let speed = status.downloaded_size.saturating_sub(prev_downloaded);
                    prev_downloaded = status.downloaded_size;
                    state.download_rate_ref().store(speed, Ordering::Relaxed);
                    state
                        .remaining_bytes_ref()
                        .store(status.remaining_size, Ordering::Relaxed);
                    state
                        .downloaded_bytes_ref()
                        .store(status.downloaded_size, Ordering::Relaxed);
                    state
                        .speed_limit_ref()
                        .store(status.download_rate, Ordering::Relaxed);
                }
                Err(_) => break,
            }
        }
    })
}

#[derive(Clone)]
struct RpcState {
    methods: Arc<Methods>,
}

pub struct WebServer {
    config: Arc<ServerConfig>,
    rpc_methods: Methods,
}

impl WebServer {
    pub fn new(config: Arc<ServerConfig>, state: Arc<AppState>) -> Self {
        let rpc_methods: Methods = build_rpc_module(state).into();
        Self {
            config,
            rpc_methods,
        }
    }

    pub fn validate_tls_config(config: &ServerConfig) -> Result<(), Box<dyn std::error::Error>> {
        if config.secure_control
            && config.secure_cert.is_none()
            && config.secure_key.is_none()
            && config.cert_store.as_os_str().is_empty()
        {
            return Err(
                "SecureControl is enabled but no SecureCert/SecureKey or CertStore is configured"
                    .into(),
            );
        }
        if config.secure_control
            && (config.secure_cert.is_some() != config.secure_key.is_some())
        {
            return Err("Both SecureCert and SecureKey must be provided together".into());
        }
        Ok(())
    }

    fn build_router(&self) -> Router<()> {
        let auth_state = AuthState {
            config: (*self.config).clone(),
        };
        let rpc_state = RpcState {
            methods: Arc::new(self.rpc_methods.clone()),
        };
        let mut app = Router::new()
            .route("/jsonrpc", post(handle_jsonrpc))
            .route("/jsonprpc", get(handle_jsonprpc))
            .route("/xmlrpc", post(handle_xmlrpc))
            .route("/{credentials}/jsonrpc/{method}", get(handle_api_shortcut))
            .layer(middleware::from_fn_with_state(
                auth_state.clone(),
                auth_middleware,
            ))
            .with_state(rpc_state);

        if self.config.form_auth {
            let login_auth = auth_state.clone();
            app = app
                .route("/login", get(handle_login_page))
                .route(
                    "/login",
                    post(handle_login_submit).with_state(Arc::new(login_auth)),
                )
                .route("/logout", post(handle_logout));
        }

        let fallback_methods = Arc::new(self.rpc_methods.clone());
        let fallback_auth = auth_state;
        let fallback = axum::routing::any(move |req: axum::http::Request<axum::body::Body>| {
            let methods = fallback_methods.clone();
            let auth = fallback_auth.clone();
            async move {
                let path = req.uri().path().to_string();
                if let Some(method) = path.strip_prefix("/jsonrpc/")
                    && !method.is_empty()
                    && req.method() == axum::http::Method::GET
                {
                    let access = extract_access(&req, &auth.config);
                    let required = required_access(method);
                    if access > required || access == AccessLevel::Denied {
                        return unauthorized_response().into_response();
                    }
                    let params = parse_query_params(req.uri().query());
                    let (result, err) = call_rpc_via_module(&methods, method, &params).await;
                    let response = if let Some(err) = err {
                        JsonRpcResponse {
                            jsonrpc: "2.0".to_string(),
                            result: None,
                            error: Some(serde_json::to_value(JsonRpcErrorBody::from(err)).unwrap()),
                            id: serde_json::json!(0),
                        }
                    } else {
                        JsonRpcResponse {
                            jsonrpc: "2.0".to_string(),
                            result,
                            error: None,
                            id: serde_json::json!(0),
                        }
                    };
                    return Json(response).into_response();
                }
                serve_embedded_file(&path)
            }
        });

        app.fallback_service(fallback)
            .layer(TraceLayer::new_for_http())
            .layer(CompressionLayer::new().gzip(true))
            .layer(CorsLayer::permissive())
    }

    pub async fn run(&self) -> Result<(), Box<dyn std::error::Error>> {
        Self::validate_tls_config(&self.config)?;
        let app = self.build_router();

        if self.config.secure_control {
            let (cert_path, key_path) = if let (Some(cert), Some(key)) =
                (&self.config.secure_cert, &self.config.secure_key)
            {
                (cert.clone(), key.clone())
            } else {
                let paths = crate::tls::ensure_certificates(&self.config.cert_store)?;
                (paths.cert, paths.key)
            };

            let tls_config =
                axum_server::tls_rustls::RustlsConfig::from_pem_file(&cert_path, &key_path)
                    .await?;

            let https_addr: std::net::SocketAddr =
                format!("{}:{}", self.config.control_ip, self.config.secure_port).parse()?;

            if self.config.control_port != self.config.secure_port {
                let redirect_host = self.config.control_ip.clone();
                let redirect_port = self.config.secure_port;
                let http_addr =
                    format!("{}:{}", self.config.control_ip, self.config.control_port);
                tokio::spawn(async move {
                    let redirect_app = Router::new().fallback(
                        move |req: axum::http::Request<axum::body::Body>| {
                            let host = redirect_host.clone();
                            let port = redirect_port;
                            async move {
                                let path_and_query = req
                                    .uri()
                                    .path_and_query()
                                    .map(|pq| pq.as_str())
                                    .unwrap_or("/");
                                let port_suffix = if port == 443 {
                                    String::new()
                                } else {
                                    format!(":{port}")
                                };
                                let url = format!("https://{host}{port_suffix}{path_and_query}");
                                axum::response::Redirect::permanent(&url).into_response()
                            }
                        },
                    );
                    if let Ok(listener) = TcpListener::bind(&http_addr).await {
                        let _ = axum::serve(listener, redirect_app).await;
                    }
                });
            }

            axum_server::bind_rustls(https_addr, tls_config)
                .serve(app.into_make_service_with_connect_info::<std::net::SocketAddr>())
                .await?;
        } else {
            let bind_addr = format!("{}:{}", self.config.control_ip, self.config.control_port);
            let listener = TcpListener::bind(&bind_addr).await?;
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await?;
        }
        Ok(())
    }
}

async fn handle_jsonrpc(
    axum::extract::State(rpc): axum::extract::State<RpcState>,
    axum::Extension(access): axum::Extension<AccessLevel>,
    body: String,
) -> axum::response::Response {
    let _ = access;
    let normalized = normalize_jsonrpc_request(&body);
    match rpc.methods.raw_json_request(&normalized, 1).await {
        Ok((response, _rx)) => axum::response::Response::builder()
            .header("Content-Type", "application/json")
            .body(axum::body::Body::from(response))
            .unwrap()
            .into_response(),
        Err(_) => {
            let err_response = serde_json::json!({
                "jsonrpc": "2.0",
                "error": {"code": -32700, "message": "Parse error"},
                "id": null
            });
            axum::response::Response::builder()
                .header("Content-Type", "application/json")
                .body(axum::body::Body::from(err_response.to_string()))
                .unwrap()
                .into_response()
        }
    }
}

fn normalize_jsonrpc_request(body: &str) -> String {
    if let Ok(mut obj) = serde_json::from_str::<serde_json::Value>(body)
        && let Some(map) = obj.as_object_mut()
    {
        if !map.contains_key("jsonrpc") {
            map.insert("jsonrpc".to_string(), serde_json::json!("2.0"));
        }
        if !map.contains_key("id") {
            map.insert("id".to_string(), serde_json::json!(0));
        }
        return serde_json::to_string(&obj).unwrap();
    }
    body.to_string()
}

async fn call_rpc_via_module(
    methods: &Methods,
    method: &str,
    params: &serde_json::Value,
) -> (Option<serde_json::Value>, Option<JsonRpcError>) {
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
        "id": 0
    });
    let request_str = serde_json::to_string(&request).unwrap();
    match methods.raw_json_request(&request_str, 1).await {
        Ok((response, _rx)) => {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&response) {
                if let Some(err) = parsed.get("error")
                    && !err.is_null()
                {
                    let code = err.get("code").and_then(|c| c.as_i64()).unwrap_or(-32000) as i32;
                    let message = err
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("Unknown error")
                        .to_string();
                    return (None, Some(JsonRpcError { code, message }));
                }
                (parsed.get("result").cloned(), None)
            } else {
                (
                    None,
                    Some(JsonRpcError {
                        code: -32603,
                        message: "Internal error".to_string(),
                    }),
                )
            }
        }
        Err(_) => (
            None,
            Some(JsonRpcError {
                code: -32700,
                message: "Parse error".to_string(),
            }),
        ),
    }
}

async fn handle_jsonprpc(
    axum::extract::State(rpc): axum::extract::State<RpcState>,
    axum::Extension(access): axum::Extension<AccessLevel>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> String {
    let callback = params
        .get("callback")
        .cloned()
        .unwrap_or_else(|| "cb".to_string());
    let method = params
        .get("method")
        .cloned()
        .unwrap_or_else(|| "status".to_string());
    let _ = access;
    let (result, err) = call_rpc_via_module(&rpc.methods, &method, &serde_json::json!([])).await;
    let response = if let Some(err) = err {
        JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            result: None,
            error: Some(serde_json::to_value(JsonRpcErrorBody::from(err)).unwrap()),
            id: serde_json::json!(0),
        }
    } else {
        JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            result,
            error: None,
            id: serde_json::json!(0),
        }
    };
    format!(
        "{}({});",
        callback,
        serde_json::to_string(&response).unwrap()
    )
}

async fn handle_xmlrpc(
    axum::extract::State(rpc): axum::extract::State<RpcState>,
    axum::Extension(access): axum::Extension<AccessLevel>,
    body: String,
) -> axum::response::Response<String> {
    let _ = access;
    let (method, params) = match xmlrpc::parse_method_call(&body) {
        Ok(parsed) => parsed,
        Err(err) => {
            let xml = xmlrpc::json_to_xmlrpc_fault(-32700, &format!("Parse error: {err}"));
            return axum::response::Response::builder()
                .header("Content-Type", "text/xml")
                .body(xml)
                .unwrap();
        }
    };

    let (result, err) = call_rpc_via_module(&rpc.methods, &method, &params).await;
    if let Some(err) = err {
        let xml = xmlrpc::json_to_xmlrpc_fault(err.code, &err.message);
        axum::response::Response::builder()
            .header("Content-Type", "text/xml")
            .body(xml)
            .unwrap()
    } else {
        let result = result.unwrap_or(serde_json::json!(null));
        let xml = xmlrpc::json_to_xmlrpc_response(&result);
        axum::response::Response::builder()
            .header("Content-Type", "text/xml")
            .body(xml)
            .unwrap()
    }
}

fn serve_embedded_file(path: &str) -> axum::response::Response {
    let file_path = path.trim_start_matches('/');
    let file_path = if file_path.is_empty() {
        "index.html"
    } else {
        file_path
    };

    match WebUiAssets::get(file_path) {
        Some(content) => {
            let mime = mime_guess::from_path(file_path)
                .first_or_octet_stream()
                .to_string();
            ([(header::CONTENT_TYPE, mime)], content.data.to_vec()).into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn handle_login_page() -> axum::response::Html<&'static str> {
    axum::response::Html(
        r#"<!DOCTYPE html>
<html><head><title>bergamot Login</title></head>
<body>
<h2>Login</h2>
<form method="post" action="/login">
<label>Username: <input name="username" type="text"></label><br>
<label>Password: <input name="password" type="password"></label><br>
<button type="submit">Login</button>
</form>
</body></html>"#,
    )
}

async fn handle_login_submit(
    axum::extract::State(auth): axum::extract::State<Arc<AuthState>>,
    axum::extract::Form(form): axum::extract::Form<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let username = form.get("username").map(|s| s.as_str()).unwrap_or("");
    let password = form.get("password").map(|s| s.as_str()).unwrap_or("");

    if let Some(level) = authenticate(username, password, &auth.config) {
        let cookie_val = crate::auth::create_session_cookie(level, &auth.config.control_password);
        let cookie = format!("bergamot_session={cookie_val}; Path=/; HttpOnly; SameSite=Strict");
        axum::response::Response::builder()
            .status(StatusCode::SEE_OTHER)
            .header("Location", "/")
            .header("Set-Cookie", cookie)
            .body(axum::body::Body::empty())
            .unwrap()
            .into_response()
    } else {
        axum::response::Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .body(axum::body::Body::from("Invalid credentials"))
            .unwrap()
            .into_response()
    }
}

async fn handle_logout() -> axum::response::Response {
    use axum::response::IntoResponse;
    axum::response::Response::builder()
        .status(StatusCode::SEE_OTHER)
        .header("Location", "/login")
        .header(
            "Set-Cookie",
            "bergamot_session=; Path=/; HttpOnly; Max-Age=0",
        )
        .body(axum::body::Body::empty())
        .unwrap()
        .into_response()
}

fn parse_query_params(query: Option<&str>) -> serde_json::Value {
    match query {
        Some(q) if !q.is_empty() => {
            let values: Vec<serde_json::Value> = q
                .split('&')
                .filter_map(|pair| {
                    let val = pair
                        .strip_prefix('=')
                        .unwrap_or(pair.split_once('=').map(|(_, v)| v).unwrap_or(pair));
                    if val.is_empty() {
                        return None;
                    }
                    let decoded = percent_encoding::percent_decode_str(val).decode_utf8_lossy();
                    let decoded = decoded.into_owned();
                    if let Ok(n) = decoded.parse::<u64>() {
                        Some(serde_json::Value::Number(n.into()))
                    } else if let Ok(n) = decoded.parse::<f64>() {
                        Some(serde_json::json!(n))
                    } else {
                        Some(serde_json::Value::String(decoded))
                    }
                })
                .collect();
            serde_json::Value::Array(values)
        }
        _ => serde_json::json!([]),
    }
}

async fn handle_api_shortcut(
    axum::extract::State(rpc): axum::extract::State<RpcState>,
    axum::extract::Path(method): axum::extract::Path<String>,
    axum::Extension(access): axum::Extension<AccessLevel>,
    uri: axum::http::Uri,
) -> Result<Json<JsonRpcResponse>, Json<JsonRpcResponse>> {
    let required = required_access(&method);
    if access < required {
        let err = JsonRpcError {
            code: -32600,
            message: "Access denied".to_string(),
        };
        return Err(Json(JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            result: None,
            error: Some(serde_json::to_value(JsonRpcErrorBody::from(err)).unwrap()),
            id: serde_json::json!(0),
        }));
    }

    let params = parse_query_params(uri.query());
    let (result, err) = call_rpc_via_module(&rpc.methods, &method, &params).await;
    if let Some(err) = err {
        Err(Json(JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            result: None,
            error: Some(serde_json::to_value(JsonRpcErrorBody::from(err)).unwrap()),
            id: serde_json::json!(0),
        }))
    } else {
        Ok(Json(JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            result,
            error: None,
            id: serde_json::json!(0),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn server_config() -> ServerConfig {
        ServerConfig {
            control_ip: "127.0.0.1".to_string(),
            control_port: 6789,
            secure_control: false,
            secure_port: 6791,
            secure_cert: None,
            secure_key: None,
            cert_store: std::path::PathBuf::new(),
            form_auth: false,
            authorized_ips: vec![],
            control_username: "admin".to_string(),
            control_password: "secret".to_string(),
            restricted_username: "ro".to_string(),
            restricted_password: "rosecret".to_string(),
            add_username: "add".to_string(),
            add_password: "addsecret".to_string(),
        }
    }

    #[test]
    fn validate_tls_config_requires_cert_and_key() {
        let mut cfg = server_config();
        cfg.secure_control = true;
        cfg.secure_cert = None;
        cfg.secure_key = None;
        assert!(WebServer::validate_tls_config(&cfg).is_err());

        cfg.secure_cert = Some(std::path::PathBuf::from("/tmp/cert.pem"));
        cfg.secure_key = None;
        assert!(WebServer::validate_tls_config(&cfg).is_err());

        cfg.secure_cert = None;
        cfg.secure_key = Some(std::path::PathBuf::from("/tmp/key.pem"));
        assert!(WebServer::validate_tls_config(&cfg).is_err());

        cfg.secure_cert = Some(std::path::PathBuf::from("/tmp/cert.pem"));
        cfg.secure_key = Some(std::path::PathBuf::from("/tmp/key.pem"));
        assert!(WebServer::validate_tls_config(&cfg).is_ok());
    }

    #[test]
    fn validate_tls_config_ok_when_not_secure() {
        let cfg = server_config();
        assert!(!cfg.secure_control);
        assert!(WebServer::validate_tls_config(&cfg).is_ok());
    }

    #[test]
    fn status_reflects_atomic_updates() {
        let state = AppState::default();
        assert_eq!(state.status().download_rate, 0);
        assert_eq!(state.status().remaining_size_mb, 0);

        state.download_rate_ref().store(512_000, Ordering::Relaxed);
        state
            .remaining_bytes_ref()
            .store(1024 * 1024 * 100, Ordering::Relaxed);

        let status = state.status();
        assert_eq!(status.download_rate, 512_000);
        assert_eq!(status.remaining_size_mb, 100);
    }

    fn rpc_state() -> RpcState {
        let state = Arc::new(AppState::default());
        let methods: Methods = build_rpc_module(state).into();
        RpcState {
            methods: Arc::new(methods),
        }
    }

    #[tokio::test]
    async fn jsonrpc_returns_status() {
        let rpc = rpc_state();
        let auth_state = AuthState {
            config: server_config(),
        };
        let app = Router::new()
            .route("/jsonrpc", post(handle_jsonrpc))
            .layer(middleware::from_fn_with_state(auth_state, auth_middleware))
            .with_state(rpc);

        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "status",
            "params": [],
            "id": 1
        });
        let body = serde_json::to_vec(&payload).unwrap();

        let request = Request::builder()
            .uri("/jsonrpc")
            .method("POST")
            .header("Authorization", "Basic YWRtaW46c2VjcmV0")
            .header("Content-Type", "application/json")
            .body(Body::from(body))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn jsonrpc_get_shortcut_returns_status() {
        let state = Arc::new(AppState::default());
        let config = Arc::new(server_config());
        let server = WebServer::new(config, state);
        let app = server.build_router();

        let request = Request::builder()
            .uri("/jsonrpc/status")
            .method("GET")
            .header("Authorization", "Basic YWRtaW46c2VjcmV0")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}
