use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub control_ip: String,
    pub control_port: u16,
    pub secure_control: bool,
    pub secure_cert: Option<PathBuf>,
    pub secure_key: Option<PathBuf>,
    pub cert_store: PathBuf,
    pub form_auth: bool,
    pub authorized_ips: Vec<String>,
    pub control_username: String,
    pub control_password: String,
    pub restricted_username: String,
    pub restricted_password: String,
    pub add_username: String,
    pub add_password: String,
}
