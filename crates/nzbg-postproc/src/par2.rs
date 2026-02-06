use std::path::{Path, PathBuf};

use async_trait::async_trait;
use tokio::process::Command;

use crate::error::Par2Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Par2Result {
    AllFilesOk,
    RepairComplete,
    RepairNeeded { blocks_needed: usize, blocks_available: usize },
    RepairFailed { reason: String },
}

#[async_trait]
pub trait Par2Engine: Send + Sync {
    async fn verify(&self, par2_file: &Path, working_dir: &Path) -> Result<Par2Result, Par2Error>;
    async fn repair(&self, par2_file: &Path, working_dir: &Path) -> Result<Par2Result, Par2Error>;
}

#[derive(Debug, Clone)]
pub struct Par2CommandLine {
    pub par2_path: PathBuf,
}

#[async_trait]
impl Par2Engine for Par2CommandLine {
    async fn verify(&self, par2_file: &Path, working_dir: &Path) -> Result<Par2Result, Par2Error> {
        let output = Command::new(&self.par2_path)
            .arg("verify")
            .arg(par2_file)
            .current_dir(working_dir)
            .output()
            .await
            .map_err(|e| Par2Error::CommandFailed {
                message: e.to_string(),
            })?;

        parse_par2_output(&output.stdout, &output.stderr, output.status.code())
    }

    async fn repair(&self, par2_file: &Path, working_dir: &Path) -> Result<Par2Result, Par2Error> {
        let output = Command::new(&self.par2_path)
            .arg("repair")
            .arg(par2_file)
            .current_dir(working_dir)
            .output()
            .await
            .map_err(|e| Par2Error::CommandFailed {
                message: e.to_string(),
            })?;

        parse_par2_output(&output.stdout, &output.stderr, output.status.code())
    }
}

fn parse_par2_output(
    stdout: &[u8],
    stderr: &[u8],
    exit_code: Option<i32>,
) -> Result<Par2Result, Par2Error> {
    let out = String::from_utf8_lossy(stdout);
    let err = String::from_utf8_lossy(stderr);
    let combined = format!("{out}\n{err}");

    if combined.contains("All files are correct") {
        return Ok(Par2Result::AllFilesOk);
    }

    if combined.contains("Repair complete") {
        return Ok(Par2Result::RepairComplete);
    }

    if combined.contains("Repair is required") {
        let (needed, available) = parse_repair_counts(&combined).unwrap_or((0, 0));
        return Ok(Par2Result::RepairNeeded {
            blocks_needed: needed,
            blocks_available: available,
        });
    }

    if let Some(code) = exit_code
        && code != 0
    {
        return Err(Par2Error::ExitStatus { code: Some(code) });
    }

    Err(Par2Error::ExitStatus { code: exit_code })
}

fn parse_repair_counts(text: &str) -> Option<(usize, usize)> {
    let mut needed = None;
    let mut available = None;

    for line in text.lines() {
        if let Some(value) = line.strip_prefix("Repair is required") {
            let nums: Vec<_> = value
                .split_whitespace()
                .filter_map(|tok| tok.parse::<usize>().ok())
                .collect();
            if nums.len() >= 2 {
                needed = Some(nums[0]);
                available = Some(nums[1]);
            }
        }
    }

    match (needed, available) {
        (Some(n), Some(a)) => Some((n, a)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_par2_output_detects_ok() {
        let output = b"All files are correct";
        let result = parse_par2_output(output, b"", Some(0)).unwrap();
        assert_eq!(result, Par2Result::AllFilesOk);
    }

    #[test]
    fn parse_par2_output_detects_repair_needed() {
        let output = b"Repair is required 3 5";
        let result = parse_par2_output(output, b"", Some(1)).unwrap();
        assert_eq!(
            result,
            Par2Result::RepairNeeded {
                blocks_needed: 3,
                blocks_available: 5
            }
        );
    }
}
