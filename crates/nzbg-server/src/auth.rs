use axum::extract::{Request, State};
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
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let config = &state.config;

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
}
