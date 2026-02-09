use std::collections::HashMap;

use crate::error::ConfigError;
use crate::model::{CategoryConfig, IpVersion, ServerConfig};

pub fn parse_config(content: &str) -> Result<HashMap<String, String>, ConfigError> {
    let mut values: HashMap<String, String> = HashMap::new();

    for (line_num, line) in content.lines().enumerate() {
        let line = line.trim();

        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let (key, raw_value) = line.split_once('=').ok_or(ConfigError::SyntaxError {
            line: line_num + 1,
            message: "expected Key=Value".into(),
        })?;

        let key = key.trim().to_string();
        let value = interpolate(raw_value.trim(), &values)?;
        values.insert(key, value);
    }

    Ok(values)
}

pub fn interpolate(value: &str, resolved: &HashMap<String, String>) -> Result<String, ConfigError> {
    let mut result = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '~' && result.is_empty() {
            if let Some(home) = dirs::home_dir() {
                result.push_str(&home.to_string_lossy());
            } else {
                result.push(ch);
            }
        } else if ch == '$' && chars.peek() == Some(&'{') {
            chars.next();
            let var_name: String = chars.by_ref().take_while(|&c| c != '}').collect();
            if let Some(var_value) = resolved.get(&var_name) {
                result.push_str(var_value);
            } else {
                return Err(ConfigError::UnknownVariable(var_name));
            }
        } else {
            result.push(ch);
        }
    }

    Ok(result)
}

pub fn extract_servers(raw: &HashMap<String, String>) -> Vec<ServerConfig> {
    let mut servers = Vec::new();

    for id in 1.. {
        let prefix = format!("Server{id}.");
        let host_key = format!("{prefix}Host");

        match raw.get(&host_key) {
            Some(host) if !host.is_empty() => {
                servers.push(ServerConfig {
                    id: id as u32,
                    active: parse_bool(raw.get(&format!("{prefix}Active")), true),
                    name: raw
                        .get(&format!("{prefix}Name"))
                        .cloned()
                        .unwrap_or_default(),
                    host: host.clone(),
                    port: raw
                        .get(&format!("{prefix}Port"))
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(119),
                    username: raw
                        .get(&format!("{prefix}Username"))
                        .cloned()
                        .unwrap_or_default(),
                    password: raw
                        .get(&format!("{prefix}Password"))
                        .cloned()
                        .unwrap_or_default(),
                    encryption: parse_bool(raw.get(&format!("{prefix}Encryption")), false),
                    connections: raw
                        .get(&format!("{prefix}Connections"))
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(1),
                    level: raw
                        .get(&format!("{prefix}Level"))
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0),
                    optional: parse_bool(raw.get(&format!("{prefix}Optional")), false),
                    group: raw
                        .get(&format!("{prefix}Group"))
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0),
                    retention: raw
                        .get(&format!("{prefix}Retention"))
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0),
                    cipher: raw
                        .get(&format!("{prefix}Cipher"))
                        .cloned()
                        .unwrap_or_default(),
                    cert_verification: parse_bool(
                        raw.get(&format!("{prefix}CertVerification")),
                        false,
                    ),
                    ip_version: parse_ip_version(raw.get(&format!("{prefix}IpVersion"))),
                });
            }
            _ => break,
        }
    }

    servers
}

pub fn extract_categories(raw: &HashMap<String, String>) -> Vec<CategoryConfig> {
    let mut categories = Vec::new();

    for id in 1.. {
        let prefix = format!("Category{id}.");
        let name_key = format!("{prefix}Name");

        match raw.get(&name_key) {
            Some(name) if !name.is_empty() => {
                let dest_dir = raw
                    .get(&format!("{prefix}DestDir"))
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|| std::path::PathBuf::from(""));
                let extensions = raw
                    .get(&format!("{prefix}Extensions"))
                    .map(|value| {
                        value
                            .split(',')
                            .map(|entry| entry.trim().to_string())
                            .filter(|entry| !entry.is_empty())
                            .collect()
                    })
                    .unwrap_or_default();
                let aliases = raw
                    .get(&format!("{prefix}Aliases"))
                    .map(|value| {
                        value
                            .split(',')
                            .map(|entry| entry.trim().to_string())
                            .filter(|entry| !entry.is_empty())
                            .collect()
                    })
                    .unwrap_or_default();

                categories.push(CategoryConfig {
                    name: name.clone(),
                    dest_dir,
                    unpack: parse_bool(raw.get(&format!("{prefix}Unpack")), true),
                    extensions,
                    aliases,
                });
            }
            _ => break,
        }
    }

    categories
}

pub fn extract_feeds(raw: &HashMap<String, String>) -> Vec<super::model::FeedConfigEntry> {
    let mut feeds = Vec::new();

    for id in 1u32.. {
        let prefix = format!("Feed{id}.");
        let name_key = format!("{prefix}Name");

        match raw.get(&name_key) {
            Some(name) if !name.is_empty() => {
                let url = raw
                    .get(&format!("{prefix}URL"))
                    .cloned()
                    .unwrap_or_default();
                let filter = raw
                    .get(&format!("{prefix}Filter"))
                    .cloned()
                    .unwrap_or_default();
                let interval_min = raw
                    .get(&format!("{prefix}Interval"))
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(15);
                let backlog = parse_bool(raw.get(&format!("{prefix}Backlog")), false);
                let pause_nzb = parse_bool(raw.get(&format!("{prefix}PauseNzb")), false);
                let category = raw
                    .get(&format!("{prefix}Category"))
                    .cloned()
                    .unwrap_or_default();
                let priority = raw
                    .get(&format!("{prefix}Priority"))
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                let extensions = raw
                    .get(&format!("{prefix}Extensions"))
                    .map(|value| {
                        value
                            .split(',')
                            .map(|e| e.trim().to_string())
                            .filter(|e| !e.is_empty())
                            .collect()
                    })
                    .unwrap_or_default();

                feeds.push(super::model::FeedConfigEntry {
                    id,
                    name: name.clone(),
                    url,
                    filter,
                    interval_min,
                    backlog,
                    pause_nzb,
                    category,
                    priority,
                    extensions,
                });
            }
            _ => break,
        }
    }

    feeds
}

pub fn parse_bool(value: Option<&String>, default: bool) -> bool {
    match value.map(|s| s.to_lowercase()).as_deref() {
        Some("yes" | "true" | "1") => true,
        Some("no" | "false" | "0") => false,
        _ => default,
    }
}

pub fn parse_ip_version(value: Option<&String>) -> IpVersion {
    match value.map(|s| s.to_lowercase()).as_deref() {
        Some("ipv4") => IpVersion::IPv4,
        Some("ipv6") => IpVersion::IPv6,
        _ => IpVersion::Auto,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_config_interpolates_variables() {
        let content = "MainDir=/data\nDestDir=${MainDir}/dst\n";
        let parsed = parse_config(content).expect("parse");
        assert_eq!(parsed.get("DestDir").unwrap(), "/data/dst");
    }

    #[test]
    fn extract_servers_stops_on_gap() {
        let content = "Server1.Host=example.com\nServer1.Connections=2\n";
        let parsed = parse_config(content).expect("parse");
        let servers = extract_servers(&parsed);
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].connections, 2);
    }

    #[test]
    fn extract_categories_reads_aliases() {
        let content = "Category1.Name=Movies\nCategory1.Aliases=Films, Cinema\n";
        let parsed = parse_config(content).expect("parse");
        let categories = extract_categories(&parsed);
        assert_eq!(categories[0].aliases, vec!["Films", "Cinema"]);
    }

    #[test]
    fn extract_feeds_parses_config() {
        let content = "Feed1.Name=My Feed\nFeed1.URL=https://example.com/rss\nFeed1.Filter=A: title(.*)\nFeed1.Interval=30\nFeed1.Category=TV\n";
        let parsed = parse_config(content).expect("parse");
        let feeds = extract_feeds(&parsed);
        assert_eq!(feeds.len(), 1);
        assert_eq!(feeds[0].id, 1);
        assert_eq!(feeds[0].name, "My Feed");
        assert_eq!(feeds[0].url, "https://example.com/rss");
        assert_eq!(feeds[0].interval_min, 30);
        assert_eq!(feeds[0].category, "TV");
    }

    #[test]
    fn extract_feeds_stops_on_gap() {
        let content = "Feed1.Name=A\nFeed1.URL=http://a\nFeed3.Name=C\nFeed3.URL=http://c\n";
        let parsed = parse_config(content).expect("parse");
        let feeds = extract_feeds(&parsed);
        assert_eq!(feeds.len(), 1);
    }
}
