use std::{path::PathBuf, sync::Arc, time::Duration};

use async_trait::async_trait;
use serde::Deserialize;

use crate::{
    config::AcpxCliConfig,
    domain::{
        PromptResponse, SessionHistoryEntry, SessionRecord, SessionSelector, SessionStatus,
        SessionSummary,
    },
    error::BridgeError,
    ports::{AcpxGateway, ProcessOutput, ProcessRequest, ProcessRunner},
};

pub struct AcpxCliGateway {
    runner: Arc<dyn ProcessRunner>,
    config: AcpxCliConfig,
}

impl AcpxCliGateway {
    pub fn new(runner: Arc<dyn ProcessRunner>, config: AcpxCliConfig) -> Self {
        Self { runner, config }
    }

    fn base_args(&self, selector: &SessionSelector) -> Vec<String> {
        let mut args = self.config.args.clone();
        args.push(selector.permission_mode.as_acpx_flag().to_string());
        args.push("--cwd".to_string());
        args.push(selector.cwd.display().to_string());
        args.push("--timeout".to_string());
        args.push(self.config.timeout_secs.to_string());
        args.push("--ttl".to_string());
        args.push(self.config.ttl_secs.to_string());
        args.push(selector.agent.clone());
        args
    }

    fn build_request(&self, args: Vec<String>, cwd: &std::path::Path) -> ProcessRequest {
        ProcessRequest {
            program: self.config.program.clone(),
            args,
            cwd: Some(cwd.to_path_buf()),
            timeout: Duration::from_secs(self.config.timeout_secs),
        }
    }

    async fn run_checked(&self, request: ProcessRequest) -> Result<ProcessOutput, BridgeError> {
        let display = format!("{} {}", request.program, request.args.join(" "));
        let output = self.runner.run(request).await?;
        if output.exit_code != 0 {
            return Err(BridgeError::ProcessFailed {
                command: display,
                exit_code: output.exit_code,
                stdout: output.stdout,
                stderr: output.stderr,
            });
        }
        Ok(output)
    }

    async fn run_json<T>(&self, args: Vec<String>, cwd: &std::path::Path) -> Result<T, BridgeError>
    where
        T: for<'de> Deserialize<'de>,
    {
        let output = self.run_checked(self.build_request(args, cwd)).await?;
        serde_json::from_str::<T>(output.stdout.trim()).map_err(|error| {
            BridgeError::InvalidOutput {
                context: "acpx json response".to_string(),
                message: error.to_string(),
                raw_output: output.stdout,
            }
        })
    }

    async fn run_quiet(
        &self,
        args: Vec<String>,
        cwd: &std::path::Path,
    ) -> Result<PromptResponse, BridgeError> {
        let output = self.run_checked(self.build_request(args, cwd)).await?;
        Ok(PromptResponse {
            text: output.stdout,
        })
    }

    fn apply_json_flags(mut args: Vec<String>) -> Vec<String> {
        args.push("--format".to_string());
        args.push("json".to_string());
        args.push("--json-strict".to_string());
        args
    }

    fn apply_quiet_flags(mut args: Vec<String>) -> Vec<String> {
        args.push("--format".to_string());
        args.push("quiet".to_string());
        args
    }

    fn apply_session_name(mut args: Vec<String>, session_name: &Option<String>) -> Vec<String> {
        if let Some(session_name) = session_name {
            args.push("-s".to_string());
            args.push(session_name.clone());
        }
        args
    }

    fn summary_from_close(
        &self,
        response: CloseSessionResponse,
        session_name: Option<String>,
    ) -> SessionSummary {
        SessionSummary {
            record_id: response.acpx_record_id,
            name: session_name,
            created: false,
            acp_session_id: response.acpx_session_id,
            agent_session_id: response.agent_session_id,
        }
    }
}

#[async_trait]
impl AcpxGateway for AcpxCliGateway {
    async fn ensure_session(
        &self,
        selector: &SessionSelector,
    ) -> Result<SessionSummary, BridgeError> {
        let mut args = Self::apply_json_flags(self.base_args(selector));
        args.extend(["sessions".to_string(), "ensure".to_string()]);
        if let Some(name) = &selector.session_name {
            args.push("--name".to_string());
            args.push(name.clone());
        }
        let response: SessionSummaryResponse = self.run_json(args, &selector.cwd).await?;
        Ok(response.into_summary())
    }

    async fn new_session(&self, selector: &SessionSelector) -> Result<SessionSummary, BridgeError> {
        let mut args = Self::apply_json_flags(self.base_args(selector));
        args.extend(["sessions".to_string(), "new".to_string()]);
        if let Some(name) = &selector.session_name {
            args.push("--name".to_string());
            args.push(name.clone());
        }
        let response: SessionSummaryResponse = self.run_json(args, &selector.cwd).await?;
        Ok(response.into_summary())
    }

    async fn close_session(
        &self,
        selector: &SessionSelector,
    ) -> Result<SessionSummary, BridgeError> {
        let mut args = Self::apply_json_flags(self.base_args(selector));
        args.extend(["sessions".to_string(), "close".to_string()]);
        if let Some(name) = &selector.session_name {
            args.push(name.clone());
        }
        let response: CloseSessionResponse = self.run_json(args, &selector.cwd).await?;
        Ok(self.summary_from_close(response, selector.session_name.clone()))
    }

    async fn show_session(&self, selector: &SessionSelector) -> Result<SessionRecord, BridgeError> {
        let mut args = Self::apply_json_flags(self.base_args(selector));
        args.extend(["sessions".to_string(), "show".to_string()]);
        if let Some(name) = &selector.session_name {
            args.push(name.clone());
        }
        let response: SessionRecordResponse = self.run_json(args, &selector.cwd).await?;
        Ok(response.into_record())
    }

    async fn list_sessions(&self, agent: &str) -> Result<Vec<SessionRecord>, BridgeError> {
        let cwd = PathBuf::from(".");
        let selector = SessionSelector {
            cwd: cwd.clone(),
            agent: agent.to_string(),
            session_name: None,
            permission_mode: crate::domain::PermissionMode::ApproveReads,
        };
        let mut args = Self::apply_json_flags(self.base_args(&selector));
        args.extend(["sessions".to_string(), "list".to_string()]);
        let responses: Vec<SessionRecordResponse> = self.run_json(args, &cwd).await?;
        Ok(responses
            .into_iter()
            .map(SessionRecordResponse::into_record)
            .collect())
    }

    async fn history(
        &self,
        selector: &SessionSelector,
        limit: usize,
    ) -> Result<Vec<SessionHistoryEntry>, BridgeError> {
        let mut args = Self::apply_json_flags(self.base_args(selector));
        args.extend(["sessions".to_string(), "history".to_string()]);
        if let Some(name) = &selector.session_name {
            args.push(name.clone());
        }
        args.push("--limit".to_string());
        args.push(limit.to_string());
        let response: SessionHistoryResponse = self.run_json(args, &selector.cwd).await?;
        Ok(response
            .entries
            .into_iter()
            .map(|entry| SessionHistoryEntry {
                role: entry.role,
                timestamp: entry.timestamp,
                text_preview: entry.text_preview,
            })
            .collect())
    }

    async fn status(&self, selector: &SessionSelector) -> Result<SessionStatus, BridgeError> {
        let mut args = Self::apply_json_flags(self.base_args(selector));
        args.push("status".to_string());
        args = Self::apply_session_name(args, &selector.session_name);
        let response: StatusResponse = self.run_json(args, &selector.cwd).await?;
        Ok(response.into_status())
    }

    async fn prompt(
        &self,
        selector: &SessionSelector,
        prompt: &str,
    ) -> Result<PromptResponse, BridgeError> {
        let mut args = Self::apply_quiet_flags(self.base_args(selector));
        args.extend(["prompt".to_string()]);
        args = Self::apply_session_name(args, &selector.session_name);
        args.push(prompt.to_string());
        self.run_quiet(args, &selector.cwd).await
    }

    async fn exec(
        &self,
        selector: &SessionSelector,
        prompt: &str,
    ) -> Result<PromptResponse, BridgeError> {
        let mut args = Self::apply_quiet_flags(self.base_args(selector));
        args.extend(["exec".to_string(), prompt.to_string()]);
        self.run_quiet(args, &selector.cwd).await
    }

    async fn set_mode(&self, selector: &SessionSelector, mode: &str) -> Result<(), BridgeError> {
        let mut args = Self::apply_quiet_flags(self.base_args(selector));
        args.extend(["set-mode".to_string(), mode.to_string()]);
        args = Self::apply_session_name(args, &selector.session_name);
        let _ = self
            .run_checked(self.build_request(args, &selector.cwd))
            .await?;
        Ok(())
    }

    async fn set_model(&self, selector: &SessionSelector, model: &str) -> Result<(), BridgeError> {
        let mut args = Self::apply_quiet_flags(self.base_args(selector));
        args.extend(["set".to_string(), "model".to_string(), model.to_string()]);
        args = Self::apply_session_name(args, &selector.session_name);
        let _ = self
            .run_checked(self.build_request(args, &selector.cwd))
            .await?;
        Ok(())
    }

    async fn set_option(
        &self,
        selector: &SessionSelector,
        key: &str,
        value: &str,
    ) -> Result<(), BridgeError> {
        let mut args = Self::apply_quiet_flags(self.base_args(selector));
        args.extend(["set".to_string(), key.to_string(), value.to_string()]);
        args = Self::apply_session_name(args, &selector.session_name);
        let _ = self
            .run_checked(self.build_request(args, &selector.cwd))
            .await?;
        Ok(())
    }

    async fn cancel(&self, selector: &SessionSelector) -> Result<bool, BridgeError> {
        let mut args = Self::apply_json_flags(self.base_args(selector));
        args.push("cancel".to_string());
        args = Self::apply_session_name(args, &selector.session_name);
        let response: CancelResponse = self.run_json(args, &selector.cwd).await?;
        Ok(response.cancelled)
    }
}

#[derive(Debug, Deserialize)]
struct SessionSummaryResponse {
    #[serde(rename = "acpxRecordId")]
    acpx_record_id: String,
    name: Option<String>,
    created: bool,
    #[serde(rename = "acpxSessionId")]
    acpx_session_id: Option<String>,
    #[serde(rename = "agentSessionId")]
    agent_session_id: Option<String>,
}

impl SessionSummaryResponse {
    fn into_summary(self) -> SessionSummary {
        SessionSummary {
            record_id: self.acpx_record_id,
            name: self.name,
            created: self.created,
            acp_session_id: self.acpx_session_id,
            agent_session_id: self.agent_session_id,
        }
    }
}

#[derive(Debug, Deserialize)]
struct CloseSessionResponse {
    #[serde(rename = "acpxRecordId")]
    acpx_record_id: String,
    #[serde(rename = "acpxSessionId")]
    acpx_session_id: Option<String>,
    #[serde(rename = "agentSessionId")]
    agent_session_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SessionRecordResponse {
    #[serde(rename = "acpxRecordId")]
    acpx_record_id: String,
    #[serde(rename = "acpSessionId")]
    acp_session_id: String,
    #[serde(rename = "agentSessionId")]
    agent_session_id: Option<String>,
    #[serde(rename = "agentCommand")]
    agent_command: String,
    cwd: String,
    name: Option<String>,
    #[serde(rename = "createdAt")]
    created_at: String,
    #[serde(rename = "lastUsedAt")]
    last_used_at: String,
    #[serde(rename = "lastPromptAt")]
    last_prompt_at: Option<String>,
    closed: Option<bool>,
    acpx: Option<AcpxState>,
}

impl SessionRecordResponse {
    fn into_record(self) -> SessionRecord {
        SessionRecord {
            record_id: self.acpx_record_id,
            acp_session_id: self.acp_session_id,
            agent_session_id: self.agent_session_id,
            agent: self.agent_command,
            cwd: PathBuf::from(self.cwd),
            name: self.name,
            created_at: self.created_at,
            last_used_at: self.last_used_at,
            last_prompt_at: self.last_prompt_at,
            closed: self.closed.unwrap_or(false),
            model: self
                .acpx
                .as_ref()
                .and_then(|state| state.current_model_id.clone()),
            mode: self
                .acpx
                .as_ref()
                .and_then(|state| state.current_mode_id.clone()),
        }
    }
}

#[derive(Debug, Deserialize)]
struct AcpxState {
    #[serde(rename = "current_model_id")]
    current_model_id: Option<String>,
    #[serde(rename = "current_mode_id")]
    current_mode_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SessionHistoryResponse {
    entries: Vec<HistoryEntryResponse>,
}

#[derive(Debug, Deserialize)]
struct HistoryEntryResponse {
    role: String,
    timestamp: String,
    #[serde(rename = "textPreview")]
    text_preview: String,
}

#[derive(Debug, Deserialize)]
struct StatusResponse {
    status: String,
    #[serde(rename = "acpxRecordId")]
    acpx_record_id: Option<String>,
    #[serde(rename = "agentCommand")]
    agent_command: Option<String>,
    pid: Option<i64>,
    model: Option<String>,
    mode: Option<String>,
    uptime: Option<String>,
    #[serde(rename = "lastPromptTime")]
    last_prompt_time: Option<String>,
    #[serde(rename = "exitCode")]
    exit_code: Option<i64>,
    signal: Option<String>,
    #[serde(rename = "agentSessionId")]
    agent_session_id: Option<String>,
}

impl StatusResponse {
    fn into_status(self) -> SessionStatus {
        SessionStatus {
            status: self.status,
            session_id: self.acpx_record_id,
            agent_command: self.agent_command.unwrap_or_else(|| "unknown".to_string()),
            pid: self.pid,
            model: self.model,
            mode: self.mode,
            uptime: self.uptime,
            last_prompt_time: self.last_prompt_time,
            exit_code: self.exit_code,
            signal: self.signal,
            agent_session_id: self.agent_session_id,
        }
    }
}

#[derive(Debug, Deserialize)]
struct CancelResponse {
    cancelled: bool,
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, sync::Arc, time::Duration};

    use async_trait::async_trait;
    use tokio::sync::Mutex;

    use crate::{
        adapters::acpx::AcpxCliGateway,
        config::AcpxCliConfig,
        domain::{PermissionMode, SessionSelector},
        error::BridgeError,
        ports::{AcpxGateway, ProcessOutput, ProcessRequest, ProcessRunner},
    };

    struct StubRunner {
        outputs: Mutex<VecDeque<ProcessOutput>>,
        requests: Mutex<Vec<ProcessRequest>>,
    }

    impl StubRunner {
        fn new(outputs: Vec<ProcessOutput>) -> Self {
            Self {
                outputs: Mutex::new(outputs.into()),
                requests: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl ProcessRunner for StubRunner {
        async fn run(&self, request: ProcessRequest) -> Result<ProcessOutput, BridgeError> {
            self.requests.lock().await.push(request);
            Ok(self.outputs.lock().await.pop_front().unwrap())
        }
    }

    fn selector() -> SessionSelector {
        SessionSelector {
            cwd: std::path::PathBuf::from("/repo"),
            agent: "codex".to_string(),
            session_name: Some("backend".to_string()),
            permission_mode: PermissionMode::ApproveReads,
        }
    }

    fn gateway(runner: Arc<dyn ProcessRunner>) -> AcpxCliGateway {
        AcpxCliGateway::new(
            runner,
            AcpxCliConfig {
                program: "acpx".to_string(),
                args: vec![],
                timeout_secs: 30,
                ttl_secs: 5,
            },
        )
    }

    #[tokio::test]
    async fn ensure_session_builds_expected_command_and_parses_json() {
        let runner = Arc::new(StubRunner::new(vec![ProcessOutput {
            exit_code: 0,
            stdout: r#"{"acpxRecordId":"rec-1","name":"backend","created":true,"acpxSessionId":"acp-1"}"#.to_string(),
            stderr: String::new(),
        }]));
        let gateway = gateway(runner.clone());
        let summary = gateway.ensure_session(&selector()).await.unwrap();
        assert_eq!(summary.record_id, "rec-1");

        let request = &runner.requests.lock().await[0];
        assert_eq!(request.program, "acpx");
        assert!(request.args.contains(&"--format".to_string()));
        assert!(request.args.contains(&"sessions".to_string()));
        assert!(request.args.contains(&"ensure".to_string()));
        assert!(request.args.contains(&"--name".to_string()));
        assert_eq!(request.timeout, Duration::from_secs(30));
    }

    #[tokio::test]
    async fn prompt_uses_quiet_output() {
        let runner = Arc::new(StubRunner::new(vec![ProcessOutput {
            exit_code: 0,
            stdout: "hello".to_string(),
            stderr: String::new(),
        }]));
        let gateway = gateway(runner.clone());
        let response = gateway.prompt(&selector(), "fix the tests").await.unwrap();
        assert_eq!(response.text, "hello");
        let request = &runner.requests.lock().await[0];
        assert!(request.args.contains(&"quiet".to_string()));
        assert!(request.args.contains(&"prompt".to_string()));
        assert!(request.args.contains(&"fix the tests".to_string()));
    }

    #[tokio::test]
    async fn parses_status_and_history() {
        let runner = Arc::new(StubRunner::new(vec![
            ProcessOutput {
                exit_code: 0,
                stdout: r#"{"status":"running","acpxRecordId":"rec-1","agentCommand":"codex","pid":123}"#.to_string(),
                stderr: String::new(),
            },
            ProcessOutput {
                exit_code: 0,
                stdout: r#"{"entries":[{"role":"assistant","timestamp":"2026","textPreview":"done"}]}"#.to_string(),
                stderr: String::new(),
            },
        ]));
        let gateway = gateway(runner);
        let status = gateway.status(&selector()).await.unwrap();
        let history = gateway.history(&selector(), 10).await.unwrap();
        assert_eq!(status.status, "running");
        assert_eq!(history[0].text_preview, "done");
    }

    #[tokio::test]
    async fn non_zero_exit_becomes_process_failed() {
        let runner = Arc::new(StubRunner::new(vec![ProcessOutput {
            exit_code: 5,
            stdout: String::new(),
            stderr: "boom".to_string(),
        }]));
        let gateway = gateway(runner);
        let error = gateway.cancel(&selector()).await.unwrap_err();
        assert!(matches!(error, BridgeError::ProcessFailed { .. }));
    }
}
