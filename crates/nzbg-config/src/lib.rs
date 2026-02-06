mod error;
mod model;
mod parse;

pub use crate::error::ConfigError;
pub use crate::model::{CategoryConfig, Config, FeedConfigEntry, IpVersion, ServerConfig};
pub use crate::parse::{
    extract_feeds, extract_servers, interpolate, parse_config, parse_ip_version,
};
