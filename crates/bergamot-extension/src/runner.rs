use std::collections::HashMap;
use std::path::Path;

use crate::{ExtensionInfo, ExtensionManager, ExtensionResult, PostProcessResult};

#[async_trait::async_trait]
pub trait ProcessRunner: Send + Sync {
    async fn run(
        &self,
        cmd: &Path,
        env: &HashMap<String, String>,
        cwd: &Path,
    ) -> Result<ProcessOutput, std::io::Error>;
}

#[derive(Debug, Clone)]
pub struct ProcessOutput {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

pub struct RealProcessRunner;

#[async_trait::async_trait]
impl ProcessRunner for RealProcessRunner {
    async fn run(
        &self,
        cmd: &Path,
        env: &HashMap<String, String>,
        cwd: &Path,
    ) -> Result<ProcessOutput, std::io::Error> {
        let output = tokio::process::Command::new(cmd)
            .envs(env)
            .current_dir(cwd)
            .output()
            .await?;

        Ok(ProcessOutput {
            exit_code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    }
}

pub struct ExtensionRunner<R: ProcessRunner> {
    runner: R,
    manager: ExtensionManager,
}

impl<R: ProcessRunner> ExtensionRunner<R> {
    pub fn new(runner: R, manager: ExtensionManager) -> Self {
        Self { runner, manager }
    }

    pub async fn run_post_process(
        &self,
        ext: &ExtensionInfo,
        nzbpp_vars: &HashMap<String, String>,
        working_dir: &Path,
    ) -> Result<ExtensionResult, std::io::Error> {
        let env = self.build_env(ext, nzbpp_vars);
        let output = self.runner.run(&ext.path, &env, working_dir).await?;
        let messages = crate::parse_script_output(&output.stdout);
        Ok(ExtensionResult {
            exit_code: output.exit_code.unwrap_or(1),
            stdout: output.stdout,
            stderr: output.stderr,
            messages,
        })
    }

    fn build_env(
        &self,
        ext: &ExtensionInfo,
        nzbpp_vars: &HashMap<String, String>,
    ) -> HashMap<String, String> {
        let mut env = HashMap::new();

        env.insert("NZBOP_TEMPDIR".to_string(), "/tmp".to_string());

        self.manager.inject_config_vars(ext, &mut env);

        for (key, value) in nzbpp_vars {
            env.insert(key.clone(), value.clone());
        }

        env
    }
}

pub struct NzbppContext<'a> {
    pub nzb_name: &'a str,
    pub nzb_id: u32,
    pub category: &'a str,
    pub working_dir: &'a Path,
    pub dest_dir: &'a Path,
    pub final_dir: &'a Path,
    pub total_status: &'a str,
    pub par_status: &'a str,
    pub unpack_status: &'a str,
    pub parameters: &'a [(String, String)],
}

pub fn build_nzbpp_env(ctx: &NzbppContext<'_>) -> HashMap<String, String> {
    let mut env = HashMap::new();
    env.insert("NZBPP_NZBNAME".to_string(), ctx.nzb_name.to_string());
    env.insert("NZBPP_NZBID".to_string(), ctx.nzb_id.to_string());
    env.insert("NZBPP_CATEGORY".to_string(), ctx.category.to_string());
    env.insert(
        "NZBPP_DIRECTORY".to_string(),
        ctx.working_dir.to_string_lossy().to_string(),
    );
    env.insert(
        "NZBPP_FINALDIR".to_string(),
        ctx.final_dir.to_string_lossy().to_string(),
    );
    env.insert(
        "NZBPP_NZBFILENAME".to_string(),
        ctx.dest_dir.to_string_lossy().to_string(),
    );
    env.insert(
        "NZBPP_TOTALSTATUS".to_string(),
        ctx.total_status.to_string(),
    );
    env.insert("NZBPP_PARSTATUS".to_string(), ctx.par_status.to_string());
    env.insert(
        "NZBPP_UNPACKSTATUS".to_string(),
        ctx.unpack_status.to_string(),
    );

    for (key, value) in ctx.parameters {
        env.insert(format!("NZBPR_{key}"), value.clone());
    }

    env
}

pub fn interpret_post_result(result: &ExtensionResult) -> PostProcessResult {
    PostProcessResult::from_exit_code(result.exit_code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ExtensionKind, ExtensionMetadata, ExtensionParameter, ParamType};
    use std::path::PathBuf;
    use std::sync::Mutex;

    struct FakeRunner {
        output: ProcessOutput,
        last_env: Mutex<Option<HashMap<String, String>>>,
    }

    impl FakeRunner {
        fn new(stdout: &str, exit_code: i32) -> Self {
            Self {
                output: ProcessOutput {
                    exit_code: Some(exit_code),
                    stdout: stdout.to_string(),
                    stderr: String::new(),
                },
                last_env: Mutex::new(None),
            }
        }
    }

    #[async_trait::async_trait]
    impl ProcessRunner for FakeRunner {
        async fn run(
            &self,
            _cmd: &Path,
            env: &HashMap<String, String>,
            _cwd: &Path,
        ) -> Result<ProcessOutput, std::io::Error> {
            *self.last_env.lock().unwrap() = Some(env.clone());
            Ok(self.output.clone())
        }
    }

    fn test_extension() -> ExtensionInfo {
        ExtensionInfo {
            metadata: ExtensionMetadata {
                name: "TestExt".to_string(),
                display_name: "Test Extension".to_string(),
                description: "test".to_string(),
                kind: vec![ExtensionKind::PostProcessing],
                parameters: vec![ExtensionParameter {
                    name: "ApiKey".to_string(),
                    display_name: "API Key".to_string(),
                    description: "key".to_string(),
                    default: "default-key".to_string(),
                    select: None,
                    param_type: ParamType::String,
                }],
                author: None,
                homepage: None,
                version: None,
                nzbget_min_version: None,
            },
            path: PathBuf::from("/scripts/test.py"),
            enabled: true,
            order: 0,
        }
    }

    #[tokio::test]
    async fn runner_passes_env_and_parses_output() {
        let runner = FakeRunner::new("[NZB] NZBNAME=Changed\n[INFO] done\n", 93);
        let manager = ExtensionManager::new(vec![]);
        let ext_runner = ExtensionRunner::new(runner, manager);

        let nzbpp = build_nzbpp_env(&NzbppContext {
            nzb_name: "Test",
            nzb_id: 1,
            category: "tv",
            working_dir: Path::new("/tmp/work"),
            dest_dir: Path::new("/tmp/dest"),
            final_dir: Path::new("/tmp/final"),
            total_status: "SUCCESS",
            par_status: "SUCCESS",
            unpack_status: "SUCCESS",
            parameters: &[],
        });

        let ext = test_extension();
        let result = ext_runner
            .run_post_process(&ext, &nzbpp, Path::new("/tmp/work"))
            .await
            .expect("run");

        assert_eq!(result.exit_code, 93);
        assert_eq!(result.messages.len(), 2);
    }

    #[tokio::test]
    async fn runner_includes_config_vars() {
        let runner = FakeRunner::new("", 0);
        let mut manager = ExtensionManager::new(vec![]);
        manager.set_config_value("TestExt/ApiKey", "secret-123");
        let ext_runner = ExtensionRunner::new(runner, manager);

        let ext = test_extension();
        let _ = ext_runner
            .run_post_process(&ext, &HashMap::new(), Path::new("/tmp"))
            .await
            .expect("run");

        let runner_ref = &ext_runner.runner;
        let env = runner_ref.last_env.lock().unwrap();
        let env = env.as_ref().unwrap();
        assert_eq!(env.get("NZBPO_ApiKey"), Some(&"secret-123".to_string()));
    }

    #[test]
    fn build_nzbpp_env_sets_expected_vars() {
        let params = vec![("key1".to_string(), "val1".to_string())];
        let env = build_nzbpp_env(&NzbppContext {
            nzb_name: "MyNzb",
            nzb_id: 42,
            category: "movies",
            working_dir: Path::new("/work"),
            dest_dir: Path::new("/dest"),
            final_dir: Path::new("/final"),
            total_status: "SUCCESS",
            par_status: "NONE",
            unpack_status: "SUCCESS",
            parameters: &params,
        });

        assert_eq!(env.get("NZBPP_NZBNAME"), Some(&"MyNzb".to_string()));
        assert_eq!(env.get("NZBPP_NZBID"), Some(&"42".to_string()));
        assert_eq!(env.get("NZBPP_CATEGORY"), Some(&"movies".to_string()));
        assert_eq!(env.get("NZBPR_key1"), Some(&"val1".to_string()));
    }

    #[test]
    fn interpret_post_result_maps_exit_codes() {
        let success = ExtensionResult {
            exit_code: 93,
            stdout: String::new(),
            stderr: String::new(),
            messages: vec![],
        };
        assert_eq!(interpret_post_result(&success), PostProcessResult::Success);

        let failure = ExtensionResult {
            exit_code: 94,
            stdout: String::new(),
            stderr: String::new(),
            messages: vec![],
        };
        assert_eq!(interpret_post_result(&failure), PostProcessResult::Failure);

        let none = ExtensionResult {
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
            messages: vec![],
        };
        assert_eq!(interpret_post_result(&none), PostProcessResult::None);
    }
}
