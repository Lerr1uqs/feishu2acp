use std::{path::PathBuf, sync::Arc, time::Duration};

use async_trait::async_trait;

use crate::{
    domain::{
        ConversationBinding, ConversationKey, InboundMessage, PromptResponse, ReplyTarget,
        SessionHistoryEntry, SessionRecord, SessionSelector, SessionStatus, SessionSummary,
        ShellOutput,
    },
    error::BridgeError,
};

#[derive(Clone, Debug, PartialEq)]
pub struct ProcessRequest {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub timeout: Duration,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

#[async_trait]
pub trait ProcessRunner: Send + Sync {
    async fn run(&self, request: ProcessRequest) -> Result<ProcessOutput, BridgeError>;
}

#[async_trait]
pub trait ChannelClient: Send + Sync {
    async fn send_text(&self, target: &ReplyTarget, text: &str) -> Result<(), BridgeError>;
}

#[async_trait]
pub trait MessageHandler: Send + Sync {
    async fn handle_message(&self, message: InboundMessage) -> Result<(), BridgeError>;
}

#[async_trait(?Send)]
pub trait ChannelRuntime: Send + Sync {
    async fn run(&self, handler: Arc<dyn MessageHandler>) -> Result<(), BridgeError>;
}

#[async_trait]
pub trait ConversationRepository: Send + Sync {
    async fn get(&self, key: &ConversationKey) -> Result<Option<ConversationBinding>, BridgeError>;

    async fn put(
        &self,
        key: &ConversationKey,
        binding: &ConversationBinding,
    ) -> Result<(), BridgeError>;
}

#[async_trait]
pub trait ShellExecutor: Send + Sync {
    async fn execute(
        &self,
        cwd: &std::path::Path,
        command: &str,
    ) -> Result<ShellOutput, BridgeError>;
}

#[async_trait]
pub trait AcpxGateway: Send + Sync {
    async fn ensure_session(
        &self,
        selector: &SessionSelector,
    ) -> Result<SessionSummary, BridgeError>;

    async fn new_session(&self, selector: &SessionSelector) -> Result<SessionSummary, BridgeError>;

    async fn close_session(
        &self,
        selector: &SessionSelector,
    ) -> Result<SessionSummary, BridgeError>;

    async fn show_session(&self, selector: &SessionSelector) -> Result<SessionRecord, BridgeError>;

    async fn list_sessions(&self, agent: &str) -> Result<Vec<SessionRecord>, BridgeError>;

    async fn history(
        &self,
        selector: &SessionSelector,
        limit: usize,
    ) -> Result<Vec<SessionHistoryEntry>, BridgeError>;

    async fn status(&self, selector: &SessionSelector) -> Result<SessionStatus, BridgeError>;

    async fn prompt(
        &self,
        selector: &SessionSelector,
        prompt: &str,
    ) -> Result<PromptResponse, BridgeError>;

    async fn exec(
        &self,
        selector: &SessionSelector,
        prompt: &str,
    ) -> Result<PromptResponse, BridgeError>;

    async fn set_mode(&self, selector: &SessionSelector, mode: &str) -> Result<(), BridgeError>;

    async fn set_model(&self, selector: &SessionSelector, model: &str) -> Result<(), BridgeError>;

    async fn set_option(
        &self,
        selector: &SessionSelector,
        key: &str,
        value: &str,
    ) -> Result<(), BridgeError>;

    async fn cancel(&self, selector: &SessionSelector) -> Result<bool, BridgeError>;
}
