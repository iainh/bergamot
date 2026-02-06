use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

use crate::config::Config;
use crate::error::PostProcessError;
use crate::par2::{Par2Engine, Par2Result};

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

pub struct PostProcessor<E: Par2Engine> {
    rx: mpsc::Receiver<PostProcessRequest>,
    _config: Arc<Config>,
    history: Arc<Mutex<Vec<String>>>,
    par2: Arc<E>,
}

impl<E: Par2Engine> PostProcessor<E> {
    pub fn new(
        rx: mpsc::Receiver<PostProcessRequest>,
        config: Arc<Config>,
        history: Arc<Mutex<Vec<String>>>,
        par2: Arc<E>,
    ) -> Self {
        Self {
            rx,
            _config: config,
            history,
            par2,
        }
    }

    pub async fn run(&mut self) {
        while let Some(request) = self.rx.recv().await {
            self.process(request).await;
        }
    }

    async fn process(&self, req: PostProcessRequest) {
        let mut ctx = PostProcessContext::new(req);

        ctx.set_stage(PostStage::ParRenaming);
        ctx.set_stage(PostStage::ParVerifying);
        if let Ok(result) = self.par_verify(&ctx).await {
            ctx.par_result = Some(result);
        }

        if matches!(ctx.par_result, Some(Par2Result::RepairNeeded { .. })) {
            ctx.set_stage(PostStage::ParRepairing);
            let _ = self.par_repair(&ctx).await;
        }

        ctx.set_stage(PostStage::Finished);
        let mut history = self.history.lock().expect("history lock");
        history.push(ctx.request.nzb_name.clone());
    }

    async fn par_verify(&self, ctx: &PostProcessContext) -> Result<Par2Result, PostProcessError> {
        let par2_file = ctx.request.working_dir.join(format!("{}.par2", ctx.request.nzb_name));
        let result = self
            .par2
            .verify(&par2_file, &ctx.request.working_dir)
            .await?;
        Ok(result)
    }

    async fn par_repair(&self, ctx: &PostProcessContext) -> Result<Par2Result, PostProcessError> {
        let par2_file = ctx.request.working_dir.join(format!("{}.par2", ctx.request.nzb_name));
        let result = self
            .par2
            .repair(&par2_file, &ctx.request.working_dir)
            .await?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::par2::Par2Result;
    use async_trait::async_trait;

    #[derive(Debug)]
    struct FakePar2;

    #[async_trait]
    impl Par2Engine for FakePar2 {
        async fn verify(&self, _par2_file: &std::path::Path, _working_dir: &std::path::Path) -> Result<Par2Result, crate::error::Par2Error> {
            Ok(Par2Result::AllFilesOk)
        }

        async fn repair(&self, _par2_file: &std::path::Path, _working_dir: &std::path::Path) -> Result<Par2Result, crate::error::Par2Error> {
            Ok(Par2Result::RepairComplete)
        }
    }

    #[tokio::test]
    async fn post_processor_records_history() {
        let (tx, rx) = mpsc::channel(1);
        let config = Arc::new(Config {
            par2_path: PathBuf::from("par2"),
            dest_dir: PathBuf::from("/tmp"),
            append_category_dir: false,
            password_file: None,
            unpack_cleanup_disk: false,
            ext_cleanup_disk: String::new(),
        });
        let history = Arc::new(Mutex::new(Vec::new()));
        let par2 = Arc::new(FakePar2);
        let mut processor = PostProcessor::new(rx, config, history.clone(), par2);

        let request = PostProcessRequest {
            nzb_id: 1,
            nzb_name: "example".to_string(),
            working_dir: PathBuf::from("/tmp/example"),
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
}
