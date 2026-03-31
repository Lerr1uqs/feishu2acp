use std::{collections::HashMap, path::PathBuf, sync::Arc};

use async_trait::async_trait;
use tokio::sync::{Mutex, OwnedMutexGuard};

use crate::{
    application::{
        command::{BotCommand, SessionCommand, UserRequest, parse_user_request},
        render::{
            binding_text, help_text, history_text, prompt_text, session_list_text,
            session_record_text, session_summary_text, shell_output_text, status_text,
        },
    },
    domain::{
        ConversationBinding, ConversationKey, InboundMessage, PermissionMode, SessionSelector,
    },
    error::BridgeError,
    ports::{AcpxGateway, ChannelClient, ConversationRepository, MessageHandler, ShellExecutor},
    support::{chunk_text, finalize_chunk_labels, normalize_session_name, resolve_workspace},
};

#[derive(Clone, Debug)]
pub struct ServiceDefaults {
    pub command_prefix: String,
    pub default_workspace: PathBuf,
    pub default_agent: String,
    pub default_permission_mode: PermissionMode,
    pub reply_chunk_chars: usize,
}

pub struct BridgeService {
    channel: Arc<dyn ChannelClient>,
    acpx: Arc<dyn AcpxGateway>,
    shell: Arc<dyn ShellExecutor>,
    repository: Arc<dyn ConversationRepository>,
    defaults: ServiceDefaults,
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
            conversation_locks: Mutex::new(HashMap::new()),
        }
    }

    async fn process_message(&self, message: &InboundMessage) -> Result<String, BridgeError> {
        let mut binding = self.load_binding(&message.conversation).await?;
        let request = parse_user_request(&self.defaults.command_prefix, &message.text)?;
        let response = match request {
            UserRequest::Prompt(prompt) => self.handle_prompt(&binding, &prompt).await?,
            UserRequest::Command(command) => self.handle_command(&mut binding, command).await?,
        };
        self.repository.put(&message.conversation, &binding).await?;
        Ok(response)
    }

    async fn handle_prompt(
        &self,
        binding: &ConversationBinding,
        prompt: &str,
    ) -> Result<String, BridgeError> {
        let selector = SessionSelector::from(binding);
        let _ = self.acpx.ensure_session(&selector).await?;
        let response = self.acpx.prompt(&selector, prompt).await?;
        Ok(prompt_text(&response))
    }

    async fn handle_command(
        &self,
        binding: &mut ConversationBinding,
        command: BotCommand,
    ) -> Result<String, BridgeError> {
        match command {
            BotCommand::Help => Ok(help_text(&self.defaults.command_prefix)),
            BotCommand::Pwd => Ok(binding_text(binding)),
            BotCommand::ChangeDirectory(raw_path) => {
                let resolved = resolve_workspace(&binding.cwd, &raw_path);
                if !resolved.exists() {
                    return Err(BridgeError::WorkspaceNotFound(resolved));
                }
                if !resolved.is_dir() {
                    return Err(BridgeError::NotDirectory(resolved));
                }
                binding.cwd = resolved;
                Ok(binding_text(binding))
            }
            BotCommand::Agent(agent) => {
                binding.agent = agent.trim().to_string();
                Ok(binding_text(binding))
            }
            BotCommand::Permissions(permission_mode) => {
                binding.permission_mode = permission_mode;
                Ok(binding_text(binding))
            }
            BotCommand::Session(command) => self.handle_session_command(binding, command).await,
            BotCommand::Status => {
                let status = self.acpx.status(&SessionSelector::from(&*binding)).await?;
                Ok(status_text(&status))
            }
            BotCommand::Mode(mode) => {
                let selector = SessionSelector::from(&*binding);
                let _ = self.acpx.ensure_session(&selector).await?;
                self.acpx.set_mode(&selector, &mode).await?;
                Ok(format!("mode 已设置为 {mode}"))
            }
            BotCommand::Model(model) => {
                let selector = SessionSelector::from(&*binding);
                let _ = self.acpx.ensure_session(&selector).await?;
                self.acpx.set_model(&selector, &model).await?;
                Ok(format!("model 已设置为 {model}"))
            }
            BotCommand::Option { key, value } => {
                let selector = SessionSelector::from(&*binding);
                let _ = self.acpx.ensure_session(&selector).await?;
                self.acpx.set_option(&selector, &key, &value).await?;
                Ok(format!("已设置 {key}={value}"))
            }
            BotCommand::Prompt(prompt) => self.handle_prompt(binding, &prompt).await,
            BotCommand::Exec(prompt) => {
                let selector = SessionSelector::from(&*binding);
                let response = self.acpx.exec(&selector, &prompt).await?;
                Ok(prompt_text(&response))
            }
            BotCommand::Shell(command) => {
                let output = self.shell.execute(&binding.cwd, &command).await?;
                Ok(shell_output_text(&output))
            }
            BotCommand::Cancel => {
                let cancelled = self.acpx.cancel(&SessionSelector::from(&*binding)).await?;
                Ok(if cancelled {
                    "已请求取消当前任务。".to_string()
                } else {
                    "当前没有正在执行的任务。".to_string()
                })
            }
        }
    }

    async fn handle_session_command(
        &self,
        binding: &mut ConversationBinding,
        command: SessionCommand,
    ) -> Result<String, BridgeError> {
        match command {
            SessionCommand::New(name) => {
                binding.session_name = normalize_session_name(name);
                let summary = self
                    .acpx
                    .new_session(&SessionSelector::from(&*binding))
                    .await?;
                Ok(session_summary_text(&summary, "已创建新 session"))
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
                Ok(session_summary_text(&summary, action))
            }
            SessionCommand::Show(name) => {
                let selector = SessionSelector::from(&*binding)
                    .with_session_name(normalize_session_name(name));
                let record = self.acpx.show_session(&selector).await?;
                Ok(session_record_text(&record))
            }
            SessionCommand::Close(name) => {
                let target = normalize_session_name(name);
                let selector = SessionSelector::from(&*binding).with_session_name(target.clone());
                let summary = self.acpx.close_session(&selector).await?;
                if target == binding.session_name {
                    binding.session_name = None;
                }
                Ok(session_summary_text(&summary, "已关闭 session"))
            }
            SessionCommand::List => {
                let records = self.acpx.list_sessions(&binding.agent).await?;
                Ok(session_list_text(&records, &binding.cwd))
            }
            SessionCommand::History(limit) => {
                let entries = self
                    .acpx
                    .history(&SessionSelector::from(&*binding), limit)
                    .await?;
                Ok(history_text(&entries))
            }
        }
    }

    async fn load_binding(
        &self,
        key: &ConversationKey,
    ) -> Result<ConversationBinding, BridgeError> {
        let loaded = self.repository.get(key).await?;
        let mut binding = loaded.unwrap_or_else(|| ConversationBinding {
            cwd: self.defaults.default_workspace.clone(),
            agent: self.defaults.default_agent.clone(),
            session_name: None,
            permission_mode: self.defaults.default_permission_mode.clone(),
        });

        if !binding.cwd.exists() || !binding.cwd.is_dir() {
            binding.cwd = self.defaults.default_workspace.clone();
        }

        Ok(binding)
    }

    async fn send_response(&self, message: &InboundMessage, text: &str) -> Result<(), BridgeError> {
        let chunks = finalize_chunk_labels(chunk_text(text, self.defaults.reply_chunk_chars));
        for chunk in chunks {
            self.channel
                .send_text(&message.reply_target, &chunk)
                .await?;
        }
        Ok(())
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
        let _guard = self.lock_conversation(&message.conversation).await;
        let reply = match self.process_message(&message).await {
            Ok(reply) => reply,
            Err(error) => error.user_message(),
        };
        self.send_response(&message, &reply).await
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
            ConversationBinding, ConversationKey, InboundMessage, PermissionMode, PromptResponse,
            ReplyTarget, SessionHistoryEntry, SessionRecord, SessionSelector, SessionStatus,
            SessionSummary, ShellOutput,
        },
        error::BridgeError,
        ports::{
            AcpxGateway, ChannelClient, ConversationRepository, MessageHandler, ShellExecutor,
        },
    };

    #[derive(Default)]
    struct TestChannel {
        sent: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl ChannelClient for TestChannel {
        async fn send_text(&self, _target: &ReplyTarget, text: &str) -> Result<(), BridgeError> {
            self.sent.lock().await.push(text.to_string());
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
        ensure_calls: Mutex<usize>,
        prompt_responses: Mutex<VecDeque<String>>,
    }

    impl Default for TestAcpx {
        fn default() -> Self {
            Self {
                ensure_calls: Mutex::new(0),
                prompt_responses: Mutex::new(VecDeque::from([String::from("answer")])),
            }
        }
    }

    #[async_trait]
    impl AcpxGateway for TestAcpx {
        async fn ensure_session(
            &self,
            _selector: &SessionSelector,
        ) -> Result<SessionSummary, BridgeError> {
            *self.ensure_calls.lock().await += 1;
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
            _selector: &SessionSelector,
            _prompt: &str,
        ) -> Result<PromptResponse, BridgeError> {
            let text = self
                .prompt_responses
                .lock()
                .await
                .pop_front()
                .unwrap_or_else(|| "answer".to_string());
            Ok(PromptResponse { text })
        }

        async fn exec(
            &self,
            _selector: &SessionSelector,
            _prompt: &str,
        ) -> Result<PromptResponse, BridgeError> {
            Ok(PromptResponse {
                text: "one-shot".to_string(),
            })
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
        ServiceDefaults {
            command_prefix: "/command".to_string(),
            default_workspace: workspace,
            default_agent: "codex".to_string(),
            default_permission_mode: PermissionMode::ApproveReads,
            reply_chunk_chars: 10_000,
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
            text: text.to_string(),
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

        assert_eq!(*acpx.ensure_calls.lock().await, 1);
        assert_eq!(channel.sent.lock().await.as_slice(), &["answer"]);
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
        assert!(channel.sent.lock().await[0].contains("当前上下文"));
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

        let sent = channel.sent.lock().await;
        assert!(sent.iter().any(|message| message.contains("shell result")));
        assert!(sent.iter().any(|message| message.contains("status")));
    }
}
