use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use async_trait::async_trait;
use tokio::process::Command;

use crate::error::Par2Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Par2Result {
    AllFilesOk,
    RepairComplete,
    RepairNeeded {
        blocks_needed: usize,
        blocks_available: usize,
    },
    RepairFailed {
        reason: String,
    },
}

#[async_trait]
pub trait Par2Engine: Send + Sync {
    async fn verify(&self, par2_file: &Path, working_dir: &Path) -> Result<Par2Result, Par2Error> {
        self.verify_with_progress(par2_file, working_dir, None)
            .await
    }
    async fn repair(&self, par2_file: &Path, working_dir: &Path) -> Result<Par2Result, Par2Error> {
        self.repair_with_progress(par2_file, working_dir, None)
            .await
    }
    async fn verify_with_progress(
        &self,
        par2_file: &Path,
        working_dir: &Path,
        progress: Option<Arc<AtomicU32>>,
    ) -> Result<Par2Result, Par2Error>;
    async fn repair_with_progress(
        &self,
        par2_file: &Path,
        working_dir: &Path,
        progress: Option<Arc<AtomicU32>>,
    ) -> Result<Par2Result, Par2Error>;
}

#[derive(Debug, Clone)]
pub struct Par2CommandLine {
    pub par2_path: PathBuf,
}

#[async_trait]
impl Par2Engine for Par2CommandLine {
    async fn verify_with_progress(
        &self,
        par2_file: &Path,
        working_dir: &Path,
        _progress: Option<Arc<AtomicU32>>,
    ) -> Result<Par2Result, Par2Error> {
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

    async fn repair_with_progress(
        &self,
        par2_file: &Path,
        working_dir: &Path,
        _progress: Option<Arc<AtomicU32>>,
    ) -> Result<Par2Result, Par2Error> {
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

#[derive(Debug, Clone)]
pub struct NativePar2Engine;

#[async_trait]
impl Par2Engine for NativePar2Engine {
    async fn verify_with_progress(
        &self,
        _par2_file: &Path,
        working_dir: &Path,
        progress: Option<Arc<AtomicU32>>,
    ) -> Result<Par2Result, Par2Error> {
        let working_dir = working_dir.to_path_buf();
        tokio::task::spawn_blocking(move || native_verify(&working_dir, progress.as_ref()))
            .await
            .map_err(|e| Par2Error::CommandFailed {
                message: e.to_string(),
            })?
    }

    async fn repair_with_progress(
        &self,
        _par2_file: &Path,
        working_dir: &Path,
        progress: Option<Arc<AtomicU32>>,
    ) -> Result<Par2Result, Par2Error> {
        let working_dir = working_dir.to_path_buf();
        tokio::task::spawn_blocking(move || native_repair(&working_dir, progress.as_ref()))
            .await
            .map_err(|e| Par2Error::CommandFailed {
                message: e.to_string(),
            })?
    }
}

fn native_verify(
    working_dir: &Path,
    progress: Option<&Arc<AtomicU32>>,
) -> Result<Par2Result, Par2Error> {
    tracing::debug!(dir = %working_dir.display(), "parsing par2 recovery set");
    let rs =
        bergamot_par2::parse_recovery_set(working_dir).map_err(|e| Par2Error::CommandFailed {
            message: e.to_string(),
        })?;
    if let Some(p) = progress {
        p.store(100, Ordering::Relaxed);
    }
    tracing::debug!(
        files = rs.files.len(),
        recovery_slices = rs.recovery_slices.len(),
        slice_size = rs.slice_size,
        "parsed par2 recovery set"
    );

    let result = bergamot_par2::verify_recovery_set_with_progress_range(
        &rs,
        working_dir,
        progress.cloned().as_ref(),
        100,
        1000,
    );

    if result.all_ok() {
        tracing::info!(dir = %working_dir.display(), "all files intact");
        Ok(Par2Result::AllFilesOk)
    } else {
        let needed = result.blocks_needed();
        let available = rs.recovery_slices.len();
        tracing::info!(
            dir = %working_dir.display(),
            blocks_needed = needed,
            blocks_available = available,
            "file damage detected, repair needed"
        );
        Ok(Par2Result::RepairNeeded {
            blocks_needed: needed,
            blocks_available: available,
        })
    }
}

fn native_repair(
    working_dir: &Path,
    progress: Option<&Arc<AtomicU32>>,
) -> Result<Par2Result, Par2Error> {
    tracing::debug!(dir = %working_dir.display(), "parsing par2 recovery set for repair");
    let rs =
        bergamot_par2::parse_recovery_set(working_dir).map_err(|e| Par2Error::CommandFailed {
            message: e.to_string(),
        })?;
    if let Some(p) = progress {
        p.store(50, Ordering::Relaxed);
    }

    let verify = bergamot_par2::verify_recovery_set_with_progress_range(
        &rs,
        working_dir,
        progress.cloned().as_ref(),
        50,
        100,
    );

    if verify.all_ok() {
        tracing::info!(dir = %working_dir.display(), "repair skipped, all files intact");
        return Ok(Par2Result::AllFilesOk);
    }

    tracing::info!(
        dir = %working_dir.display(),
        blocks_needed = verify.blocks_needed(),
        recovery_slices = rs.recovery_slices.len(),
        "starting file repair"
    );

    match bergamot_par2::repair_recovery_set_with_progress(
        &rs,
        &verify,
        working_dir,
        progress.cloned().as_ref(),
    ) {
        Ok(report) => {
            tracing::debug!(
                repaired_slices = report.repaired_slices,
                repaired_files = ?report.repaired_files,
                "par2 repair: repair_recovery_set completed"
            );
            let post_verify = bergamot_par2::verify_recovery_set(&rs, working_dir);
            if post_verify.all_ok() {
                tracing::info!(
                    dir = %working_dir.display(),
                    repaired_slices = report.repaired_slices,
                    "file repair successful"
                );
                Ok(Par2Result::RepairComplete)
            } else {
                let reason = format!(
                    "repair finished but verification still failing ({} blocks needed)",
                    post_verify.blocks_needed()
                );
                tracing::warn!(dir = %working_dir.display(), %reason, "par2 repair: post-repair verification failed");
                Ok(Par2Result::RepairFailed { reason })
            }
        }
        Err(bergamot_par2::Par2RepairError::NotEnoughRecoverySlices { needed, available }) => {
            tracing::warn!(
                dir = %working_dir.display(),
                blocks_needed = needed,
                blocks_available = available,
                "par2 repair: not enough recovery slices"
            );
            Ok(Par2Result::RepairNeeded {
                blocks_needed: needed,
                blocks_available: available,
            })
        }
        Err(e) => {
            let reason = e.to_string();
            tracing::error!(dir = %working_dir.display(), %reason, "par2 repair: failed");
            Ok(Par2Result::RepairFailed { reason })
        }
    }
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
