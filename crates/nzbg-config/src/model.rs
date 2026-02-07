use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    pub main_dir: PathBuf,
    pub dest_dir: PathBuf,
    pub inter_dir: PathBuf,
    pub nzb_dir: PathBuf,
    pub queue_dir: PathBuf,
    pub temp_dir: PathBuf,
    pub script_dir: PathBuf,
    pub log_file: PathBuf,
    pub config_template: PathBuf,
    pub required_dir: Vec<PathBuf>,
    pub cert_store: PathBuf,
    pub control_ip: String,
    pub control_port: u16,
    pub control_username: String,
    pub control_password: String,
    pub secure_control: bool,
    pub secure_port: u16,
    pub secure_cert: Option<PathBuf>,
    pub secure_key: Option<PathBuf>,
    pub form_auth: bool,
    pub authorized_ip: Vec<String>,
    pub restricted_username: String,
    pub restricted_password: String,
    pub add_username: String,
    pub add_password: String,
    pub servers: Vec<ServerConfig>,
    pub categories: Vec<CategoryConfig>,
    pub download_rate: u32,
    pub article_cache: u32,
    pub disk_space: u32,
    pub keep_history: u32,
    pub append_category_dir: bool,
    pub unpack_cleanup_disk: bool,
    pub ext_cleanup_disk: String,
    pub post_strategy: nzbg_core::models::PostStrategy,
    raw: HashMap<String, String>,
}

impl Config {
    pub fn from_raw(raw: HashMap<String, String>) -> Self {
        let main_dir = raw
            .get("MainDir")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("~/downloads"));
        let dest_dir = raw
            .get("DestDir")
            .map(PathBuf::from)
            .unwrap_or_else(|| main_dir.join("dst"));
        let inter_dir = raw
            .get("InterDir")
            .map(PathBuf::from)
            .unwrap_or_else(|| main_dir.join("inter"));
        let nzb_dir = raw
            .get("NzbDir")
            .map(PathBuf::from)
            .unwrap_or_else(|| main_dir.join("nzb"));
        let queue_dir = raw
            .get("QueueDir")
            .map(PathBuf::from)
            .unwrap_or_else(|| main_dir.join("queue"));
        let temp_dir = raw
            .get("TempDir")
            .map(PathBuf::from)
            .unwrap_or_else(|| main_dir.join("tmp"));
        let script_dir = raw
            .get("ScriptDir")
            .map(PathBuf::from)
            .unwrap_or_else(|| main_dir.join("scripts"));
        let log_file = raw
            .get("LogFile")
            .map(PathBuf::from)
            .unwrap_or_else(|| dest_dir.join("nzbg.log"));
        let config_template = raw
            .get("ConfigTemplate")
            .map(PathBuf::from)
            .unwrap_or_else(|| main_dir.join("config.template"));
        let required_dir = raw
            .get("RequiredDir")
            .map(|value| value.split(',').map(|p| PathBuf::from(p.trim())).collect())
            .unwrap_or_default();
        let cert_store = raw
            .get("CertStore")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(""));

        let control_ip = raw
            .get("ControlIP")
            .cloned()
            .unwrap_or_else(|| "0.0.0.0".to_string());
        let control_port = raw
            .get("ControlPort")
            .and_then(|value| value.parse().ok())
            .unwrap_or(6789);
        let control_username = raw
            .get("ControlUsername")
            .cloned()
            .unwrap_or_else(|| "nzbget".to_string());
        let control_password = raw
            .get("ControlPassword")
            .cloned()
            .unwrap_or_else(|| "tegbzn6789".to_string());
        let secure_control = raw
            .get("SecureControl")
            .map(|value| matches!(value.as_str(), "yes" | "true" | "1"))
            .unwrap_or(false);
        let secure_port = raw
            .get("SecurePort")
            .and_then(|value| value.parse().ok())
            .unwrap_or(6791);
        let secure_cert = raw.get("SecureCert").map(PathBuf::from);
        let secure_key = raw.get("SecureKey").map(PathBuf::from);
        let form_auth = raw
            .get("FormAuth")
            .map(|v| matches!(v.as_str(), "yes" | "true" | "1"))
            .unwrap_or(false);
        let authorized_ip = raw
            .get("AuthorizedIP")
            .map(|value| value.split(',').map(|p| p.trim().to_string()).collect())
            .unwrap_or_default();
        let restricted_username = raw.get("RestrictedUsername").cloned().unwrap_or_default();
        let restricted_password = raw.get("RestrictedPassword").cloned().unwrap_or_default();
        let add_username = raw.get("AddUsername").cloned().unwrap_or_default();
        let add_password = raw.get("AddPassword").cloned().unwrap_or_default();

        let servers = crate::parse::extract_servers(&raw);
        let categories = crate::parse::extract_categories(&raw);

        let download_rate = raw
            .get("DownloadRate")
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        let article_cache = raw
            .get("ArticleCache")
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        let disk_space = raw
            .get("DiskSpace")
            .and_then(|value| value.parse().ok())
            .unwrap_or(250);
        let keep_history = raw
            .get("KeepHistory")
            .and_then(|value| value.parse().ok())
            .unwrap_or(30);

        let append_category_dir = raw
            .get("AppendCategoryDir")
            .map(|v| matches!(v.as_str(), "yes" | "true" | "1"))
            .unwrap_or(true);
        let unpack_cleanup_disk = raw
            .get("UnpackCleanupDisk")
            .map(|v| matches!(v.as_str(), "yes" | "true" | "1"))
            .unwrap_or(true);
        let ext_cleanup_disk = raw
            .get("ExtCleanupDisk")
            .cloned()
            .unwrap_or_else(|| ".par2, .sfv".to_string());

        let post_strategy = raw
            .get("PostStrategy")
            .map(|value| match value.to_lowercase().as_str() {
                "balanced" => nzbg_core::models::PostStrategy::Balanced,
                "rocket" => nzbg_core::models::PostStrategy::Rocket,
                "aggressive" => nzbg_core::models::PostStrategy::Aggressive,
                _ => nzbg_core::models::PostStrategy::Sequential,
            })
            .unwrap_or_default();

        Self {
            main_dir,
            dest_dir,
            inter_dir,
            nzb_dir,
            queue_dir,
            temp_dir,
            script_dir,
            log_file,
            config_template,
            required_dir,
            cert_store,
            control_ip,
            control_port,
            control_username,
            control_password,
            secure_control,
            secure_port,
            secure_cert,
            secure_key,
            form_auth,
            authorized_ip,
            restricted_username,
            restricted_password,
            add_username,
            add_password,
            servers,
            categories,
            download_rate,
            article_cache,
            disk_space,
            keep_history,
            append_category_dir,
            unpack_cleanup_disk,
            ext_cleanup_disk,
            post_strategy,
            raw,
        }
    }

    pub fn set_option(&mut self, key: &str, value: &str) -> Result<(), crate::error::ConfigError> {
        let resolved = crate::parse::interpolate(value, &self.raw)?;
        self.raw.insert(key.to_string(), resolved.clone());
        match key {
            "DownloadRate" => {
                self.download_rate =
                    resolved
                        .parse()
                        .map_err(|_| crate::error::ConfigError::InvalidValue {
                            option: key.into(),
                            value: value.into(),
                        })?;
            }
            "DiskSpace" => {
                self.disk_space =
                    resolved
                        .parse()
                        .map_err(|_| crate::error::ConfigError::InvalidValue {
                            option: key.into(),
                            value: value.into(),
                        })?;
            }
            _ => {}
        }
        Ok(())
    }

    pub fn save(&self, path: &std::path::Path) -> Result<(), crate::error::ConfigError> {
        let mut output = String::new();
        let mut keys: Vec<_> = self.raw.keys().collect();
        keys.sort();
        for key in keys {
            if let Some(value) = self.raw.get(key) {
                output.push_str(&format!("{key}={value}\n"));
            }
        }
        std::fs::write(path, &output)?;
        Ok(())
    }

    pub fn raw(&self) -> &HashMap<String, String> {
        &self.raw
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerConfig {
    pub id: u32,
    pub active: bool,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub encryption: bool,
    pub connections: u32,
    pub level: u32,
    pub optional: bool,
    pub group: u32,
    pub retention: u32,
    pub cipher: String,
    pub cert_verification: bool,
    pub ip_version: IpVersion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpVersion {
    Auto,
    IPv4,
    IPv6,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedConfigEntry {
    pub id: u32,
    pub name: String,
    pub url: String,
    pub filter: String,
    pub interval_min: u32,
    pub backlog: bool,
    pub pause_nzb: bool,
    pub category: String,
    pub priority: i32,
    pub extensions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryConfig {
    pub name: String,
    pub dest_dir: PathBuf,
    pub unpack: bool,
    pub extensions: Vec<String>,
    pub aliases: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_strategy_parses_from_config() {
        let mut raw = HashMap::new();
        raw.insert("PostStrategy".to_string(), "Rocket".to_string());
        let config = Config::from_raw(raw);
        assert_eq!(
            config.post_strategy,
            nzbg_core::models::PostStrategy::Rocket
        );
    }

    #[test]
    fn post_strategy_defaults_to_sequential() {
        let config = Config::from_raw(HashMap::new());
        assert_eq!(
            config.post_strategy,
            nzbg_core::models::PostStrategy::Sequential
        );
    }

    #[test]
    fn post_strategy_case_insensitive() {
        let mut raw = HashMap::new();
        raw.insert("PostStrategy".to_string(), "aggressive".to_string());
        let config = Config::from_raw(raw);
        assert_eq!(
            config.post_strategy,
            nzbg_core::models::PostStrategy::Aggressive
        );
    }
}
