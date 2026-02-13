#![recursion_limit = "256"]

mod auth;
mod config;
mod error;
mod rpc;
mod rpc_module;
mod server;
mod shutdown;
mod status;
pub mod tls;
mod xmlrpc;

pub use crate::auth::{AccessLevel, AuthState};
pub use crate::config::ServerConfig;
pub use crate::error::JsonRpcError;
pub use crate::rpc::{JsonRpcRequest, JsonRpcResponse};
pub use crate::server::{AppState, WebServer, spawn_stats_updater};
pub use crate::shutdown::ShutdownHandle;
pub use crate::status::{SizeFields, StatusResponse};
