use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::Response;
use base64::Engine;

use crate::config::ServerConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AccessLevel {
    Control,
    Restricted,
    Add,
    Denied,
}

#[derive(Debug, Clone)]
pub struct AuthState {
    pub config: ServerConfig,
}

pub async fn auth_middleware(
    State(state): State<AuthState>,
    connect_info: Option<ConnectInfo<SocketAddr>>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let config = &state.config;

    if !config.authorized_ips.is_empty() {
        let client_ip = connect_info
            .map(|ci| ci.0.ip().to_string())
            .unwrap_or_default();
        if !is_ip_allowed(&client_ip, &config.authorized_ips) {
            return Err(StatusCode::FORBIDDEN);
        }
    }

    let access = extract_url_credentials(&request)
        .and_then(|(user, pass)| authenticate(&user, &pass, config))
        .or_else(|| {
            extract_basic_auth(&request).and_then(|(user, pass)| authenticate(&user, &pass, config))
        })
        .unwrap_or(AccessLevel::Denied);

    if access == AccessLevel::Denied {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let mut request = request;
    request.extensions_mut().insert(access);

    Ok(next.run(request).await)
}

pub fn authenticate(username: &str, password: &str, config: &ServerConfig) -> Option<AccessLevel> {
    if username == config.control_username && password == config.control_password {
        Some(AccessLevel::Control)
    } else if username == config.restricted_username && password == config.restricted_password {
        Some(AccessLevel::Restricted)
    } else if username == config.add_username && password == config.add_password {
        Some(AccessLevel::Add)
    } else {
        None
    }
}

pub fn required_access(method: &str) -> AccessLevel {
    match method {
        "version" => AccessLevel::Add,
        "append" | "scan" => AccessLevel::Add,
        "status" | "sysinfo" | "systemhealth" | "listgroups" | "listfiles" | "history" | "log"
        | "loadlog" | "servervolumes" | "config" | "configtemplates" | "loadconfig" => {
            AccessLevel::Restricted
        }
        _ => AccessLevel::Control,
    }
}

pub fn is_ip_allowed(client_ip: &str, allowed: &[String]) -> bool {
    if client_ip == "127.0.0.1" || client_ip == "::1" {
        return true;
    }
    if allowed.is_empty() {
        return true;
    }
    for pattern in allowed {
        if pattern.contains('/') {
            if cidr_matches(client_ip, pattern) {
                return true;
            }
        } else if pattern.contains('*') {
            if wildcard_matches(client_ip, pattern) {
                return true;
            }
        } else if client_ip == pattern {
            return true;
        }
    }
    false
}

fn wildcard_matches(ip: &str, pattern: &str) -> bool {
    let ip_parts: Vec<&str> = ip.split('.').collect();
    let pattern_parts: Vec<&str> = pattern.split('.').collect();
    if ip_parts.len() != pattern_parts.len() {
        return false;
    }
    ip_parts
        .iter()
        .zip(pattern_parts.iter())
        .all(|(ip_part, pat_part)| *pat_part == "*" || ip_part == pat_part)
}

fn cidr_matches(ip: &str, cidr: &str) -> bool {
    let Some((network, prefix_str)) = cidr.split_once('/') else {
        return false;
    };
    let Ok(prefix_len) = prefix_str.parse::<u32>() else {
        return false;
    };
    let Some(ip_addr) = parse_ipv4(ip) else {
        return false;
    };
    let Some(net_addr) = parse_ipv4(network) else {
        return false;
    };
    if prefix_len > 32 {
        return false;
    }
    let mask = if prefix_len == 0 {
        0
    } else {
        !0u32 << (32 - prefix_len)
    };
    (ip_addr & mask) == (net_addr & mask)
}

fn parse_ipv4(ip: &str) -> Option<u32> {
    let parts: Vec<&str> = ip.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    let a: u32 = parts[0].parse().ok()?;
    let b: u32 = parts[1].parse().ok()?;
    let c: u32 = parts[2].parse().ok()?;
    let d: u32 = parts[3].parse().ok()?;
    Some((a << 24) | (b << 16) | (c << 8) | d)
}

fn extract_basic_auth(request: &Request) -> Option<(String, String)> {
    let header = request.headers().get("Authorization")?.to_str().ok()?;
    let encoded = header.strip_prefix("Basic ")?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    let decoded = String::from_utf8(decoded).ok()?;
    let (user, pass) = decoded.split_once(':')?;
    Some((user.to_string(), pass.to_string()))
}

fn extract_url_credentials(request: &Request) -> Option<(String, String)> {
    let path = request.uri().path();
    let mut parts = path.trim_start_matches('/').split('/');
    let credentials = parts.next()?;
    let (user, pass) = credentials.split_once(':')?;
    Some((user.to_string(), pass.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> ServerConfig {
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

    #[test]
    fn authenticate_matches_roles() {
        let config = config();
        assert_eq!(
            authenticate("admin", "secret", &config),
            Some(AccessLevel::Control)
        );
        assert_eq!(
            authenticate("ro", "rosecret", &config),
            Some(AccessLevel::Restricted)
        );
        assert_eq!(
            authenticate("add", "addsecret", &config),
            Some(AccessLevel::Add)
        );
        assert_eq!(authenticate("bad", "bad", &config), None);
    }

    #[test]
    fn required_access_defaults_control() {
        assert_eq!(required_access("shutdown"), AccessLevel::Control);
        assert_eq!(required_access("append"), AccessLevel::Add);
        assert_eq!(required_access("status"), AccessLevel::Restricted);
    }

    #[test]
    fn ip_allowed_when_allowlist_empty() {
        let allowed: Vec<String> = vec![];
        assert!(is_ip_allowed("192.168.1.5", &allowed));
    }

    #[test]
    fn ip_allowed_when_in_list() {
        let allowed = vec!["192.168.1.5".to_string(), "10.0.0.1".to_string()];
        assert!(is_ip_allowed("192.168.1.5", &allowed));
        assert!(is_ip_allowed("10.0.0.1", &allowed));
    }

    #[test]
    fn ip_denied_when_not_in_list() {
        let allowed = vec!["192.168.1.5".to_string()];
        assert!(!is_ip_allowed("10.0.0.1", &allowed));
    }

    #[test]
    fn ip_allowed_with_wildcard() {
        let allowed = vec!["192.168.1.*".to_string()];
        assert!(is_ip_allowed("192.168.1.99", &allowed));
        assert!(!is_ip_allowed("192.168.2.1", &allowed));
    }

    #[test]
    fn ip_allowed_with_cidr_subnet() {
        let allowed = vec!["192.168.1.0/24".to_string()];
        assert!(is_ip_allowed("192.168.1.55", &allowed));
        assert!(!is_ip_allowed("192.168.2.1", &allowed));
    }

    #[test]
    fn localhost_always_allowed() {
        let allowed = vec!["10.0.0.1".to_string()];
        assert!(is_ip_allowed("127.0.0.1", &allowed));
        assert!(is_ip_allowed("::1", &allowed));
    }
}
