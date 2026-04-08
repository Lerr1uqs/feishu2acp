use async_trait::async_trait;
use std::time::Instant;
use tokio::{process::Command, time::timeout};
use tracing::{debug, warn};

use crate::{
    error::BridgeError,
    ports::{ProcessOutput, ProcessRequest, ProcessRunner},
};

pub struct SystemProcessRunner;

#[async_trait]
impl ProcessRunner for SystemProcessRunner {
    async fn run(&self, request: ProcessRequest) -> Result<ProcessOutput, BridgeError> {
        let started_at = Instant::now();
        let mut command = Command::new(&request.program);
        command.args(&request.args);
        if let Some(cwd) = &request.cwd {
            command.current_dir(cwd);
        }
        command.kill_on_drop(true);

        let command_display = format!("{} {}", request.program, request.args.join(" "));
        debug!(
            command = %command_display,
            cwd = request.cwd.as_ref().map(|path| path.display().to_string()).unwrap_or_else(|| "-".to_string()),
            timeout_secs = request.timeout.as_secs(),
            "spawning external process"
        );
        let output = timeout(request.timeout, command.output())
            .await
            .map_err(|_| BridgeError::ProcessTimedOut {
                command: command_display.clone(),
            })?
            .map_err(|error| BridgeError::Shell(format!("failed to spawn `{command_display}`: {error}")))?;

        if output.status.code().unwrap_or(-1) != 0 {
            warn!(
                command = %command_display,
                exit_code = output.status.code().unwrap_or(-1),
                elapsed_ms = started_at.elapsed().as_millis() as u64,
                stdout_bytes = output.stdout.len(),
                stderr_bytes = output.stderr.len(),
                "external process exited with non-zero status"
            );
        } else {
            debug!(
                command = %command_display,
                exit_code = output.status.code().unwrap_or(-1),
                elapsed_ms = started_at.elapsed().as_millis() as u64,
                stdout_bytes = output.stdout.len(),
                stderr_bytes = output.stderr.len(),
                "external process completed"
            );
        }

        Ok(ProcessOutput {
            exit_code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    }
}
