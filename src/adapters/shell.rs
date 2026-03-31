use std::{path::Path, sync::Arc, time::Duration};

use async_trait::async_trait;

use crate::{
    config::ShellConfig,
    domain::ShellOutput,
    error::BridgeError,
    ports::{ProcessRequest, ProcessRunner, ShellExecutor},
};

pub struct SystemShellExecutor {
    runner: Arc<dyn ProcessRunner>,
    config: ShellConfig,
}

impl SystemShellExecutor {
    pub fn new(runner: Arc<dyn ProcessRunner>, config: ShellConfig) -> Self {
        Self { runner, config }
    }
}

#[async_trait]
impl ShellExecutor for SystemShellExecutor {
    async fn execute(&self, cwd: &Path, command: &str) -> Result<ShellOutput, BridgeError> {
        let mut args = self.config.args.clone();
        args.push(command.to_string());

        let output = self
            .runner
            .run(ProcessRequest {
                program: self.config.program.clone(),
                args,
                cwd: Some(cwd.to_path_buf()),
                timeout: Duration::from_secs(self.config.timeout_secs),
            })
            .await?;

        Ok(ShellOutput {
            command: command.to_string(),
            cwd: cwd.to_path_buf(),
            exit_code: output.exit_code,
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, sync::Arc, time::Duration};

    use async_trait::async_trait;

    use crate::{
        adapters::shell::SystemShellExecutor,
        config::ShellConfig,
        error::BridgeError,
        ports::{ProcessOutput, ProcessRequest, ProcessRunner, ShellExecutor},
    };

    struct StubRunner;

    #[async_trait]
    impl ProcessRunner for StubRunner {
        async fn run(&self, request: ProcessRequest) -> Result<ProcessOutput, BridgeError> {
            assert_eq!(request.program, "powershell");
            assert_eq!(request.args, vec!["-NoProfile", "-Command", "pwd"]);
            assert_eq!(request.cwd, Some(PathBuf::from("/repo")));
            assert_eq!(request.timeout, Duration::from_secs(12));
            Ok(ProcessOutput {
                exit_code: 0,
                stdout: "/repo".to_string(),
                stderr: String::new(),
            })
        }
    }

    #[tokio::test]
    async fn shell_executor_delegates_to_process_runner() {
        let executor = SystemShellExecutor::new(
            Arc::new(StubRunner),
            ShellConfig {
                program: "powershell".to_string(),
                args: vec!["-NoProfile".to_string(), "-Command".to_string()],
                timeout_secs: 12,
            },
        );

        let output = executor
            .execute(std::path::Path::new("/repo"), "pwd")
            .await
            .unwrap();
        assert_eq!(output.stdout, "/repo");
    }
}
