use std::sync::Arc;

use axum::routing::{get, post};
use axum::{Json, Router, middleware};
use tokio::net::TcpListener;
use tower_http::compression::CompressionLayer;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;

use crate::auth::{AccessLevel, AuthState, auth_middleware, required_access};
use crate::config::ServerConfig;
use crate::error::{JsonRpcError, JsonRpcErrorBody};
use crate::rpc::{JsonRpcRequest, JsonRpcResponse, dispatch_rpc};
use crate::shutdown::ShutdownHandle;
use crate::status::StatusResponse;

#[derive(Debug, Clone)]
pub struct AppState {
    version: String,
    download_rate: u64,
    remaining_bytes: u64,
    queue: Option<nzbg_queue::QueueHandle>,
    shutdown: Option<ShutdownHandle>,
    disk: Option<std::sync::Arc<nzbg_diskstate::DiskState<nzbg_diskstate::JsonFormat>>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            version: "0.1.0".to_string(),
            download_rate: 0,
            remaining_bytes: 0,
            queue: None,
            shutdown: None,
            disk: None,
        }
    }
}

impl AppState {
    pub fn with_queue(mut self, queue: nzbg_queue::QueueHandle) -> Self {
        self.queue = Some(queue);
        self
    }

    pub fn with_shutdown(mut self, shutdown: ShutdownHandle) -> Self {
        self.shutdown = Some(shutdown);
        self
    }

    pub fn with_disk(
        mut self,
        disk: std::sync::Arc<nzbg_diskstate::DiskState<nzbg_diskstate::JsonFormat>>,
    ) -> Self {
        self.disk = Some(disk);
        self
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn queue_handle(&self) -> Option<&nzbg_queue::QueueHandle> {
        self.queue.as_ref()
    }

    pub fn shutdown_handle(&self) -> Option<&ShutdownHandle> {
        self.shutdown.as_ref()
    }

    pub fn disk(
        &self,
    ) -> Option<&std::sync::Arc<nzbg_diskstate::DiskState<nzbg_diskstate::JsonFormat>>> {
        self.disk.as_ref()
    }

    pub fn status(&self) -> StatusResponse {
        let fields = crate::status::SizeFields::from(self.remaining_bytes);
        StatusResponse {
            remaining_size_lo: fields.lo,
            remaining_size_hi: fields.hi,
            remaining_size_mb: fields.mb,
            download_rate: self.download_rate,
        }
    }
}

pub struct WebServer {
    config: Arc<ServerConfig>,
    state: Arc<AppState>,
}

impl WebServer {
    pub fn new(config: Arc<ServerConfig>, state: Arc<AppState>) -> Self {
        Self { config, state }
    }

    pub async fn run(&self) -> Result<(), Box<dyn std::error::Error>> {
        let auth_state = AuthState {
            config: (*self.config).clone(),
        };
        let app = Router::new()
            .route("/jsonrpc", post(handle_jsonrpc))
            .route("/jsonprpc", get(handle_jsonprpc))
            .route("/xmlrpc", post(handle_xmlrpc))
            .route("/{credentials}/jsonrpc/{method}", get(handle_api_shortcut))
            .fallback_service(ServeDir::new(&self.config.web_dir))
            .layer(CompressionLayer::new().gzip(true))
            .layer(CorsLayer::permissive())
            .layer(middleware::from_fn_with_state(
                auth_state.clone(),
                auth_middleware,
            ))
            .with_state(self.state.clone());

        let bind_addr = format!("{}:{}", self.config.control_ip, self.config.control_port);
        let listener = TcpListener::bind(&bind_addr).await?;
        axum::serve(listener, app).await?;
        Ok(())
    }
}

async fn handle_jsonrpc(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    axum::Extension(access): axum::Extension<AccessLevel>,
    Json(payload): Json<JsonRpcRequest>,
) -> Json<JsonRpcResponse> {
    let response = match dispatch_rpc(&payload.method, &payload.params, &state).await {
        Ok(result) => JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            result: Some(result),
            error: None,
            id: payload.id.unwrap_or_else(|| serde_json::json!(0)),
        },
        Err(err) => JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            result: None,
            error: Some(serde_json::to_value(JsonRpcErrorBody::from(err)).unwrap()),
            id: payload.id.unwrap_or_else(|| serde_json::json!(0)),
        },
    };

    let _ = access;
    Json(response)
}

async fn handle_jsonprpc(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
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
    let response = match dispatch_rpc(&method, &serde_json::json!([]), &state).await {
        Ok(result) => JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            result: Some(result),
            error: None,
            id: serde_json::json!(0),
        },
        Err(err) => JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            result: None,
            error: Some(serde_json::to_value(JsonRpcErrorBody::from(err)).unwrap()),
            id: serde_json::json!(0),
        },
    };
    let _ = access;
    format!(
        "{}({});",
        callback,
        serde_json::to_string(&response).unwrap()
    )
}

async fn handle_xmlrpc() -> Json<JsonRpcResponse> {
    Json(JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        result: None,
        error: Some(
            serde_json::to_value(JsonRpcErrorBody {
                code: -32000,
                message: "XML-RPC not implemented".to_string(),
            })
            .unwrap(),
        ),
        id: serde_json::json!(0),
    })
}

async fn handle_api_shortcut(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    axum::extract::Path(method): axum::extract::Path<String>,
    axum::Extension(access): axum::Extension<AccessLevel>,
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

    let result = dispatch_rpc(&method, &serde_json::json!([]), &state).await;
    match result {
        Ok(value) => Ok(Json(JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            result: Some(value),
            error: None,
            id: serde_json::json!(0),
        })),
        Err(err) => Err(Json(JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            result: None,
            error: Some(serde_json::to_value(JsonRpcErrorBody::from(err)).unwrap()),
            id: serde_json::json!(0),
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn server_config() -> ServerConfig {
        ServerConfig {
            control_ip: "127.0.0.1".to_string(),
            control_port: 6789,
            secure_control: false,
            secure_cert: None,
            secure_key: None,
            web_dir: std::path::PathBuf::from("/tmp"),
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

    #[tokio::test]
    async fn jsonrpc_returns_status() {
        let state = Arc::new(AppState::default());
        let auth_state = AuthState {
            config: server_config(),
        };
        let app = Router::new()
            .route("/jsonrpc", post(handle_jsonrpc))
            .layer(middleware::from_fn_with_state(auth_state, auth_middleware))
            .with_state(state);

        let payload = JsonRpcRequest {
            jsonrpc: Some("2.0".to_string()),
            method: "status".to_string(),
            params: serde_json::json!([]),
            id: Some(serde_json::json!(1)),
        };
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
}
