use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum BridgeError {
    #[error("configuration error: {0}")]
    Config(String),
    #[error("input rejected: {0}")]
    InputRejected(String),
    #[error("command parse error: {0}")]
    CommandParse(String),
    #[error("workspace does not exist: {0}")]
    WorkspaceNotFound(PathBuf),
    #[error("path is not a directory: {0}")]
    NotDirectory(PathBuf),
    #[error("unsupported message type")]
    UnsupportedMessage,
    #[error("process failed: {command} (exit code {exit_code})")]
    ProcessFailed {
        command: String,
        exit_code: i32,
        stdout: String,
        stderr: String,
    },
    #[error("process timed out: {command}")]
    ProcessTimedOut { command: String },
    #[error("invalid process output for {context}: {message}")]
    InvalidOutput {
        context: String,
        message: String,
        raw_output: String,
    },
    #[error("channel error: {0}")]
    Channel(String),
    #[error("acpx error: {0}")]
    Acpx(String),
    #[error("shell error: {0}")]
    Shell(String),
    #[error("persistence error: {0}")]
    Persistence(String),
}

impl BridgeError {
    pub fn user_message(&self) -> String {
        match self {
            Self::InputRejected(message) => message.clone(),
            Self::CommandParse(message) => format!("命令解析失败：{message}"),
            Self::WorkspaceNotFound(path) => {
                format!("目录不存在：{}", path.display())
            }
            Self::NotDirectory(path) => {
                format!("目标不是目录：{}", path.display())
            }
            Self::UnsupportedMessage => "当前消息类型暂不支持。".to_string(),
            Self::ProcessFailed {
                command,
                exit_code,
                stdout,
                stderr,
            } => {
                let detail = first_non_empty(stderr, stdout)
                    .unwrap_or("外部命令执行失败")
                    .trim()
                    .to_string();
                format!("命令失败（{exit_code}）：{command}\n{detail}")
            }
            Self::ProcessTimedOut { command } => format!("命令执行超时：{command}"),
            Self::InvalidOutput {
                context, message, ..
            } => format!("外部输出无法解析（{context}）：{message}"),
            Self::Config(message)
            | Self::Channel(message)
            | Self::Acpx(message)
            | Self::Shell(message)
            | Self::Persistence(message) => message.clone(),
        }
    }
}

fn first_non_empty<'a>(primary: &'a str, secondary: &'a str) -> Option<&'a str> {
    if !primary.trim().is_empty() {
        return Some(primary);
    }
    if !secondary.trim().is_empty() {
        return Some(secondary);
    }
    None
}
