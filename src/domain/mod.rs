use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ConversationKey {
    pub tenant_key: String,
    pub chat_id: String,
    pub user_open_id: String,
    pub thread_id: Option<String>,
}

impl ConversationKey {
    pub fn storage_key(&self) -> String {
        format!(
            "{}::{}::{}::{}",
            self.tenant_key,
            self.chat_id,
            self.user_open_id,
            self.thread_id.as_deref().unwrap_or("-")
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplyTarget {
    pub chat_id: String,
    pub reply_to_message_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BinarySource {
    Bytes(Vec<u8>),
    LocalPath(PathBuf),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MessageBlock {
    Text {
        text: String,
    },
    Image {
        mime_type: String,
        source: BinarySource,
        alt: Option<String>,
    },
    Document {
        mime_type: String,
        file_name: String,
        source: BinarySource,
        extracted_text: Option<String>,
    },
}

impl MessageBlock {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text { text } => Some(text),
            _ => None,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Text { .. } => "text",
            Self::Image { .. } => "image",
            Self::Document { .. } => "document",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InboundMessage {
    pub conversation: ConversationKey,
    pub reply_target: ReplyTarget,
    pub blocks: Vec<MessageBlock>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PermissionMode {
    ApproveAll,
    ApproveReads,
    DenyAll,
}

impl PermissionMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ApproveAll => "approve-all",
            Self::ApproveReads => "approve-reads",
            Self::DenyAll => "deny-all",
        }
    }

    pub fn as_acpx_flag(&self) -> &'static str {
        match self {
            Self::ApproveAll => "--approve-all",
            Self::ApproveReads => "--approve-reads",
            Self::DenyAll => "--deny-all",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "approve-all" => Some(Self::ApproveAll),
            "approve-reads" => Some(Self::ApproveReads),
            "deny-all" => Some(Self::DenyAll),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationBinding {
    pub cwd: PathBuf,
    pub agent: String,
    pub session_name: Option<String>,
    pub permission_mode: PermissionMode,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionSelector {
    pub cwd: PathBuf,
    pub agent: String,
    pub session_name: Option<String>,
    pub permission_mode: PermissionMode,
}

impl SessionSelector {
    pub fn with_session_name(&self, session_name: Option<String>) -> Self {
        Self {
            cwd: self.cwd.clone(),
            agent: self.agent.clone(),
            session_name,
            permission_mode: self.permission_mode.clone(),
        }
    }
}

impl From<&ConversationBinding> for SessionSelector {
    fn from(value: &ConversationBinding) -> Self {
        Self {
            cwd: value.cwd.clone(),
            agent: value.agent.clone(),
            session_name: value.session_name.clone(),
            permission_mode: value.permission_mode.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionSummary {
    pub record_id: String,
    pub name: Option<String>,
    pub created: bool,
    pub acp_session_id: Option<String>,
    pub agent_session_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRecord {
    pub record_id: String,
    pub acp_session_id: String,
    pub agent_session_id: Option<String>,
    pub agent: String,
    pub cwd: PathBuf,
    pub name: Option<String>,
    pub title: Option<String>,
    pub first_user_preview: Option<String>,
    pub created_at: String,
    pub last_used_at: String,
    pub last_prompt_at: Option<String>,
    pub closed: bool,
    pub model: Option<String>,
    pub mode: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionHistoryEntry {
    pub role: String,
    pub timestamp: String,
    pub text_preview: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionStatus {
    pub status: String,
    pub session_id: Option<String>,
    pub agent_command: String,
    pub pid: Option<i64>,
    pub model: Option<String>,
    pub mode: Option<String>,
    pub uptime: Option<String>,
    pub last_prompt_time: Option<String>,
    pub exit_code: Option<i64>,
    pub signal: Option<String>,
    pub agent_session_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentReply {
    pub blocks: Vec<MessageBlock>,
}

impl AgentReply {
    pub fn from_text(text: impl Into<String>) -> Self {
        Self {
            blocks: vec![MessageBlock::text(text)],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellOutput {
    pub command: String,
    pub cwd: PathBuf,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}
