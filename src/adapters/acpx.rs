use std::{path::PathBuf, sync::Arc, time::Duration};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;
use tokio::fs;
use tracing::{debug, warn};

use crate::{
    config::AcpxCliConfig,
    domain::{
        AgentReply, BinarySource, MessageBlock, SessionHistoryEntry, SessionRecord,
        SessionSelector, SessionStatus, SessionSummary,
    },
    error::BridgeError,
    ports::{AcpxGateway, ProcessOutput, ProcessRequest, ProcessRunner},
    support::text_preview,
};

const DOCUMENT_START_TAG: &str = "<feishu2acp-document";
const DOCUMENT_END_TAG: &str = "</feishu2acp-document>";
const DEFAULT_REPLY_FILE_NAME: &str = "reply.md";

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
        args
    }

    fn with_agent(mut args: Vec<String>, selector: &SessionSelector) -> Vec<String> {
        // acpx treats --cwd/--format/--json-strict/--timeout as global options, so they must
        // appear before the agent subcommand (`codex`, `claude`, ...). Putting them after the
        // agent causes clap parsing to fail inside the agent subcommand.
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
        let command_display = format!("{} {}", request.program, request.args.join(" "));
        let diagnostic_request = self.build_non_strict_diagnostic_request(&request);
        debug!(command = %command_display, "running acpx command");
        let output = self.runner.run(request).await?;
        if output.exit_code != 0 {
            let (stdout, stderr) = if output.stdout.trim().is_empty() && output.stderr.trim().is_empty()
            {
                // `acpx --json-strict` suppresses non-JSON stderr output. When a command exits
                // with no visible output, retry once without `--json-strict` so the caller gets
                // the underlying clap/runtime error instead of a generic empty failure.
                match diagnostic_request {
                    Some(request) => {
                        debug!(
                            command = %command_display,
                            "retrying acpx command without --json-strict for diagnostics"
                        );
                        match self.runner.run(request).await {
                            Ok(diagnostic_output)
                                if !diagnostic_output.stdout.trim().is_empty()
                                    || !diagnostic_output.stderr.trim().is_empty() =>
                            {
                                (diagnostic_output.stdout, diagnostic_output.stderr)
                            }
                            _ => (output.stdout, output.stderr),
                        }
                    }
                    None => (output.stdout, output.stderr),
                }
            } else {
                (output.stdout, output.stderr)
            };

            warn!(
                command = %command_display,
                exit_code = output.exit_code,
                stdout = %text_preview(&stdout, 200),
                stderr = %text_preview(&stderr, 200),
                "acpx command failed"
            );

            return Err(BridgeError::ProcessFailed {
                command: command_display,
                exit_code: output.exit_code,
                stdout,
                stderr,
            });
        }
        debug!(
            command = %command_display,
            stdout_chars = output.stdout.chars().count(),
            stderr_chars = output.stderr.chars().count(),
            "acpx command succeeded"
        );
        Ok(output)
    }

    fn build_non_strict_diagnostic_request(&self, request: &ProcessRequest) -> Option<ProcessRequest> {
        if !request.args.iter().any(|arg| arg == "--json-strict") {
            return None;
        }

        Some(ProcessRequest {
            program: request.program.clone(),
            args: request
                .args
                .iter()
                .filter(|arg| arg.as_str() != "--json-strict")
                .cloned()
                .collect(),
            cwd: request.cwd.clone(),
            timeout: request.timeout,
        })
    }

    async fn run_json<T>(&self, args: Vec<String>, cwd: &std::path::Path) -> Result<T, BridgeError>
    where
        T: for<'de> Deserialize<'de>,
    {
        let output = self.run_checked(self.build_request(args, cwd)).await?;
        serde_json::from_str::<T>(output.stdout.trim()).map_err(|error| {
            warn!(
                context = "acpx json response",
                error = %error,
                output = %text_preview(&output.stdout, 200),
                "failed to parse acpx json output"
            );
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
    ) -> Result<AgentReply, BridgeError> {
        let output = self.run_checked(self.build_request(args, cwd)).await?;
        Ok(parse_agent_reply(&output.stdout))
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
        let mut args = Self::with_agent(Self::apply_json_flags(self.base_args(selector)), selector);
        args.extend(["sessions".to_string(), "ensure".to_string()]);
        if let Some(name) = &selector.session_name {
            args.push("--name".to_string());
            args.push(name.clone());
        }
        let response: SessionSummaryResponse = self.run_json(args, &selector.cwd).await?;
        Ok(response.into_summary())
    }

    async fn new_session(&self, selector: &SessionSelector) -> Result<SessionSummary, BridgeError> {
        let mut args = Self::with_agent(Self::apply_json_flags(self.base_args(selector)), selector);
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
        let mut args = Self::with_agent(Self::apply_json_flags(self.base_args(selector)), selector);
        args.extend(["sessions".to_string(), "close".to_string()]);
        if let Some(name) = &selector.session_name {
            args.push(name.clone());
        }
        let response: CloseSessionResponse = self.run_json(args, &selector.cwd).await?;
        Ok(self.summary_from_close(response, selector.session_name.clone()))
    }

    async fn show_session(&self, selector: &SessionSelector) -> Result<SessionRecord, BridgeError> {
        let mut args = Self::with_agent(Self::apply_json_flags(self.base_args(selector)), selector);
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
        let mut args = Self::with_agent(Self::apply_json_flags(self.base_args(&selector)), &selector);
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
        let mut args = Self::with_agent(Self::apply_json_flags(self.base_args(selector)), selector);
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
        let mut args = Self::with_agent(Self::apply_json_flags(self.base_args(selector)), selector);
        args.push("status".to_string());
        args = Self::apply_session_name(args, &selector.session_name);
        let response: StatusResponse = self.run_json(args, &selector.cwd).await?;
        Ok(response.into_status())
    }

    async fn prompt(
        &self,
        selector: &SessionSelector,
        blocks: &[MessageBlock],
    ) -> Result<AgentReply, BridgeError> {
        let mut args = Self::with_agent(Self::apply_quiet_flags(self.base_args(selector)), selector);
        args.extend(["prompt".to_string()]);
        args = Self::apply_session_name(args, &selector.session_name);
        args.push(render_prompt_blocks(blocks).await?);
        self.run_quiet(args, &selector.cwd).await
    }

    async fn exec(
        &self,
        selector: &SessionSelector,
        blocks: &[MessageBlock],
    ) -> Result<AgentReply, BridgeError> {
        let mut args = Self::with_agent(Self::apply_quiet_flags(self.base_args(selector)), selector);
        args.extend(["exec".to_string(), render_prompt_blocks(blocks).await?]);
        self.run_quiet(args, &selector.cwd).await
    }

    async fn set_mode(&self, selector: &SessionSelector, mode: &str) -> Result<(), BridgeError> {
        let mut args = Self::with_agent(Self::apply_quiet_flags(self.base_args(selector)), selector);
        args.extend(["set-mode".to_string(), mode.to_string()]);
        args = Self::apply_session_name(args, &selector.session_name);
        let _ = self
            .run_checked(self.build_request(args, &selector.cwd))
            .await?;
        Ok(())
    }

    async fn set_model(&self, selector: &SessionSelector, model: &str) -> Result<(), BridgeError> {
        let mut args = Self::with_agent(Self::apply_quiet_flags(self.base_args(selector)), selector);
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
        let mut args = Self::with_agent(Self::apply_quiet_flags(self.base_args(selector)), selector);
        args.extend(["set".to_string(), key.to_string(), value.to_string()]);
        args = Self::apply_session_name(args, &selector.session_name);
        let _ = self
            .run_checked(self.build_request(args, &selector.cwd))
            .await?;
        Ok(())
    }

    async fn cancel(&self, selector: &SessionSelector) -> Result<bool, BridgeError> {
        let mut args = Self::with_agent(Self::apply_json_flags(self.base_args(selector)), selector);
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
    title: Option<String>,
    #[serde(rename = "createdAt")]
    created_at: String,
    #[serde(rename = "lastUsedAt")]
    last_used_at: String,
    #[serde(rename = "lastPromptAt")]
    last_prompt_at: Option<String>,
    messages: Option<Vec<Value>>,
    closed: Option<bool>,
    acpx: Option<AcpxState>,
}

impl SessionRecordResponse {
    fn into_record(self) -> SessionRecord {
        let first_user_preview = self
            .messages
            .as_deref()
            .and_then(extract_first_user_preview);
        SessionRecord {
            record_id: self.acpx_record_id,
            acp_session_id: self.acp_session_id,
            agent_session_id: self.agent_session_id,
            agent: self.agent_command,
            cwd: PathBuf::from(self.cwd),
            name: self.name,
            title: self.title,
            first_user_preview,
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

fn extract_first_user_preview(messages: &[Value]) -> Option<String> {
    messages.iter().find_map(|message| {
        let user = message.get("User")?;
        let content = user.get("content")?.as_array()?;
        content.iter().find_map(|block| {
            block.get("Text").and_then(Value::as_str).and_then(|text| {
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            })
        })
    })
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

async fn render_prompt_blocks(blocks: &[MessageBlock]) -> Result<String, BridgeError> {
    let mut sections = Vec::new();

    for block in blocks {
        match block {
            MessageBlock::Text { text } => {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    sections.push(trimmed.to_string());
                }
            }
            MessageBlock::Document {
                mime_type,
                file_name,
                source,
                extracted_text,
            } => {
                if !is_markdown_mime(mime_type) {
                    return Err(BridgeError::InputRejected(format!(
                        "当前 ACPX CLI 适配器只支持 markdown 文档，暂不支持附件 `{file_name}`。"
                    )));
                }

                let text = read_document_text(source, extracted_text.as_deref()).await?;
                sections.push(format!(
                    "附加 markdown 文档 `{}`：\n<markdown-document file_name=\"{}\">\n{}\n</markdown-document>",
                    file_name, file_name, text
                ));
            }
            MessageBlock::Image { .. } => {
                return Err(BridgeError::InputRejected(
                    "当前 ACPX CLI 适配器还不支持图片块，请先升级到支持结构化媒体的 ACPX/ACP 适配器。"
                        .to_string(),
                ));
            }
        }
    }

    if sections.is_empty() {
        return Err(BridgeError::InputRejected("消息内容为空。".to_string()));
    }

    Ok(sections.join("\n\n"))
}

async fn read_document_text(
    source: &BinarySource,
    extracted_text: Option<&str>,
) -> Result<String, BridgeError> {
    if let Some(text) = extracted_text {
        return Ok(text.to_string());
    }

    let bytes = match source {
        BinarySource::Bytes(bytes) => bytes.clone(),
        BinarySource::LocalPath(path) => fs::read(path).await.map_err(|error| {
            BridgeError::Acpx(format!(
                "failed to read markdown document {}: {error}",
                path.display()
            ))
        })?,
    };

    String::from_utf8(bytes).map_err(|error| {
        BridgeError::InputRejected(format!(
            "markdown 文档不是有效的 UTF-8 文本：{error}"
        ))
    })
}

fn parse_agent_reply(raw: &str) -> AgentReply {
    let mut blocks = Vec::new();
    let mut remaining = raw;

    while let Some(start) = remaining.find(DOCUMENT_START_TAG) {
        push_text_reply_block(&mut blocks, &remaining[..start]);

        let after_start = &remaining[start..];
        let Some(tag_end) = after_start.find('>') else {
            return plain_text_reply(raw);
        };
        let start_tag = &after_start[..=tag_end];
        let Some(file_name) = parse_document_file_name(start_tag) else {
            return plain_text_reply(raw);
        };
        let after_tag = &after_start[tag_end + 1..];
        let Some(end_index) = after_tag.find(DOCUMENT_END_TAG) else {
            return plain_text_reply(raw);
        };
        let content = after_tag[..end_index].trim_matches('\n').to_string();
        if !content.is_empty() {
            blocks.push(MessageBlock::Document {
                mime_type: "text/markdown".to_string(),
                file_name,
                source: BinarySource::Bytes(content.as_bytes().to_vec()),
                extracted_text: Some(content),
            });
        }
        remaining = &after_tag[end_index + DOCUMENT_END_TAG.len()..];
    }

    push_text_reply_block(&mut blocks, remaining);
    AgentReply { blocks }
}

fn plain_text_reply(raw: &str) -> AgentReply {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        AgentReply { blocks: Vec::new() }
    } else {
        AgentReply::from_text(trimmed)
    }
}

fn push_text_reply_block(blocks: &mut Vec<MessageBlock>, fragment: &str) {
    let trimmed = fragment.trim();
    if !trimmed.is_empty() {
        blocks.push(MessageBlock::text(trimmed));
    }
}

fn parse_document_file_name(start_tag: &str) -> Option<String> {
    let attribute = r#"file_name=""#;
    let start = start_tag.find(attribute)?;
    let value_start = start + attribute.len();
    let value_end = start_tag[value_start..].find('"')?;
    Some(sanitize_reply_file_name(
        &start_tag[value_start..value_start + value_end],
    ))
}

fn sanitize_reply_file_name(value: &str) -> String {
    let sanitized = value
        .trim()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    let candidate = if sanitized.is_empty() {
        DEFAULT_REPLY_FILE_NAME.to_string()
    } else {
        sanitized
    };
    if candidate.ends_with(".md") || candidate.ends_with(".markdown") {
        candidate
    } else {
        format!("{candidate}.md")
    }
}

fn is_markdown_mime(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "text/markdown" | "text/x-markdown"
    )
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, sync::Arc, time::Duration};

    use async_trait::async_trait;
    use tokio::sync::Mutex;

    use crate::{
        adapters::acpx::AcpxCliGateway,
        config::AcpxCliConfig,
        domain::{MessageBlock, PermissionMode, SessionSelector},
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

    fn arg_index(args: &[String], value: &str) -> usize {
        args.iter().position(|arg| arg == value).unwrap()
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
        assert!(arg_index(&request.args, "--format") < arg_index(&request.args, "codex"));
        assert!(arg_index(&request.args, "codex") < arg_index(&request.args, "sessions"));
    }

    #[tokio::test]
    async fn prompt_uses_quiet_output() {
        let runner = Arc::new(StubRunner::new(vec![ProcessOutput {
            exit_code: 0,
            stdout: "hello".to_string(),
            stderr: String::new(),
        }]));
        let gateway = gateway(runner.clone());
        let response = gateway
            .prompt(&selector(), &[MessageBlock::text("fix the tests")])
            .await
            .unwrap();
        assert_eq!(
            response.blocks,
            vec![MessageBlock::text("hello".to_string())]
        );
        let request = &runner.requests.lock().await[0];
        assert!(request.args.contains(&"quiet".to_string()));
        assert!(request.args.contains(&"prompt".to_string()));
        assert!(request.args.contains(&"fix the tests".to_string()));
        assert!(arg_index(&request.args, "--format") < arg_index(&request.args, "codex"));
        assert!(arg_index(&request.args, "codex") < arg_index(&request.args, "prompt"));
    }

    #[tokio::test]
    async fn prompt_renders_markdown_document_blocks() {
        let runner = Arc::new(StubRunner::new(vec![ProcessOutput {
            exit_code: 0,
            stdout: "done".to_string(),
            stderr: String::new(),
        }]));
        let gateway = gateway(runner.clone());
        let _ = gateway
            .prompt(
                &selector(),
                &[MessageBlock::Document {
                    mime_type: "text/markdown".to_string(),
                    file_name: "README.md".to_string(),
                    source: crate::domain::BinarySource::Bytes(b"# Title".to_vec()),
                    extracted_text: Some("# Title".to_string()),
                }],
            )
            .await
            .unwrap();

        let request = &runner.requests.lock().await[0];
        let prompt = request.args.last().unwrap();
        assert!(prompt.contains("附加 markdown 文档 `README.md`"));
        assert!(prompt.contains("<markdown-document file_name=\"README.md\">"));
        assert!(prompt.contains("# Title"));
    }

    #[tokio::test]
    async fn prompt_parses_embedded_markdown_reply_documents() {
        let runner = Arc::new(StubRunner::new(vec![ProcessOutput {
            exit_code: 0,
            stdout: concat!(
                "说明文字\n\n",
                "<feishu2acp-document file_name=\"plan.md\">\n",
                "# Plan\n",
                "</feishu2acp-document>\n"
            )
            .to_string(),
            stderr: String::new(),
        }]));
        let gateway = gateway(runner);
        let response = gateway
            .prompt(&selector(), &[MessageBlock::text("generate a plan")])
            .await
            .unwrap();

        assert_eq!(
            response.blocks,
            vec![
                MessageBlock::text("说明文字".to_string()),
                MessageBlock::Document {
                    mime_type: "text/markdown".to_string(),
                    file_name: "plan.md".to_string(),
                    source: crate::domain::BinarySource::Bytes(b"# Plan".to_vec()),
                    extracted_text: Some("# Plan".to_string()),
                }
            ]
        );
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
    async fn list_sessions_extracts_title_and_first_user_preview() {
        let runner = Arc::new(StubRunner::new(vec![ProcessOutput {
            exit_code: 0,
            stdout: r#"[{
                "acpxRecordId":"rec-1",
                "acpSessionId":"acp-1",
                "agentSessionId":"agent-1",
                "agentCommand":"codex",
                "cwd":"/repo",
                "name":null,
                "title":null,
                "createdAt":"2026-01-01T00:00:00Z",
                "lastUsedAt":"2026-01-01T00:01:00Z",
                "lastPromptAt":"2026-01-01T00:01:00Z",
                "closed":false,
                "messages":[
                    {"User":{"id":"u1","content":[{"Text":" first prompt with  spaces \n and newline "}] }},
                    {"Agent":{"content":[{"Text":"answer"}]}}
                ],
                "acpx":{"current_model_id":"gpt-5.4","current_mode_id":"auto"}
            }]"#
            .to_string(),
            stderr: String::new(),
        }]));
        let gateway = gateway(runner);
        let records = gateway.list_sessions("codex").await.unwrap();

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].title, None);
        assert_eq!(
            records[0].first_user_preview.as_deref(),
            Some("first prompt with  spaces \n and newline")
        );
        assert_eq!(records[0].model.as_deref(), Some("gpt-5.4"));
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

    #[tokio::test]
    async fn strict_json_failure_retries_once_without_strict_for_diagnostics() {
        let runner = Arc::new(StubRunner::new(vec![
            ProcessOutput {
                exit_code: 1,
                stdout: String::new(),
                stderr: String::new(),
            },
            ProcessOutput {
                exit_code: 1,
                stdout: String::new(),
                stderr: "error: unknown option '--format'".to_string(),
            },
        ]));
        let gateway = gateway(runner.clone());
        let error = gateway.cancel(&selector()).await.unwrap_err();

        match error {
            BridgeError::ProcessFailed { stderr, .. } => {
                assert!(stderr.contains("unknown option '--format'"));
            }
            other => panic!("unexpected error: {other:?}"),
        }

        let requests = runner.requests.lock().await;
        assert_eq!(requests.len(), 2);
        assert!(requests[0].args.contains(&"--json-strict".to_string()));
        assert!(!requests[1].args.contains(&"--json-strict".to_string()));
    }
}
