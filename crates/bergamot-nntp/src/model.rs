use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewsServer {
    pub id: u32,
    pub name: String,
    pub active: bool,
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
    pub encryption: Encryption,
    pub cipher: Option<String>,
    pub connections: u32,
    pub retention: u32,
    pub level: u32,
    pub optional: bool,
    pub group: u32,
    pub join_group: bool,
    pub ip_version: IpVersion,
    pub cert_verification: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Encryption {
    None,
    Tls,
    StartTls,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IpVersion {
    Auto,
    IPv4Only,
    IPv6Only,
}

/// Parsed NNTP response line.
///
/// Response codes are defined in [RFC 3977 §3.2](https://datatracker.ietf.org/doc/html/rfc3977#section-3.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NntpResponse {
    pub code: u16,
    pub message: String,
}
