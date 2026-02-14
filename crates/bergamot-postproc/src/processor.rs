use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tokio::sync::mpsc;

use crate::cleanup::cleanup_archives;
use crate::config::Config;
use crate::error::PostProcessError;
use crate::mover::{move_to_destination, resolve_dest_dir};
use crate::par2::{Par2Engine, Par2Result};
use crate::unpack::{UnpackResult, Unpacker, detect_archives};

pub struct ExtensionContext {
    pub nzb_id: u32,
    pub nzb_name: String,
    pub working_dir: PathBuf,
    pub category: String,
    pub par_status: String,
    pub unpack_status: String,
    pub parameters: Vec<(String, String)>,
}

#[async_trait::async_trait]
pub trait ExtensionExecutor: Send + Sync {
    async fn run_post_process(&self, ctx: &ExtensionContext) -> Result<(), PostProcessError>;
}

pub struct PostTimings {
    pub total_sec: u64,
    pub par_sec: u64,
    pub repair_sec: u64,
    pub unpack_sec: u64,
}

#[async_trait::async_trait]
pub trait PostStatusReporter: Send + Sync {
    async fn report_stage(&self, nzb_id: u32, stage: bergamot_core::models::PostStage);
    async fn report_progress(&self, nzb_id: u32, progress: u32);
    async fn report_done(
        &self,
        nzb_id: u32,
        par: bergamot_core::models::ParStatus,
        unpack: bergamot_core::models::UnpackStatus,
        mv: bergamot_core::models::MoveStatus,
        timings: PostTimings,
    );
}

#[derive(Debug, Clone)]
pub struct PostProcessRequest {
    pub nzb_id: i64,
    pub nzb_name: String,
    pub working_dir: PathBuf,
    pub category: Option<String>,
    pub parameters: Vec<(String, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostStage {
    Queued,
    Loading,
    ParRenaming,
    ParVerifying,
    ParRepairing,
    RarRenaming,
    Unpacking,
    Cleanup,
    Moving,
    Extensions,
    Finished,
}

#[derive(Debug)]
pub struct PostProcessContext {
    pub request: PostProcessRequest,
    pub stage: PostStage,
    pub par_result: Option<Par2Result>,
}

impl PostProcessContext {
    pub fn new(request: PostProcessRequest) -> Self {
        Self {
            request,
            stage: PostStage::Queued,
            par_result: None,
        }
    }

    pub fn set_stage(&mut self, stage: PostStage) {
        self.stage = stage;
    }
}

pub struct PostProcessor<E: Par2Engine, U: Unpacker> {
    rx: mpsc::Receiver<PostProcessRequest>,
    config: Arc<Config>,
    history: Arc<Mutex<Vec<String>>>,
    par2: Arc<E>,
    unpacker: Arc<U>,
    extensions: Option<Arc<dyn ExtensionExecutor>>,
    reporter: Option<Arc<dyn PostStatusReporter>>,
}

impl<E: Par2Engine, U: Unpacker> PostProcessor<E, U> {
    pub fn new(
        rx: mpsc::Receiver<PostProcessRequest>,
        config: Arc<Config>,
        history: Arc<Mutex<Vec<String>>>,
        par2: Arc<E>,
        unpacker: Arc<U>,
    ) -> Self {
        Self {
            rx,
            config,
            history,
            par2,
            unpacker,
            extensions: None,
            reporter: None,
        }
    }

    pub fn with_extensions(mut self, executor: Arc<dyn ExtensionExecutor>) -> Self {
        self.extensions = Some(executor);
        self
    }

    pub fn with_reporter(mut self, reporter: Arc<dyn PostStatusReporter>) -> Self {
        self.reporter = Some(reporter);
        self
    }

    pub async fn run(&mut self) {
        while let Some(request) = self.rx.recv().await {
            self.process(request).await;
        }
    }

    async fn process(&self, req: PostProcessRequest) {
        let nzb_id = req.nzb_id as u32;
        let mut ctx = PostProcessContext::new(req);
        let pp_start = Instant::now();
        tracing::info!(nzb = %ctx.request.nzb_name, "starting post-processing");

        if let Some(reporter) = &self.reporter {
            reporter
                .report_stage(nzb_id, bergamot_core::models::PostStage::Queued)
                .await;
        }

        ctx.set_stage(PostStage::ParRenaming);
        if let Some(reporter) = &self.reporter {
            reporter
                .report_stage(nzb_id, bergamot_core::models::PostStage::ParRenaming)
                .await;
        }
        if let Some(par2_file) = find_par2_file(&ctx.request.working_dir, &ctx.request.nzb_name) {
            match bergamot_par2::parse_recovery_set_from_file(&par2_file) {
                Ok(recovery_set) => {
                    let renames = crate::deobfuscate::deobfuscate_files(
                        &ctx.request.working_dir,
                        &recovery_set,
                    )
                    .await;
                    if !renames.is_empty() {
                        tracing::info!(
                            nzb = %ctx.request.nzb_name,
                            count = renames.len(),
                            "deobfuscated files using PAR2 metadata"
                        );
                    }
                }
                Err(err) => {
                    tracing::debug!(
                        nzb = %ctx.request.nzb_name,
                        error = %err,
                        "skipping deobfuscation: could not parse PAR2"
                    );
                }
            }
        }

        ctx.set_stage(PostStage::ParVerifying);
        if let Some(reporter) = &self.reporter {
            reporter
                .report_stage(nzb_id, bergamot_core::models::PostStage::ParVerifying)
                .await;
        }
        let par_start = Instant::now();
        tracing::info!(nzb = %ctx.request.nzb_name, "verifying file integrity (par2)");
        let progress = Arc::new(AtomicU32::new(0));
        let verify_result = {
            let reporter = self.reporter.clone();
            let progress = progress.clone();
            let poller = spawn_progress_poller(nzb_id, progress.clone(), reporter);
            let result = self.par_verify(&ctx, Some(progress)).await;
            poller.abort();
            result
        };
        match verify_result {
            Ok(result) => {
                tracing::info!(nzb = %ctx.request.nzb_name, result = ?result, "file integrity check complete");
                ctx.par_result = Some(result);
            }
            Err(err) => {
                tracing::warn!(nzb = %ctx.request.nzb_name, error = %err, "par2 verify failed");
            }
        }
        let par_elapsed = par_start.elapsed();

        let mut repair_elapsed = std::time::Duration::ZERO;
        if matches!(ctx.par_result, Some(Par2Result::RepairNeeded { .. })) {
            ctx.set_stage(PostStage::ParRepairing);
            if let Some(reporter) = &self.reporter {
                reporter
                    .report_stage(nzb_id, bergamot_core::models::PostStage::ParRepairing)
                    .await;
            }
            let repair_start = Instant::now();
            tracing::info!(nzb = %ctx.request.nzb_name, "repairing damaged files (par2)");
            let progress = Arc::new(AtomicU32::new(0));
            let repair_result = {
                let reporter = self.reporter.clone();
                let progress = progress.clone();
                let poller = spawn_progress_poller(nzb_id, progress.clone(), reporter);
                let result = self.par_repair(&ctx, Some(progress)).await;
                poller.abort();
                result
            };
            match repair_result {
                Ok(result) => {
                    tracing::info!(nzb = %ctx.request.nzb_name, result = ?result, "file repair complete");
                    ctx.par_result = Some(result);
                }
                Err(err) => {
                    tracing::warn!(nzb = %ctx.request.nzb_name, error = %err, "par2 repair failed");
                }
            }
            repair_elapsed = repair_start.elapsed();
        }

        ctx.set_stage(PostStage::Unpacking);
        if let Some(reporter) = &self.reporter {
            reporter
                .report_stage(nzb_id, bergamot_core::models::PostStage::Unpacking)
                .await;
        }
        let unpack_start = Instant::now();
        let unpack_status = match self.unpack(&ctx).await {
            Ok(()) => bergamot_core::models::UnpackStatus::Success,
            Err(PostProcessError::Unpack { ref message }) if message.contains("password") => {
                tracing::warn!(nzb = %ctx.request.nzb_name, "archive is password-protected");
                bergamot_core::models::UnpackStatus::Password
            }
            Err(err) => {
                tracing::error!(nzb = %ctx.request.nzb_name, error = %err, "unpacking failed");
                bergamot_core::models::UnpackStatus::Failure
            }
        };
        let unpack_elapsed = unpack_start.elapsed();

        if self.config.unpack_cleanup_disk {
            ctx.set_stage(PostStage::Cleanup);
            if let Err(err) = cleanup_archives(&ctx.request.working_dir).await {
                tracing::warn!(nzb = %ctx.request.nzb_name, error = %err, "archive cleanup failed");
            }
        }

        ctx.set_stage(PostStage::Moving);
        if let Some(reporter) = &self.reporter {
            reporter
                .report_stage(nzb_id, bergamot_core::models::PostStage::Moving)
                .await;
        }
        let dest = resolve_dest_dir(
            &self.config.dest_dir,
            ctx.request.category.as_deref(),
            self.config.append_category_dir,
        );
        let move_status = match move_to_destination(&ctx.request.working_dir, &dest).await {
            Ok(()) => {
                tracing::info!(nzb = %ctx.request.nzb_name, dest = %dest.display(), "moved files to destination");
                bergamot_core::models::MoveStatus::Success
            }
            Err(err) => {
                tracing::error!(
                    nzb = %ctx.request.nzb_name,
                    src = %ctx.request.working_dir.display(),
                    dest = %dest.display(),
                    error = %err,
                    "moving files to destination failed"
                );
                bergamot_core::models::MoveStatus::Failure
            }
        };

        if let Some(executor) = &self.extensions {
            ctx.set_stage(PostStage::Extensions);
            if let Some(reporter) = &self.reporter {
                reporter
                    .report_stage(nzb_id, bergamot_core::models::PostStage::Executing)
                    .await;
            }
            let par_status_str = match ctx.par_result {
                Some(Par2Result::AllFilesOk) | Some(Par2Result::RepairComplete) => "SUCCESS",
                Some(Par2Result::RepairNeeded { .. }) | Some(Par2Result::RepairFailed { .. }) => {
                    "FAILURE"
                }
                None => "NONE",
            };
            let ext_ctx = ExtensionContext {
                nzb_id: ctx.request.nzb_id as u32,
                nzb_name: ctx.request.nzb_name.clone(),
                working_dir: ctx.request.working_dir.clone(),
                category: ctx.request.category.clone().unwrap_or_default(),
                par_status: par_status_str.to_string(),
                unpack_status: "SUCCESS".to_string(),
                parameters: ctx.request.parameters.clone(),
            };
            if let Err(err) = executor.run_post_process(&ext_ctx).await {
                tracing::warn!("extension execution failed: {err}");
            }
        }

        let par_status = match ctx.par_result {
            Some(Par2Result::AllFilesOk) | Some(Par2Result::RepairComplete) => {
                bergamot_core::models::ParStatus::Success
            }
            Some(Par2Result::RepairNeeded { .. }) | Some(Par2Result::RepairFailed { .. }) => {
                bergamot_core::models::ParStatus::Failure
            }
            None => bergamot_core::models::ParStatus::None,
        };

        let total_elapsed = pp_start.elapsed();
        tracing::info!(
            nzb = %ctx.request.nzb_name,
            total_ms = total_elapsed.as_millis() as u64,
            par_ms = par_elapsed.as_millis() as u64,
            repair_ms = repair_elapsed.as_millis() as u64,
            unpack_ms = unpack_elapsed.as_millis() as u64,
            "post-processing complete"
        );
        let timings = PostTimings {
            total_sec: total_elapsed.as_secs(),
            par_sec: par_elapsed.as_secs_f64().round() as u64,
            repair_sec: repair_elapsed.as_secs_f64().round() as u64,
            unpack_sec: unpack_elapsed.as_secs_f64().round() as u64,
        };
        if let Some(reporter) = &self.reporter {
            reporter
                .report_done(nzb_id, par_status, unpack_status, move_status, timings)
                .await;
        }

        ctx.set_stage(PostStage::Finished);
        let mut history = self.history.lock().expect("history lock");
        history.push(ctx.request.nzb_name.clone());
    }

    async fn par_verify(
        &self,
        ctx: &PostProcessContext,
        progress: Option<Arc<AtomicU32>>,
    ) -> Result<Par2Result, PostProcessError> {
        let par2_file = match find_par2_file(&ctx.request.working_dir, &ctx.request.nzb_name) {
            Some(path) => path,
            None => {
                tracing::info!(nzb = %ctx.request.nzb_name, "no repair data found, skipping integrity check");
                return Ok(Par2Result::AllFilesOk);
            }
        };
        let result = self
            .par2
            .verify_with_progress(&par2_file, &ctx.request.working_dir, progress)
            .await?;
        Ok(result)
    }

    async fn par_repair(
        &self,
        ctx: &PostProcessContext,
        progress: Option<Arc<AtomicU32>>,
    ) -> Result<Par2Result, PostProcessError> {
        let par2_file = match find_par2_file(&ctx.request.working_dir, &ctx.request.nzb_name) {
            Some(path) => path,
            None => return Ok(Par2Result::AllFilesOk),
        };
        let result = self
            .par2
            .repair_with_progress(&par2_file, &ctx.request.working_dir, progress)
            .await?;
        Ok(result)
    }

    async fn unpack(&self, ctx: &PostProcessContext) -> Result<(), PostProcessError> {
        let archives = detect_archives(&ctx.request.working_dir);
        if archives.is_empty() {
            tracing::info!(nzb = %ctx.request.nzb_name, "no archives to extract");
            return Ok(());
        }
        tracing::info!(nzb = %ctx.request.nzb_name, count = archives.len(), "extracting archives");
        for (_, archive_path) in &archives {
            let result = self
                .unpacker
                .unpack(archive_path, &ctx.request.working_dir)
                .await;
            match result {
                UnpackResult::Success => {}
                UnpackResult::Failure(msg) => {
                    return Err(PostProcessError::Unpack { message: msg });
                }
                UnpackResult::Password => {
                    return Err(PostProcessError::Unpack {
                        message: "archive is password-protected".to_string(),
                    });
                }
            }
        }
        Ok(())
    }
}

fn spawn_progress_poller(
    nzb_id: u32,
    progress: Arc<AtomicU32>,
    reporter: Option<Arc<dyn PostStatusReporter>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut last = u32::MAX;
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            let val = progress.load(Ordering::Relaxed);
            if val != last {
                last = val;
                if let Some(reporter) = &reporter {
                    reporter.report_progress(nzb_id, val).await;
                }
            }
        }
    })
}

pub fn find_par2_file(
    working_dir: &std::path::Path,
    _nzb_name: &str,
) -> Option<std::path::PathBuf> {
    let mut candidates: Vec<std::path::PathBuf> = std::fs::read_dir(working_dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("par2"))
        })
        .collect();

    candidates.sort();
    if let Some(found) = candidates.first() {
        tracing::debug!(path = %found.display(), "found par2 file");
    } else {
        tracing::debug!(dir = %working_dir.display(), "no par2 files found");
    }
    candidates.into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::par2::Par2Result;
    use async_trait::async_trait;
    use std::path::Path;

    #[derive(Debug)]
    struct FakePar2;

    #[async_trait]
    impl Par2Engine for FakePar2 {
        async fn verify_with_progress(
            &self,
            _par2_file: &Path,
            _working_dir: &Path,
            _progress: Option<Arc<AtomicU32>>,
        ) -> Result<Par2Result, crate::error::Par2Error> {
            Ok(Par2Result::AllFilesOk)
        }

        async fn repair_with_progress(
            &self,
            _par2_file: &Path,
            _working_dir: &Path,
            _progress: Option<Arc<AtomicU32>>,
        ) -> Result<Par2Result, crate::error::Par2Error> {
            Ok(Par2Result::RepairComplete)
        }
    }

    #[derive(Debug)]
    struct FakeUnpacker;

    #[async_trait]
    impl Unpacker for FakeUnpacker {
        async fn unpack(&self, archive: &Path, working_dir: &Path) -> UnpackResult {
            let stem = archive
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("out");
            let output_file = working_dir.join(format!("{stem}.unpacked"));
            let _ = std::fs::write(&output_file, b"extracted content");
            UnpackResult::Success
        }
    }

    fn test_config(dest_dir: PathBuf) -> Arc<Config> {
        Arc::new(Config {
            par2_path: PathBuf::from("par2"),
            dest_dir,
            append_category_dir: false,
            password_file: None,
            unpack_cleanup_disk: false,
            ext_cleanup_disk: String::new(),
        })
    }

    #[tokio::test]
    async fn post_processor_records_history() {
        let working = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        let (tx, rx) = mpsc::channel(1);
        let config = test_config(dest.path().to_path_buf());
        let history = Arc::new(Mutex::new(Vec::new()));
        let par2 = Arc::new(FakePar2);
        let unpacker = Arc::new(FakeUnpacker);
        let mut processor = PostProcessor::new(rx, config, history.clone(), par2, unpacker);

        let request = PostProcessRequest {
            nzb_id: 1,
            nzb_name: "example".to_string(),
            working_dir: working.path().to_path_buf(),
            category: None,
            parameters: vec![],
        };

        let handle = tokio::spawn(async move {
            processor.run().await;
        });

        tx.send(request).await.expect("send");
        drop(tx);
        handle.await.expect("join");

        let history = history.lock().expect("lock");
        assert_eq!(history.as_slice(), ["example"]);
    }

    #[test]
    fn find_par2_prefers_exact_match() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("example.par2"), b"par2").unwrap();
        std::fs::write(dir.path().join("example.vol00+01.par2"), b"vol").unwrap();

        let result = find_par2_file(dir.path(), "example");
        assert_eq!(result, Some(dir.path().join("example.par2")));
    }

    #[test]
    fn find_par2_falls_back_to_discovered_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("other.par2"), b"par2").unwrap();

        let result = find_par2_file(dir.path(), "example");
        assert_eq!(result, Some(dir.path().join("other.par2")));
    }

    #[test]
    fn find_par2_finds_vol_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("data.vol00+01.par2"), b"vol").unwrap();

        let result = find_par2_file(dir.path(), "example");
        assert_eq!(result, Some(dir.path().join("data.vol00+01.par2")));
    }

    #[test]
    fn find_par2_returns_none_when_no_par2_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("data.rar"), b"rar").unwrap();

        let result = find_par2_file(dir.path(), "example");
        assert!(result.is_none());
    }

    #[test]
    fn find_par2_deterministic_with_multiple() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("b.par2"), b"par2").unwrap();
        std::fs::write(dir.path().join("a.par2"), b"par2").unwrap();

        let result = find_par2_file(dir.path(), "nonexistent");
        assert_eq!(result, Some(dir.path().join("a.par2")));
    }

    #[tokio::test]
    async fn full_pipeline_unpack_cleanup_move() {
        let working = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();

        std::fs::write(working.path().join("archive.rar"), b"rar data").unwrap();
        std::fs::write(working.path().join("archive.r00"), b"r00 data").unwrap();
        std::fs::write(working.path().join("archive.par2"), b"par2 data").unwrap();

        let (tx, rx) = mpsc::channel(1);
        let config = Arc::new(Config {
            par2_path: PathBuf::from("par2"),
            dest_dir: dest.path().to_path_buf(),
            append_category_dir: true,
            password_file: None,
            unpack_cleanup_disk: true,
            ext_cleanup_disk: String::new(),
        });
        let history = Arc::new(Mutex::new(Vec::new()));
        let par2 = Arc::new(FakePar2);
        let unpacker = Arc::new(FakeUnpacker);
        let mut processor = PostProcessor::new(rx, config, history.clone(), par2, unpacker);

        let request = PostProcessRequest {
            nzb_id: 1,
            nzb_name: "test_nzb".to_string(),
            working_dir: working.path().to_path_buf(),
            category: Some("movies".to_string()),
            parameters: vec![],
        };

        let handle = tokio::spawn(async move {
            processor.run().await;
        });

        tx.send(request).await.expect("send");
        drop(tx);
        handle.await.expect("join");

        let history = history.lock().expect("lock");
        assert_eq!(history.as_slice(), ["test_nzb"]);

        assert!(dest.path().join("movies").join("archive.unpacked").exists());
    }

    struct FakeExtensionExecutor {
        calls: Mutex<Vec<(u32, String, PathBuf, String)>>,
    }

    impl FakeExtensionExecutor {
        fn new() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl ExtensionExecutor for FakeExtensionExecutor {
        async fn run_post_process(&self, ctx: &ExtensionContext) -> Result<(), PostProcessError> {
            self.calls.lock().unwrap().push((
                ctx.nzb_id,
                ctx.nzb_name.clone(),
                ctx.working_dir.clone(),
                ctx.category.clone(),
            ));
            Ok(())
        }
    }

    #[tokio::test]
    async fn extensions_called_during_post_processing() {
        let working = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        let (tx, rx) = mpsc::channel(1);
        let config = test_config(dest.path().to_path_buf());
        let history = Arc::new(Mutex::new(Vec::new()));
        let par2 = Arc::new(FakePar2);
        let unpacker = Arc::new(FakeUnpacker);
        let executor = Arc::new(FakeExtensionExecutor::new());
        let executor_ref = executor.clone();
        let mut processor = PostProcessor::new(rx, config, history.clone(), par2, unpacker)
            .with_extensions(executor);

        let request = PostProcessRequest {
            nzb_id: 7,
            nzb_name: "ext-test".to_string(),
            working_dir: working.path().to_path_buf(),
            category: Some("tv".to_string()),
            parameters: vec![],
        };

        let handle = tokio::spawn(async move {
            processor.run().await;
        });

        tx.send(request).await.expect("send");
        drop(tx);
        handle.await.expect("join");

        let calls = executor_ref.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, 7);
        assert_eq!(calls[0].1, "ext-test");
        assert_eq!(calls[0].3, "tv");
    }

    #[tokio::test]
    async fn no_extensions_still_works() {
        let working = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        let (tx, rx) = mpsc::channel(1);
        let config = test_config(dest.path().to_path_buf());
        let history = Arc::new(Mutex::new(Vec::new()));
        let par2 = Arc::new(FakePar2);
        let unpacker = Arc::new(FakeUnpacker);
        let mut processor = PostProcessor::new(rx, config, history.clone(), par2, unpacker);

        let request = PostProcessRequest {
            nzb_id: 1,
            nzb_name: "no-ext".to_string(),
            working_dir: working.path().to_path_buf(),
            category: None,
            parameters: vec![],
        };

        let handle = tokio::spawn(async move {
            processor.run().await;
        });

        tx.send(request).await.expect("send");
        drop(tx);
        handle.await.expect("join");

        let history = history.lock().expect("lock");
        assert_eq!(history.as_slice(), ["no-ext"]);
    }
}
