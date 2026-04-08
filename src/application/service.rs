use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

use async_trait::async_trait;
use tokio::sync::{Mutex, OwnedMutexGuard};
use tracing::{debug, info, warn};

use crate::{
    application::{
        command::{BotCommand, SessionCommand, UserRequest, parse_user_request},
        render::{
            binding_text, help_text, history_text, session_list_text, session_record_text,
            session_summary_text, shell_output_text, status_text,
        },
    },
    domain::{
        AgentReply, ConversationBinding, ConversationKey, InboundMessage, MessageBlock,
        PermissionMode, SessionSelector,
    },
    error::BridgeError,
    ports::{AcpxGateway, ChannelClient, ConversationRepository, MessageHandler, ShellExecutor},
    support::{
        chunk_text, finalize_chunk_labels, normalize_session_name, resolve_workspace, text_preview,
    },
};

#[derive(Clone, Debug)]
pub struct ServiceDefaults {
    pub command_prefix: String,
    pub default_workspace: PathBuf,
    pub default_agent: String,
    pub default_permission_mode: PermissionMode,
    pub reply_chunk_chars: usize,
}

#[derive(Default)]
struct InteractionMetrics {
    interactions_total: AtomicU64,
    successful_interactions_total: AtomicU64,
    process_errors_total: AtomicU64,
    reply_errors_total: AtomicU64,
    typing_reaction_errors_total: AtomicU64,
    typing_reaction_ms_total: AtomicU64,
    processing_ms_total: AtomicU64,
    reply_ms_total: AtomicU64,
    total_ms_total: AtomicU64,
    max_total_ms: AtomicU64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct InteractionMetricsSnapshot {
    interactions_total: u64,
    successful_interactions_total: u64,
    process_errors_total: u64,
    reply_errors_total: u64,
    typing_reaction_errors_total: u64,
    typing_reaction_ms_total: u64,
    processing_ms_total: u64,
    reply_ms_total: u64,
    total_ms_total: u64,
    max_total_ms: u64,
}

struct InteractionSample {
    typing_reaction_ok: bool,
    process_ok: bool,
    reply_ok: bool,
    typing_reaction_ms: u64,
    processing_ms: u64,
    reply_ms: u64,
    total_ms: u64,
}

impl InteractionMetrics {
    fn record(&self, sample: InteractionSample) -> InteractionMetricsSnapshot {
        self.interactions_total.fetch_add(1, Ordering::Relaxed);
        if sample.process_ok && sample.reply_ok {
            self.successful_interactions_total
                .fetch_add(1, Ordering::Relaxed);
        }
        if !sample.process_ok {
            self.process_errors_total.fetch_add(1, Ordering::Relaxed);
        }
        if !sample.reply_ok {
            self.reply_errors_total.fetch_add(1, Ordering::Relaxed);
        }
        if !sample.typing_reaction_ok {
            self.typing_reaction_errors_total
                .fetch_add(1, Ordering::Relaxed);
        }

        self.typing_reaction_ms_total
            .fetch_add(sample.typing_reaction_ms, Ordering::Relaxed);
        self.processing_ms_total
            .fetch_add(sample.processing_ms, Ordering::Relaxed);
        self.reply_ms_total
            .fetch_add(sample.reply_ms, Ordering::Relaxed);
        self.total_ms_total
            .fetch_add(sample.total_ms, Ordering::Relaxed);
        self.max_total_ms
            .fetch_max(sample.total_ms, Ordering::Relaxed);

        self.snapshot()
    }

    fn snapshot(&self) -> InteractionMetricsSnapshot {
        InteractionMetricsSnapshot {
            interactions_total: self.interactions_total.load(Ordering::Relaxed),
            successful_interactions_total: self
                .successful_interactions_total
                .load(Ordering::Relaxed),
            process_errors_total: self.process_errors_total.load(Ordering::Relaxed),
            reply_errors_total: self.reply_errors_total.load(Ordering::Relaxed),
            typing_reaction_errors_total: self
                .typing_reaction_errors_total
                .load(Ordering::Relaxed),
            typing_reaction_ms_total: self
                .typing_reaction_ms_total
                .load(Ordering::Relaxed),
            processing_ms_total: self.processing_ms_total.load(Ordering::Relaxed),
            reply_ms_total: self.reply_ms_total.load(Ordering::Relaxed),
            total_ms_total: self.total_ms_total.load(Ordering::Relaxed),
            max_total_ms: self.max_total_ms.load(Ordering::Relaxed),
        }
    }
}

pub struct BridgeService {
    channel: Arc<dyn ChannelClient>,
    acpx: Arc<dyn AcpxGateway>,
    shell: Arc<dyn ShellExecutor>,
    repository: Arc<dyn ConversationRepository>,
    defaults: ServiceDefaults,
    metrics: InteractionMetrics,
    conversation_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl BridgeService {
    pub fn new(
        channel: Arc<dyn ChannelClient>,
        acpx: Arc<dyn AcpxGateway>,
        shell: Arc<dyn ShellExecutor>,
        repository: Arc<dyn ConversationRepository>,
        defaults: ServiceDefaults,
    ) -> Self {
        Self {
            channel,
            acpx,
            shell,
            repository,
            defaults,
            metrics: InteractionMetrics::default(),
            conversation_locks: Mutex::new(HashMap::new()),
        }
    }

    async fn process_message(&self, message: &InboundMessage) -> Result<AgentReply, BridgeError> {
        let mut binding = self.load_binding(&message.conversation).await?;
        let request = message
            .blocks
            .iter()
            .find_map(MessageBlock::as_text)
            .map(|text| parse_user_request(&self.defaults.command_prefix, text))
            .transpose()?;
        let conversation_key = message.conversation.storage_key();
        debug!(
            conversation = %conversation_key,
            request_kind = request_kind(request.as_ref()),
            agent = %binding.agent,
            session = binding.session_name.as_deref().unwrap_or("default"),
            cwd = %binding.cwd.display(),
            blocks = %blocks_preview(&message.blocks, 120),
            "processing inbound message"
        );
        let response = match request {
            Some(UserRequest::Command(command)) => self.handle_command(&mut binding, command).await?,
            _ => self.handle_prompt(&binding, &message.blocks).await?,
        };
        self.repository.put(&message.conversation, &binding).await?;
        debug!(
            conversation = %conversation_key,
            reply_blocks = response.blocks.len(),
            "persisted updated conversation binding"
        );
        Ok(response)
    }

    async fn handle_prompt(
        &self,
        binding: &ConversationBinding,
        blocks: &[MessageBlock],
    ) -> Result<AgentReply, BridgeError> {
        let selector = SessionSelector::from(binding);
        debug!(
            agent = %selector.agent,
            session = selector.session_name.as_deref().unwrap_or("default"),
            cwd = %selector.cwd.display(),
            blocks = %blocks_preview(blocks, 160),
            "sending prompt to acpx"
        );
        let _ = self.acpx.ensure_session(&selector).await?;
        let response = self.acpx.prompt(&selector, blocks).await?;
        debug!(
            agent = %selector.agent,
            session = selector.session_name.as_deref().unwrap_or("default"),
            reply_blocks = response.blocks.len(),
            "received prompt response from acpx"
        );
        Ok(response)
    }

    async fn handle_command(
        &self,
        binding: &mut ConversationBinding,
        command: BotCommand,
    ) -> Result<AgentReply, BridgeError> {
        debug!(
            command = %command_name(&command),
            agent = %binding.agent,
            session = binding.session_name.as_deref().unwrap_or("default"),
            cwd = %binding.cwd.display(),
            "handling bot command"
        );
        match command {
            BotCommand::Help => Ok(AgentReply::from_text(help_text(&self.defaults.command_prefix))),
            BotCommand::Pwd => Ok(AgentReply::from_text(binding_text(binding))),
            BotCommand::ChangeDirectory(raw_path) => {
                let resolved = resolve_workspace(&binding.cwd, &raw_path);
                if !resolved.exists() {
                    return Err(BridgeError::WorkspaceNotFound(resolved));
                }
                if !resolved.is_dir() {
                    return Err(BridgeError::NotDirectory(resolved));
                }
                debug!(
                    from = %binding.cwd.display(),
                    to = %resolved.display(),
                    "changing conversation workspace"
                );
                binding.cwd = resolved;
                Ok(AgentReply::from_text(binding_text(binding)))
            }
            BotCommand::Agent(agent) => {
                binding.agent = agent.trim().to_string();
                Ok(AgentReply::from_text(binding_text(binding)))
            }
            BotCommand::Permissions(permission_mode) => {
                binding.permission_mode = permission_mode;
                Ok(AgentReply::from_text(binding_text(binding)))
            }
            BotCommand::Session(command) => self.handle_session_command(binding, command).await,
            BotCommand::Status => {
                let status = self.acpx.status(&SessionSelector::from(&*binding)).await?;
                Ok(AgentReply::from_text(status_text(&status)))
            }
            BotCommand::Mode(mode) => {
                let selector = SessionSelector::from(&*binding);
                let _ = self.acpx.ensure_session(&selector).await?;
                self.acpx.set_mode(&selector, &mode).await?;
                Ok(AgentReply::from_text(format!("mode 已设置为 {mode}")))
            }
            BotCommand::Model(model) => {
                let selector = SessionSelector::from(&*binding);
                let _ = self.acpx.ensure_session(&selector).await?;
                self.acpx.set_model(&selector, &model).await?;
                Ok(AgentReply::from_text(format!("model 已设置为 {model}")))
            }
            BotCommand::Option { key, value } => {
                let selector = SessionSelector::from(&*binding);
                let _ = self.acpx.ensure_session(&selector).await?;
                self.acpx.set_option(&selector, &key, &value).await?;
                Ok(AgentReply::from_text(format!("已设置 {key}={value}")))
            }
            BotCommand::Prompt(prompt) => {
                self.handle_prompt(binding, &[MessageBlock::text(prompt)]).await
            }
            BotCommand::Exec(prompt) => {
                let selector = SessionSelector::from(&*binding);
                self.acpx.exec(&selector, &[MessageBlock::text(prompt)]).await
            }
            BotCommand::Shell(command) => {
                let output = self.shell.execute(&binding.cwd, &command).await?;
                Ok(AgentReply::from_text(shell_output_text(&output)))
            }
            BotCommand::Cancel => {
                let cancelled = self.acpx.cancel(&SessionSelector::from(&*binding)).await?;
                Ok(AgentReply::from_text(if cancelled {
                    "已请求取消当前任务。".to_string()
                } else {
                    "当前没有正在执行的任务。".to_string()
                }))
            }
        }
    }

    async fn handle_session_command(
        &self,
        binding: &mut ConversationBinding,
        command: SessionCommand,
    ) -> Result<AgentReply, BridgeError> {
        match command {
            SessionCommand::New(name) => {
                binding.session_name = normalize_session_name(name);
                let summary = self
                    .acpx
                    .new_session(&SessionSelector::from(&*binding))
                    .await?;
                Ok(AgentReply::from_text(session_summary_text(
                    &summary,
                    "已创建新 session",
                )))
            }
            SessionCommand::Use(name) => {
                binding.session_name = normalize_session_name(name);
                let summary = self
                    .acpx
                    .ensure_session(&SessionSelector::from(&*binding))
                    .await?;
                let action = if summary.created {
                    "session 不存在，已创建"
                } else {
                    "已切换 session"
                };
                Ok(AgentReply::from_text(session_summary_text(&summary, action)))
            }
            SessionCommand::Show(name) => {
                let selector = SessionSelector::from(&*binding)
                    .with_session_name(normalize_session_name(name));
                let record = self.acpx.show_session(&selector).await?;
                Ok(AgentReply::from_text(session_record_text(&record)))
            }
            SessionCommand::Close(name) => {
                let target = normalize_session_name(name);
                let selector = SessionSelector::from(&*binding).with_session_name(target.clone());
                let summary = self.acpx.close_session(&selector).await?;
                if target == binding.session_name {
                    binding.session_name = None;
                }
                Ok(AgentReply::from_text(session_summary_text(
                    &summary,
                    "已关闭 session",
                )))
            }
            SessionCommand::List => {
                let records = self.acpx.list_sessions(&binding.agent).await?;
                Ok(AgentReply::from_text(session_list_text(&records, binding)))
            }
            SessionCommand::History(limit) => {
                let entries = self
                    .acpx
                    .history(&SessionSelector::from(&*binding), limit)
                    .await?;
                Ok(AgentReply::from_text(history_text(&entries)))
            }
        }
    }

    async fn load_binding(
        &self,
        key: &ConversationKey,
    ) -> Result<ConversationBinding, BridgeError> {
        let loaded = self.repository.get(key).await?;
        let had_binding = loaded.is_some();
        let mut binding = loaded.unwrap_or_else(|| ConversationBinding {
            cwd: self.defaults.default_workspace.clone(),
            agent: self.defaults.default_agent.clone(),
            session_name: None,
            permission_mode: self.defaults.default_permission_mode.clone(),
        });
        debug!(
            conversation = %key.storage_key(),
            loaded = had_binding,
            agent = %binding.agent,
            session = binding.session_name.as_deref().unwrap_or("default"),
            cwd = %binding.cwd.display(),
            "loaded conversation binding"
        );

        if !binding.cwd.exists() || !binding.cwd.is_dir() {
            warn!(
                conversation = %key.storage_key(),
                invalid_cwd = %binding.cwd.display(),
                fallback_cwd = %self.defaults.default_workspace.display(),
                "conversation workspace missing, resetting to default"
            );
            binding.cwd = self.defaults.default_workspace.clone();
        }

        Ok(binding)
    }

    async fn send_response(
        &self,
        message: &InboundMessage,
        reply: &AgentReply,
    ) -> Result<(), BridgeError> {
        let blocks = expand_reply_blocks(&reply.blocks, self.defaults.reply_chunk_chars);
        debug!(
            conversation = %message.conversation.storage_key(),
            blocks = blocks.len(),
            preview = %blocks_preview(&blocks, 200),
            "sending response to feishu"
        );
        self.channel.send_message(&message.reply_target, &blocks).await
    }

    async fn lock_conversation(&self, key: &ConversationKey) -> OwnedMutexGuard<()> {
        let lock = {
            let mut locks = self.conversation_locks.lock().await;
            locks
                .entry(key.storage_key())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        lock.lock_owned().await
    }
}

#[async_trait]
impl MessageHandler for BridgeService {
    async fn handle_message(&self, message: InboundMessage) -> Result<(), BridgeError> {
        let started_at = Instant::now();
        let conversation_key = message.conversation.storage_key();
        debug!(
            conversation = %conversation_key,
            reply_to = %message.reply_target.reply_to_message_id,
            blocks = %blocks_preview(&message.blocks, 120),
            "received inbound message for handling"
        );
        let typing_reaction_started_at = Instant::now();
        let typing_reaction_ok = match self.channel.react_typing(&message.reply_target).await {
            Ok(()) => true,
            Err(error) => {
                warn!(
                    conversation = %conversation_key,
                    reply_to = %message.reply_target.reply_to_message_id,
                    error = %error,
                    "failed to send typing reaction"
                );
                false
            }
        };
        let typing_reaction_ms = typing_reaction_started_at.elapsed().as_millis() as u64;

        let _guard = self.lock_conversation(&message.conversation).await;
        let processing_started_at = Instant::now();
        let process_result = self.process_message(&message).await;
        let process_ok = process_result.is_ok();
        let reply = match process_result {
            Ok(reply) => reply,
            Err(error) => {
                warn!(
                    conversation = %conversation_key,
                    error = %error,
                    "message processing failed, returning user-facing error"
                );
                AgentReply::from_text(error.user_message())
            }
        };
        let processing_ms = processing_started_at.elapsed().as_millis() as u64;

        let reply_started_at = Instant::now();
        let send_result = self.send_response(&message, &reply).await;
        let reply_ms = reply_started_at.elapsed().as_millis() as u64;
        let total_ms = started_at.elapsed().as_millis() as u64;
        let reply_ok = send_result.is_ok();
        let metrics = self.metrics.record(InteractionSample {
            typing_reaction_ok,
            process_ok,
            reply_ok,
            typing_reaction_ms,
            processing_ms,
            reply_ms,
            total_ms,
        });
        info!(
            conversation = %conversation_key,
            outcome = interaction_outcome(process_ok, reply_ok),
            typing_reaction_ok,
            reply_ok,
            typing_reaction_ms,
            processing_ms,
            reply_ms,
            total_ms,
            interactions_total = metrics.interactions_total,
            successful_interactions_total = metrics.successful_interactions_total,
            process_errors_total = metrics.process_errors_total,
            reply_errors_total = metrics.reply_errors_total,
            typing_reaction_errors_total = metrics.typing_reaction_errors_total,
            avg_total_ms = average_ms(metrics.total_ms_total, metrics.interactions_total),
            max_total_ms = metrics.max_total_ms,
            "interaction metrics"
        );
        send_result?;
        debug!(
            conversation = %conversation_key,
            elapsed_ms = total_ms,
            "finished handling inbound message"
        );
        Ok(())
    }
}

fn average_ms(total_ms: u64, count: u64) -> u64 {
    if count == 0 { 0 } else { total_ms / count }
}

fn interaction_outcome(process_ok: bool, reply_ok: bool) -> &'static str {
    match (process_ok, reply_ok) {
        (true, true) => "ok",
        (false, true) => "handled-error",
        (true, false) => "reply-failed",
        (false, false) => "reply-failed-after-error",
    }
}

fn request_kind(request: Option<&UserRequest>) -> &'static str {
    match request {
        Some(UserRequest::Prompt(_)) | None => "prompt",
        Some(UserRequest::Command(command)) => command_name(command),
    }
}

fn expand_reply_blocks(blocks: &[MessageBlock], reply_chunk_chars: usize) -> Vec<MessageBlock> {
    let mut expanded = Vec::new();

    for block in blocks {
        match block {
            MessageBlock::Text { text } => {
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    continue;
                }
                for chunk in finalize_chunk_labels(chunk_text(trimmed, reply_chunk_chars)) {
                    expanded.push(MessageBlock::text(chunk));
                }
            }
            other => expanded.push(other.clone()),
        }
    }

    if expanded.is_empty() {
        expanded.push(MessageBlock::text(
            "Codex 已完成，但没有返回可显示的内容。",
        ));
    }

    expanded
}

fn blocks_preview(blocks: &[MessageBlock], max_chars: usize) -> String {
    let summary = blocks
        .iter()
        .map(|block| match block {
            MessageBlock::Text { text } => format!("text:{}", text_preview(text, 48)),
            MessageBlock::Image { mime_type, .. } => format!("image:{mime_type}"),
            MessageBlock::Document { file_name, .. } => format!("document:{file_name}"),
        })
        .collect::<Vec<_>>()
        .join(" | ");
    text_preview(&summary, max_chars)
}

fn command_name(command: &BotCommand) -> &'static str {
    match command {
        BotCommand::Help => "help",
        BotCommand::Pwd => "pwd",
        BotCommand::ChangeDirectory(_) => "cd",
        BotCommand::Agent(_) => "agent",
        BotCommand::Permissions(_) => "permissions",
        BotCommand::Session(SessionCommand::New(_)) => "session-new",
        BotCommand::Session(SessionCommand::Use(_)) => "session-use",
        BotCommand::Session(SessionCommand::Show(_)) => "session-show",
        BotCommand::Session(SessionCommand::Close(_)) => "session-close",
        BotCommand::Session(SessionCommand::List) => "session-list",
        BotCommand::Session(SessionCommand::History(_)) => "session-history",
        BotCommand::Status => "status",
        BotCommand::Mode(_) => "mode",
        BotCommand::Model(_) => "model",
        BotCommand::Option { .. } => "set",
        BotCommand::Prompt(_) => "prompt-command",
        BotCommand::Exec(_) => "exec",
        BotCommand::Shell(_) => "shell",
        BotCommand::Cancel => "cancel",
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, path::PathBuf, sync::Arc};

    use async_trait::async_trait;
    use tempfile::tempdir;
    use tokio::sync::Mutex;

    use crate::{
        application::service::{BridgeService, ServiceDefaults},
        domain::{
            AgentReply, BinarySource, ConversationBinding, ConversationKey, InboundMessage,
            MessageBlock, PermissionMode, ReplyTarget, SessionHistoryEntry, SessionRecord,
            SessionSelector, SessionStatus, SessionSummary, ShellOutput,
        },
        error::BridgeError,
        ports::{
            AcpxGateway, ChannelClient, ConversationRepository, MessageHandler, ShellExecutor,
        },
    };

    struct TestChannel {
        sent_texts: Mutex<Vec<String>>,
        sent_documents: Mutex<Vec<String>>,
        typing_reactions: Mutex<Vec<String>>,
        events: Mutex<Vec<String>>,
        typing_reaction_error: Option<String>,
    }

    impl Default for TestChannel {
        fn default() -> Self {
            Self::with_typing_reaction_error(None)
        }
    }

    impl TestChannel {
        fn with_typing_reaction_error(error: Option<&str>) -> Self {
            Self {
                sent_texts: Mutex::new(Vec::new()),
                sent_documents: Mutex::new(Vec::new()),
                typing_reactions: Mutex::new(Vec::new()),
                events: Mutex::new(Vec::new()),
                typing_reaction_error: error.map(ToString::to_string),
            }
        }
    }

    #[async_trait]
    impl ChannelClient for TestChannel {
        async fn react_typing(&self, target: &ReplyTarget) -> Result<(), BridgeError> {
            self.typing_reactions
                .lock()
                .await
                .push(target.reply_to_message_id.clone());
            self.events
                .lock()
                .await
                .push(format!("typing:{}", target.reply_to_message_id));
            if let Some(message) = &self.typing_reaction_error {
                return Err(BridgeError::Channel(message.clone()));
            }
            Ok(())
        }

        async fn send_message(
            &self,
            _target: &ReplyTarget,
            blocks: &[MessageBlock],
        ) -> Result<(), BridgeError> {
            for block in blocks {
                match block {
                    MessageBlock::Text { text } => {
                        self.sent_texts.lock().await.push(text.to_string());
                        self.events.lock().await.push(format!("send-text:{text}"));
                    }
                    MessageBlock::Document { file_name, .. } => {
                        self.sent_documents.lock().await.push(file_name.clone());
                        self.events
                            .lock()
                            .await
                            .push(format!("send-document:{file_name}"));
                    }
                    MessageBlock::Image { .. } => {
                        self.events.lock().await.push("send-image".to_string());
                    }
                }
            }
            Ok(())
        }
    }

    #[derive(Default)]
    struct TestRepository {
        binding: Mutex<Option<ConversationBinding>>,
    }

    #[async_trait]
    impl ConversationRepository for TestRepository {
        async fn get(
            &self,
            _key: &ConversationKey,
        ) -> Result<Option<ConversationBinding>, BridgeError> {
            Ok(self.binding.lock().await.clone())
        }

        async fn put(
            &self,
            _key: &ConversationKey,
            binding: &ConversationBinding,
        ) -> Result<(), BridgeError> {
            *self.binding.lock().await = Some(binding.clone());
            Ok(())
        }
    }

    struct TestAcpx {
        // The bridge talks to Codex via ACPX. In tests we script the response queue so command
        // sequences can assert on state changes without requiring network access or a real agent.
        ensure_calls: Mutex<Vec<SessionSelector>>,
        prompt_calls: Mutex<Vec<(SessionSelector, Vec<MessageBlock>)>>,
        prompt_responses: Mutex<VecDeque<Result<AgentReply, BridgeError>>>,
    }

    impl Default for TestAcpx {
        fn default() -> Self {
            Self::with_prompt_texts(["answer"])
        }
    }

    impl TestAcpx {
        fn with_prompt_texts<const N: usize>(responses: [&str; N]) -> Self {
            Self {
                ensure_calls: Mutex::new(Vec::new()),
                prompt_calls: Mutex::new(Vec::new()),
                prompt_responses: Mutex::new(VecDeque::from(
                    responses.map(|text| Ok(AgentReply::from_text(text))),
                )),
            }
        }
    }

    #[async_trait]
    impl AcpxGateway for TestAcpx {
        async fn ensure_session(
            &self,
            selector: &SessionSelector,
        ) -> Result<SessionSummary, BridgeError> {
            self.ensure_calls.lock().await.push(selector.clone());
            Ok(SessionSummary {
                record_id: "rec-1".to_string(),
                name: Some("backend".to_string()),
                created: true,
                acp_session_id: Some("acp-1".to_string()),
                agent_session_id: None,
            })
        }

        async fn new_session(
            &self,
            selector: &SessionSelector,
        ) -> Result<SessionSummary, BridgeError> {
            Ok(SessionSummary {
                record_id: "rec-new".to_string(),
                name: selector.session_name.clone(),
                created: true,
                acp_session_id: Some("acp-1".to_string()),
                agent_session_id: None,
            })
        }

        async fn close_session(
            &self,
            selector: &SessionSelector,
        ) -> Result<SessionSummary, BridgeError> {
            Ok(SessionSummary {
                record_id: "rec-close".to_string(),
                name: selector.session_name.clone(),
                created: false,
                acp_session_id: Some("acp-1".to_string()),
                agent_session_id: None,
            })
        }

        async fn show_session(
            &self,
            selector: &SessionSelector,
        ) -> Result<SessionRecord, BridgeError> {
            Ok(SessionRecord {
                record_id: "rec-show".to_string(),
                acp_session_id: "acp-1".to_string(),
                agent_session_id: None,
                agent: selector.agent.clone(),
                cwd: selector.cwd.clone(),
                name: selector.session_name.clone(),
                title: Some("Session title".to_string()),
                first_user_preview: Some("first prompt".to_string()),
                created_at: "2026".to_string(),
                last_used_at: "2026".to_string(),
                last_prompt_at: Some("2026".to_string()),
                closed: false,
                model: Some("gpt-5.4".to_string()),
                mode: Some("auto".to_string()),
            })
        }

        async fn list_sessions(&self, agent: &str) -> Result<Vec<SessionRecord>, BridgeError> {
            Ok(vec![SessionRecord {
                record_id: "rec-list".to_string(),
                acp_session_id: "acp-1".to_string(),
                agent_session_id: None,
                agent: agent.to_string(),
                cwd: PathBuf::from("/repo"),
                name: Some("backend".to_string()),
                title: Some("Backend session".to_string()),
                first_user_preview: Some("analyze backend failures".to_string()),
                created_at: "2026".to_string(),
                last_used_at: "2026".to_string(),
                last_prompt_at: None,
                closed: false,
                model: None,
                mode: None,
            }])
        }

        async fn history(
            &self,
            _selector: &SessionSelector,
            _limit: usize,
        ) -> Result<Vec<SessionHistoryEntry>, BridgeError> {
            Ok(vec![SessionHistoryEntry {
                role: "assistant".to_string(),
                timestamp: "2026".to_string(),
                text_preview: "done".to_string(),
            }])
        }

        async fn status(&self, selector: &SessionSelector) -> Result<SessionStatus, BridgeError> {
            Ok(SessionStatus {
                status: "running".to_string(),
                session_id: Some("rec-status".to_string()),
                agent_command: selector.agent.clone(),
                pid: Some(123),
                model: Some("gpt-5.4".to_string()),
                mode: Some("auto".to_string()),
                uptime: Some("00:00:10".to_string()),
                last_prompt_time: Some("2026".to_string()),
                exit_code: None,
                signal: None,
                agent_session_id: None,
            })
        }

        async fn prompt(
            &self,
            selector: &SessionSelector,
            blocks: &[MessageBlock],
        ) -> Result<AgentReply, BridgeError> {
            self.prompt_calls
                .lock()
                .await
                .push((selector.clone(), blocks.to_vec()));

            self
                .prompt_responses
                .lock()
                .await
                .pop_front()
                .unwrap_or_else(|| Ok(AgentReply::from_text("answer")))
        }

        async fn exec(
            &self,
            _selector: &SessionSelector,
            _blocks: &[MessageBlock],
        ) -> Result<AgentReply, BridgeError> {
            Ok(AgentReply::from_text("one-shot"))
        }

        async fn set_mode(
            &self,
            _selector: &SessionSelector,
            _mode: &str,
        ) -> Result<(), BridgeError> {
            Ok(())
        }

        async fn set_model(
            &self,
            _selector: &SessionSelector,
            _model: &str,
        ) -> Result<(), BridgeError> {
            Ok(())
        }

        async fn set_option(
            &self,
            _selector: &SessionSelector,
            _key: &str,
            _value: &str,
        ) -> Result<(), BridgeError> {
            Ok(())
        }

        async fn cancel(&self, _selector: &SessionSelector) -> Result<bool, BridgeError> {
            Ok(true)
        }
    }

    #[derive(Default)]
    struct TestShell;

    #[async_trait]
    impl ShellExecutor for TestShell {
        async fn execute(
            &self,
            cwd: &std::path::Path,
            command: &str,
        ) -> Result<ShellOutput, BridgeError> {
            Ok(ShellOutput {
                command: command.to_string(),
                cwd: cwd.to_path_buf(),
                exit_code: 0,
                stdout: "shell-ok".to_string(),
                stderr: String::new(),
            })
        }
    }

    fn defaults(workspace: PathBuf) -> ServiceDefaults {
        defaults_with_prefix(workspace, "/command")
    }

    fn defaults_with_prefix(workspace: PathBuf, command_prefix: &str) -> ServiceDefaults {
        ServiceDefaults {
            command_prefix: command_prefix.to_string(),
            default_workspace: workspace,
            default_agent: "codex".to_string(),
            default_permission_mode: PermissionMode::ApproveReads,
            reply_chunk_chars: 10_000,
        }
    }

    fn binding_for(workspace: PathBuf) -> ConversationBinding {
        ConversationBinding {
            cwd: workspace,
            agent: "codex".to_string(),
            session_name: None,
            permission_mode: PermissionMode::ApproveReads,
        }
    }

    fn inbound(text: &str) -> InboundMessage {
        InboundMessage {
            conversation: ConversationKey {
                tenant_key: "tenant".to_string(),
                chat_id: "chat".to_string(),
                user_open_id: "user".to_string(),
                thread_id: None,
            },
            reply_target: ReplyTarget {
                chat_id: "chat".to_string(),
                reply_to_message_id: "msg".to_string(),
            },
            blocks: vec![MessageBlock::text(text)],
        }
    }

    fn inbound_markdown(file_name: &str, text: &str) -> InboundMessage {
        InboundMessage {
            conversation: ConversationKey {
                tenant_key: "tenant".to_string(),
                chat_id: "chat".to_string(),
                user_open_id: "user".to_string(),
                thread_id: None,
            },
            reply_target: ReplyTarget {
                chat_id: "chat".to_string(),
                reply_to_message_id: "msg".to_string(),
            },
            blocks: vec![MessageBlock::Document {
                mime_type: "text/markdown".to_string(),
                file_name: file_name.to_string(),
                source: BinarySource::Bytes(text.as_bytes().to_vec()),
                extracted_text: Some(text.to_string()),
            }],
        }
    }

    #[tokio::test]
    async fn prompt_flow_ensures_session_and_replies() {
        let dir = tempdir().unwrap();
        let channel = Arc::new(TestChannel::default());
        let acpx = Arc::new(TestAcpx::default());
        let service = BridgeService::new(
            channel.clone(),
            acpx.clone(),
            Arc::new(TestShell),
            Arc::new(TestRepository::default()),
            defaults(dir.path().to_path_buf()),
        );

        service.handle_message(inbound("分析仓库")).await.unwrap();

        assert_eq!(acpx.ensure_calls.lock().await.len(), 1);
        assert_eq!(channel.sent_texts.lock().await.as_slice(), &["answer"]);
        assert_eq!(channel.typing_reactions.lock().await.as_slice(), &["msg"]);
        assert_eq!(
            channel.events.lock().await.as_slice(),
            ["typing:msg", "send-text:answer"]
        );
        let metrics = service.metrics.snapshot();
        assert_eq!(metrics.interactions_total, 1);
        assert_eq!(metrics.successful_interactions_total, 1);
        assert_eq!(metrics.process_errors_total, 0);
        assert_eq!(metrics.reply_errors_total, 0);
        assert_eq!(metrics.typing_reaction_errors_total, 0);
    }

    #[tokio::test]
    async fn cd_command_updates_persisted_binding() {
        let dir = tempdir().unwrap();
        let workspace = dir.path().join("repo");
        std::fs::create_dir(&workspace).unwrap();
        let channel = Arc::new(TestChannel::default());
        let repository = Arc::new(TestRepository::default());
        let service = BridgeService::new(
            channel.clone(),
            Arc::new(TestAcpx::default()),
            Arc::new(TestShell),
            repository.clone(),
            defaults(dir.path().to_path_buf()),
        );

        service
            .handle_message(inbound(&format!("/command cd {}", workspace.display())))
            .await
            .unwrap();

        let stored = repository.binding.lock().await.clone().unwrap();
        assert_eq!(stored.cwd, workspace);
        assert!(channel.sent_texts.lock().await[0].contains("当前上下文"));
    }

    #[tokio::test]
    async fn session_commands_update_current_session_name() {
        let dir = tempdir().unwrap();
        let repository = Arc::new(TestRepository::default());
        let service = BridgeService::new(
            Arc::new(TestChannel::default()),
            Arc::new(TestAcpx::default()),
            Arc::new(TestShell),
            repository.clone(),
            defaults(dir.path().to_path_buf()),
        );

        service
            .handle_message(inbound("/command session new backend"))
            .await
            .unwrap();
        assert_eq!(
            repository
                .binding
                .lock()
                .await
                .clone()
                .unwrap()
                .session_name,
            Some("backend".to_string())
        );

        service
            .handle_message(inbound("/command session close backend"))
            .await
            .unwrap();
        assert_eq!(
            repository
                .binding
                .lock()
                .await
                .clone()
                .unwrap()
                .session_name,
            None
        );
    }

    #[tokio::test]
    async fn shell_and_status_commands_render_outputs() {
        let dir = tempdir().unwrap();
        let channel = Arc::new(TestChannel::default());
        let service = BridgeService::new(
            channel.clone(),
            Arc::new(TestAcpx::default()),
            Arc::new(TestShell),
            Arc::new(TestRepository::default()),
            defaults(dir.path().to_path_buf()),
        );

        service
            .handle_message(inbound("/command shell git status"))
            .await
            .unwrap();
        service
            .handle_message(inbound("/command status"))
            .await
            .unwrap();

        let sent = channel.sent_texts.lock().await;
        assert!(sent.iter().any(|message| message.contains("shell result")));
        assert!(sent.iter().any(|message| message.contains("status")));
    }

    #[tokio::test]
    async fn typing_reaction_failure_does_not_block_reply_and_is_counted() {
        let dir = tempdir().unwrap();
        let channel = Arc::new(TestChannel::with_typing_reaction_error(Some("boom")));
        let service = BridgeService::new(
            channel.clone(),
            Arc::new(TestAcpx::default()),
            Arc::new(TestShell),
            Arc::new(TestRepository::default()),
            defaults(dir.path().to_path_buf()),
        );

        service.handle_message(inbound("分析仓库")).await.unwrap();

        assert_eq!(channel.sent_texts.lock().await.as_slice(), &["answer"]);
        assert_eq!(
            channel.events.lock().await.as_slice(),
            ["typing:msg", "send-text:answer"]
        );
        let metrics = service.metrics.snapshot();
        assert_eq!(metrics.interactions_total, 1);
        assert_eq!(metrics.successful_interactions_total, 1);
        assert_eq!(metrics.typing_reaction_errors_total, 1);
        assert_eq!(metrics.reply_errors_total, 0);
    }

    #[tokio::test]
    async fn slash_cd_then_pwd_and_prompt_use_updated_workspace() {
        let dir = tempdir().unwrap();
        let initial_workspace = dir.path().join("initial");
        let target_workspace = dir.path().join("target");
        std::fs::create_dir(&initial_workspace).unwrap();
        std::fs::create_dir(&target_workspace).unwrap();

        let channel = Arc::new(TestChannel::default());
        let repository = Arc::new(TestRepository::default());
        *repository.binding.lock().await = Some(binding_for(initial_workspace.clone()));
        let acpx = Arc::new(TestAcpx::with_prompt_texts(["cwd-ok"]));
        let service = BridgeService::new(
            channel.clone(),
            acpx.clone(),
            Arc::new(TestShell),
            repository,
            defaults_with_prefix(initial_workspace, "/"),
        );

        service
            .handle_message(inbound(&format!("/cd {}", target_workspace.display())))
            .await
            .unwrap();
        service.handle_message(inbound("/pwd")).await.unwrap();
        service.handle_message(inbound("继续处理")).await.unwrap();

        let sent = channel.sent_texts.lock().await;
        assert!(sent[0].contains(&format!("cwd: {}", target_workspace.display())));
        assert!(sent[1].contains(&format!("cwd: {}", target_workspace.display())));
        assert_eq!(sent[2], "cwd-ok");

        let ensure_calls = acpx.ensure_calls.lock().await;
        assert_eq!(ensure_calls.len(), 1);
        assert_eq!(ensure_calls[0].cwd, target_workspace);

        let prompt_calls = acpx.prompt_calls.lock().await;
        assert_eq!(prompt_calls.len(), 1);
        assert_eq!(prompt_calls[0].0.cwd, target_workspace);
        assert_eq!(prompt_calls[0].1, vec![MessageBlock::text("继续处理")]);
    }

    #[tokio::test]
    async fn slash_cd_to_missing_directory_returns_error_without_mutating_workspace() {
        let dir = tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let missing = workspace.join("does-not-exist");

        let channel = Arc::new(TestChannel::default());
        let repository = Arc::new(TestRepository::default());
        *repository.binding.lock().await = Some(binding_for(workspace.clone()));
        let acpx = Arc::new(TestAcpx::default());
        let service = BridgeService::new(
            channel.clone(),
            acpx.clone(),
            Arc::new(TestShell),
            repository.clone(),
            defaults_with_prefix(workspace.clone(), "/"),
        );

        service
            .handle_message(inbound(&format!("/cd {}", missing.display())))
            .await
            .unwrap();

        let sent = channel.sent_texts.lock().await;
        assert_eq!(sent.len(), 1);
        assert!(sent[0].contains(&format!("目录不存在：{}", missing.display())));
        assert_eq!(
            repository.binding.lock().await.clone().unwrap().cwd,
            workspace
        );
        assert!(acpx.ensure_calls.lock().await.is_empty());
        assert!(acpx.prompt_calls.lock().await.is_empty());
        let metrics = service.metrics.snapshot();
        assert_eq!(metrics.interactions_total, 1);
        assert_eq!(metrics.process_errors_total, 1);
        assert_eq!(metrics.successful_interactions_total, 0);
    }

    #[tokio::test]
    async fn slash_cd_rejects_control_characters_without_mutating_workspace() {
        let dir = tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();

        let channel = Arc::new(TestChannel::default());
        let repository = Arc::new(TestRepository::default());
        *repository.binding.lock().await = Some(binding_for(workspace.clone()));
        let acpx = Arc::new(TestAcpx::default());
        let service = BridgeService::new(
            channel.clone(),
            acpx.clone(),
            Arc::new(TestShell),
            repository.clone(),
            defaults_with_prefix(workspace.clone(), "/"),
        );

        service
            .handle_message(inbound("/cd /tmp/test\u{001b}[31m"))
            .await
            .unwrap();

        let sent = channel.sent_texts.lock().await;
        assert_eq!(sent.as_slice(), ["输入包含非法控制字符，请移除后重试。"]);
        assert_eq!(
            repository.binding.lock().await.clone().unwrap().cwd,
            workspace
        );
        assert!(acpx.ensure_calls.lock().await.is_empty());
        assert!(acpx.prompt_calls.lock().await.is_empty());
        let metrics = service.metrics.snapshot();
        assert_eq!(metrics.interactions_total, 1);
        assert_eq!(metrics.process_errors_total, 1);
        assert_eq!(metrics.successful_interactions_total, 0);
    }

    #[tokio::test]
    async fn markdown_document_messages_are_forwarded_as_prompt_blocks() {
        let dir = tempdir().unwrap();
        let channel = Arc::new(TestChannel::default());
        let acpx = Arc::new(TestAcpx::with_prompt_texts(["文档已读取"]));
        let service = BridgeService::new(
            channel.clone(),
            acpx.clone(),
            Arc::new(TestShell),
            Arc::new(TestRepository::default()),
            defaults(dir.path().to_path_buf()),
        );

        service
            .handle_message(inbound_markdown("README.md", "# Title\n\ncontent"))
            .await
            .unwrap();

        let prompt_calls = acpx.prompt_calls.lock().await;
        assert_eq!(prompt_calls.len(), 1);
        assert_eq!(
            prompt_calls[0].1,
            vec![MessageBlock::Document {
                mime_type: "text/markdown".to_string(),
                file_name: "README.md".to_string(),
                source: BinarySource::Bytes(b"# Title\n\ncontent".to_vec()),
                extracted_text: Some("# Title\n\ncontent".to_string()),
            }]
        );
        assert_eq!(channel.sent_texts.lock().await.as_slice(), &["文档已读取"]);
    }

    #[tokio::test]
    async fn markdown_document_replies_are_sent_as_files() {
        let dir = tempdir().unwrap();
        let channel = Arc::new(TestChannel::default());
        let acpx = Arc::new(TestAcpx {
            ensure_calls: Mutex::new(Vec::new()),
            prompt_calls: Mutex::new(Vec::new()),
            prompt_responses: Mutex::new(VecDeque::from([Ok(AgentReply {
                blocks: vec![MessageBlock::Document {
                    mime_type: "text/markdown".to_string(),
                    file_name: "plan.md".to_string(),
                    source: BinarySource::Bytes(b"# Plan".to_vec()),
                    extracted_text: Some("# Plan".to_string()),
                }],
            })])),
        });
        let service = BridgeService::new(
            channel.clone(),
            acpx,
            Arc::new(TestShell),
            Arc::new(TestRepository::default()),
            defaults(dir.path().to_path_buf()),
        );

        service.handle_message(inbound("生成 markdown")).await.unwrap();

        assert_eq!(channel.sent_documents.lock().await.as_slice(), &["plan.md"]);
    }
}
