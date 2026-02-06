use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExtensionKind {
    PostProcessing,
    Scan,
    Queue,
    Scheduler,
    Feed,
    Command,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionMetadata {
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub kind: Vec<ExtensionKind>,
    pub parameters: Vec<ExtensionParameter>,
    pub author: Option<String>,
    pub homepage: Option<String>,
    pub version: Option<String>,
    pub nzbget_min_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionParameter {
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub default: String,
    pub select: Option<Vec<String>>,
    pub param_type: ParamType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ParamType {
    String,
    Number { min: Option<f64>, max: Option<f64> },
    Bool,
    Password,
    Select,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScriptMessage {
    NzbName(String),
    NzbCategory(String),
    NzbDirectory(String),
    NzbFinalDir(String),
    NzbNzbName(String),
    NzbTop,
    NzbPause,
    NzbMark(MarkType),
    Log(LogLevel, String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MarkType {
    Good,
    Bad,
    Success,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LogLevel {
    Info,
    Warning,
    Error,
    Detail,
    Debug,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub messages: Vec<ScriptMessage>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PostProcessResult {
    Success,
    Failure,
    None,
}

impl PostProcessResult {
    pub fn from_exit_code(code: i32) -> Self {
        match code {
            93 => Self::Success,
            94 => Self::Failure,
            _ => Self::None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionInfo {
    pub metadata: ExtensionMetadata,
    pub path: PathBuf,
    pub enabled: bool,
    pub order: u32,
}

#[derive(Debug, Clone, Default)]
pub struct ExtensionManager {
    extensions: Vec<ExtensionInfo>,
    script_dirs: Vec<PathBuf>,
    config_values: HashMap<String, String>,
}

impl ExtensionManager {
    pub fn new(script_dirs: Vec<PathBuf>) -> Self {
        Self {
            extensions: Vec::new(),
            script_dirs,
            config_values: HashMap::new(),
        }
    }

    pub fn extensions_of_kind(&self, kind: ExtensionKind) -> impl Iterator<Item = &ExtensionInfo> {
        self.extensions
            .iter()
            .filter(move |ext| ext.metadata.kind.contains(&kind))
    }

    pub fn inject_config_vars(&self, ext: &ExtensionInfo, vars: &mut HashMap<String, String>) {
        for param in &ext.metadata.parameters {
            let key = format!("NZBPO_{}", param.name);
            let value = self
                .config_values
                .get(&format!("{}/{}", ext.metadata.name, param.name))
                .cloned()
                .unwrap_or_else(|| param.default.clone());
            vars.insert(key, value);
        }
    }

    pub fn set_config_value(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.config_values.insert(key.into(), value.into());
    }

    pub fn add_extension(&mut self, extension: ExtensionInfo) {
        self.extensions.push(extension);
    }

    pub fn script_dirs(&self) -> &[PathBuf] {
        &self.script_dirs
    }
}

pub fn parse_script_output(output: &str) -> Vec<ScriptMessage> {
    output
        .lines()
        .filter_map(|line| parse_script_line(line.trim()))
        .collect()
}

fn parse_script_line(line: &str) -> Option<ScriptMessage> {
    if let Some(rest) = line.strip_prefix("[NZB]") {
        return parse_nzb_message(rest.trim());
    }

    if let Some(rest) = line.strip_prefix("[INFO]") {
        return Some(ScriptMessage::Log(LogLevel::Info, rest.trim().to_string()));
    }
    if let Some(rest) = line.strip_prefix("[WARNING]") {
        return Some(ScriptMessage::Log(
            LogLevel::Warning,
            rest.trim().to_string(),
        ));
    }
    if let Some(rest) = line.strip_prefix("[ERROR]") {
        return Some(ScriptMessage::Log(LogLevel::Error, rest.trim().to_string()));
    }
    if let Some(rest) = line.strip_prefix("[DETAIL]") {
        return Some(ScriptMessage::Log(
            LogLevel::Detail,
            rest.trim().to_string(),
        ));
    }
    if let Some(rest) = line.strip_prefix("[DEBUG]") {
        return Some(ScriptMessage::Log(LogLevel::Debug, rest.trim().to_string()));
    }

    None
}

fn parse_nzb_message(line: &str) -> Option<ScriptMessage> {
    if line.eq_ignore_ascii_case("TOP") {
        return Some(ScriptMessage::NzbTop);
    }
    if line.eq_ignore_ascii_case("PAUSE") {
        return Some(ScriptMessage::NzbPause);
    }

    let (key, value) = line.split_once('=')?;
    let key = key.trim().to_uppercase();
    let value = value.trim();
    match key.as_str() {
        "NZBNAME" => Some(ScriptMessage::NzbName(value.to_string())),
        "CATEGORY" => Some(ScriptMessage::NzbCategory(value.to_string())),
        "DIRECTORY" => Some(ScriptMessage::NzbDirectory(value.to_string())),
        "FINALDIR" => Some(ScriptMessage::NzbFinalDir(value.to_string())),
        "NZBFILENAME" => Some(ScriptMessage::NzbNzbName(value.to_string())),
        "MARK" => parse_mark(value).map(ScriptMessage::NzbMark),
        _ => None,
    }
}

fn parse_mark(value: &str) -> Option<MarkType> {
    match value.trim().to_uppercase().as_str() {
        "GOOD" => Some(MarkType::Good),
        "BAD" => Some(MarkType::Bad),
        "SUCCESS" => Some(MarkType::Success),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_script_output_collects_messages() {
        let output = "[NZB] NZBNAME=Test\n[INFO] Hello\n[NZB] PAUSE\n";
        let messages = parse_script_output(output);
        assert_eq!(messages.len(), 3);
    }

    #[test]
    fn parse_script_output_parses_mark() {
        let output = "[NZB] MARK=BAD";
        let messages = parse_script_output(output);
        assert_eq!(messages, vec![ScriptMessage::NzbMark(MarkType::Bad)]);
    }

    #[test]
    fn extension_manager_injects_config_vars() {
        let mut manager = ExtensionManager::new(vec![PathBuf::from("/scripts")]);
        manager.set_config_value("MyExt/Token", "secret");

        let ext = ExtensionInfo {
            metadata: ExtensionMetadata {
                name: "MyExt".to_string(),
                display_name: "My Extension".to_string(),
                description: "desc".to_string(),
                kind: vec![ExtensionKind::PostProcessing],
                parameters: vec![ExtensionParameter {
                    name: "Token".to_string(),
                    display_name: "Token".to_string(),
                    description: "".to_string(),
                    default: "default".to_string(),
                    select: None,
                    param_type: ParamType::String,
                }],
                author: None,
                homepage: None,
                version: None,
                nzbget_min_version: None,
            },
            path: PathBuf::from("/scripts/myext.py"),
            enabled: true,
            order: 0,
        };

        let mut vars = HashMap::new();
        manager.inject_config_vars(&ext, &mut vars);
        assert_eq!(vars.get("NZBPO_Token"), Some(&"secret".to_string()));
    }
}
