use async_trait::async_trait;
use tokio::{process::Command, time::timeout};

use crate::{
    error::BridgeError,
    ports::{ProcessOutput, ProcessRequest, ProcessRunner},
};

pub struct SystemProcessRunner;

#[async_trait]
impl ProcessRunner for SystemProcessRunner {
    async fn run(&self, request: ProcessRequest) -> Result<ProcessOutput, BridgeError> {
        let mut command = Command::new(&request.program);
        command.args(&request.args);
        if let Some(cwd) = &request.cwd {
            command.current_dir(cwd);
        }
        command.kill_on_drop(true);

        let display = format!("{} {}", request.program, request.args.join(" "));
        let output = timeout(request.timeout, command.output())
            .await
            .map_err(|_| BridgeError::ProcessTimedOut {
                command: display.clone(),
            })?
            .map_err(|error| BridgeError::Shell(format!("failed to spawn `{display}`: {error}")))?;

        Ok(ProcessOutput {
            exit_code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    }
}
