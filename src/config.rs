use std::{env, path::PathBuf};

use crate::{
    domain::PermissionMode,
    error::BridgeError,
    support::{default_data_dir, parse_argument_list},
};

#[derive(Clone, Debug)]
pub struct FeishuConfig {
    pub app_id: String,
    pub app_secret: String,
    pub typing_reaction_emoji: Option<String>,
    pub media_dir: PathBuf,
    pub max_markdown_bytes: usize,
    pub enable_markdown_input: bool,
    pub enable_markdown_output: bool,
}

#[derive(Clone, Debug)]
pub struct AcpxCliConfig {
    pub program: String,
    pub args: Vec<String>,
    pub timeout_secs: u64,
    pub ttl_secs: u64,
}

#[derive(Clone, Debug)]
pub struct ShellConfig {
    pub program: String,
    pub args: Vec<String>,
    pub timeout_secs: u64,
}

#[derive(Clone, Debug)]
pub struct AppConfig {
    pub feishu: FeishuConfig,
    pub acpx: AcpxCliConfig,
    pub shell: ShellConfig,
    pub command_prefix: String,
    pub default_workspace: PathBuf,
    pub default_agent: String,
    pub default_permission_mode: PermissionMode,
    pub reply_chunk_chars: usize,
    pub conversation_store_path: PathBuf,
    pub log_path: PathBuf,
    pub tracing_filter: String,
}

impl AppConfig {
    pub fn from_env() -> Result<Self, BridgeError> {
        let default_workspace = env::current_dir().map_err(|error| {
            BridgeError::Config(format!("cannot read current directory: {error}"))
        })?;

        let default_permission_mode = env::var("FEISHU2ACP_PERMISSION_MODE")
            .ok()
            .and_then(|value| PermissionMode::parse(&value))
            .unwrap_or(PermissionMode::ApproveReads);

        let reply_chunk_chars = env::var("FEISHU2ACP_REPLY_CHUNK_CHARS")
            .ok()
            .map(|value| {
                value.parse::<usize>().map_err(|error| {
                    BridgeError::Config(format!(
                        "FEISHU2ACP_REPLY_CHUNK_CHARS must be a positive integer: {error}"
                    ))
                })
            })
            .transpose()?
            .unwrap_or(3000);

        let conversation_store_path = env::var("FEISHU2ACP_STATE_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| default_data_dir().join("conversations.json"));
        let log_path = env::var("FEISHU2ACP_LOG_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| default_workspace.join(".logs").join("feishu2acp.log"));

        let acpx_program = env::var("ACPX_PROGRAM").unwrap_or_else(|_| "acpx".to_string());
        let acpx_args = match env::var("ACPX_PROGRAM_ARGS") {
            Ok(value) => parse_argument_list(&value)?,
            Err(_) => Vec::new(),
        };

        let shell_program = default_shell_program().to_string();
        let shell_args = default_shell_args();

        Ok(Self {
            feishu: FeishuConfig {
                app_id: required_env("FEISHU_APP_ID")?,
                app_secret: required_env("FEISHU_APP_SECRET")?,
                // Feishu does not expose a lightweight typing callback in this bridge, so we use
                // a configurable message reaction as the immediate "working on it" signal.
                typing_reaction_emoji: typing_reaction_emoji_from_env(),
                media_dir: env::var("FEISHU2ACP_MEDIA_DIR")
                    .map(PathBuf::from)
                    .unwrap_or_else(|_| default_data_dir().join("media")),
                max_markdown_bytes: parse_usize_env("FEISHU2ACP_MAX_MARKDOWN_BYTES", 1024 * 1024)?,
                enable_markdown_input: parse_bool_env("FEISHU2ACP_ENABLE_MARKDOWN_INPUT", true)?,
                enable_markdown_output: parse_bool_env("FEISHU2ACP_ENABLE_MARKDOWN_OUTPUT", true)?,
            },
            acpx: AcpxCliConfig {
                program: acpx_program,
                args: acpx_args,
                timeout_secs: parse_u64_env("ACPX_TIMEOUT_SECS", 1800)?,
                ttl_secs: parse_u64_env("ACPX_TTL_SECS", 300)?,
            },
            shell: ShellConfig {
                program: shell_program,
                args: shell_args,
                timeout_secs: 300,
            },
            command_prefix: "/".to_string(),
            default_workspace,
            default_agent: "codex".to_string(),
            default_permission_mode,
            reply_chunk_chars,
            conversation_store_path,
            log_path,
            tracing_filter: env::var("RUST_LOG")
                // Default to verbose bridge logs while keeping the Feishu SDK at info level.
                .unwrap_or_else(|_| "info,open_lark=info,feishu2acp=debug".to_string()),
        })
    }
}

fn required_env(name: &str) -> Result<String, BridgeError> {
    env::var(name)
        .map_err(|_| BridgeError::Config(format!("missing required environment variable {name}")))
}

fn typing_reaction_emoji_from_env() -> Option<String> {
    match env::var("FEISHU2ACP_TYPING_EMOJI") {
        Ok(value) => {
            let trimmed = value.trim();
            if trimmed.is_empty() || trimmed == "-" {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        Err(_) => Some("HOURGLASS".to_string()),
    }
}

fn parse_u64_env(name: &str, default: u64) -> Result<u64, BridgeError> {
    env::var(name)
        .ok()
        .map(|value| {
            value.parse::<u64>().map_err(|error| {
                BridgeError::Config(format!("{name} must be a non-negative integer: {error}"))
            })
        })
        .transpose()?
        .map_or(Ok(default), Ok)
}

fn parse_usize_env(name: &str, default: usize) -> Result<usize, BridgeError> {
    env::var(name)
        .ok()
        .map(|value| {
            value.parse::<usize>().map_err(|error| {
                BridgeError::Config(format!("{name} must be a non-negative integer: {error}"))
            })
        })
        .transpose()?
        .map_or(Ok(default), Ok)
}

fn parse_bool_env(name: &str, default: bool) -> Result<bool, BridgeError> {
    env::var(name)
        .ok()
        .map(|value| match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Ok(true),
            "0" | "false" | "no" | "off" => Ok(false),
            _ => Err(BridgeError::Config(format!(
                "{name} must be one of true/false/1/0/yes/no/on/off"
            ))),
        })
        .transpose()?
        .map_or(Ok(default), Ok)
}

fn default_shell_program() -> &'static str {
    if cfg!(windows) { "powershell" } else { "sh" }
}

fn default_shell_args() -> Vec<String> {
    if cfg!(windows) {
        vec!["-NoProfile".to_string(), "-Command".to_string()]
    } else {
        vec!["-lc".to_string()]
    }
}

#[cfg(test)]
mod tests {
    use std::{env, sync::{Mutex, OnceLock}};

    use super::{AppConfig, default_shell_args};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn default_shell_args_match_platform() {
        if cfg!(windows) {
            assert_eq!(default_shell_args(), vec!["-NoProfile", "-Command"]);
        } else {
            assert_eq!(default_shell_args(), vec!["-lc"]);
        }
    }

    #[test]
    fn config_parses_basic_env() {
        let _guard = env_lock().lock().unwrap();
        unsafe {
            env::set_var("FEISHU_APP_ID", "app");
            env::set_var("FEISHU_APP_SECRET", "secret");
            env::set_var("ACPX_PROGRAM_ARGS", r#"["acpx@latest"]"#);
            env::remove_var("FEISHU2ACP_TYPING_EMOJI");
            env::remove_var("RUST_LOG");
        }
        let config = AppConfig::from_env().unwrap();
        assert_eq!(config.feishu.app_id, "app");
        assert_eq!(config.acpx.args, vec!["acpx@latest"]);
        assert_eq!(config.feishu.typing_reaction_emoji.as_deref(), Some("HOURGLASS"));
        assert!(config.feishu.enable_markdown_input);
        assert!(config.feishu.enable_markdown_output);
        assert_eq!(config.feishu.max_markdown_bytes, 1024 * 1024);
        assert_eq!(config.command_prefix, "/");
        assert_eq!(config.default_agent, "codex");
        assert_eq!(
            config.log_path,
            std::env::current_dir()
                .unwrap()
                .join(".logs")
                .join("feishu2acp.log")
        );
        assert_eq!(
            config.tracing_filter,
            "info,open_lark=info,feishu2acp=debug"
        );
    }

    #[test]
    fn config_allows_disabling_typing_reaction() {
        let _guard = env_lock().lock().unwrap();
        unsafe {
            env::set_var("FEISHU_APP_ID", "app");
            env::set_var("FEISHU_APP_SECRET", "secret");
            env::set_var("FEISHU2ACP_TYPING_EMOJI", "-");
        }
        let config = AppConfig::from_env().unwrap();
        assert_eq!(config.feishu.typing_reaction_emoji, None);
    }
}
