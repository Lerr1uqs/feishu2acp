use crate::{domain::PermissionMode, error::BridgeError, support::normalize_session_name};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UserRequest {
    Prompt(String),
    Command(BotCommand),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BotCommand {
    Help,
    Pwd,
    ChangeDirectory(String),
    Agent(String),
    Permissions(PermissionMode),
    Session(SessionCommand),
    Status,
    Mode(String),
    Model(String),
    Option { key: String, value: String },
    Prompt(String),
    Exec(String),
    Shell(String),
    Cancel,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionCommand {
    New(Option<String>),
    Use(Option<String>),
    Show(Option<String>),
    Close(Option<String>),
    List,
    History(usize),
}

pub fn parse_user_request(prefix: &str, raw: &str) -> Result<UserRequest, BridgeError> {
    validate_user_input(raw)?;
    let trimmed = raw.trim();
    if !trimmed.starts_with(prefix) {
        return Ok(UserRequest::Prompt(trimmed.to_string()));
    }

    let command_body = trimmed[prefix.len()..].trim();
    if command_body.is_empty() {
        return Ok(UserRequest::Command(BotCommand::Help));
    }

    parse_command(command_body)
}

fn validate_user_input(raw: &str) -> Result<(), BridgeError> {
    // Reject control / bidi-override characters early so chat payloads cannot smuggle invisible
    // command changes into `/cd`, `/shell`, or prompt text copied from IM clients.
    if raw.chars().any(is_disallowed_input_char) {
        return Err(BridgeError::InputRejected(
            "输入包含非法控制字符，请移除后重试。".to_string(),
        ));
    }
    Ok(())
}

fn is_disallowed_input_char(ch: char) -> bool {
    if matches!(ch, '\n' | '\r' | '\t') {
        return false;
    }

    ch.is_control()
        || matches!(
            ch,
            '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
        )
}

fn parse_command(raw: &str) -> Result<UserRequest, BridgeError> {
    let (head, rest) = take_token(raw).ok_or_else(|| {
        BridgeError::CommandParse("空命令，请使用 /command help 查看可用指令".to_string())
    })?;

    match head.to_ascii_lowercase().as_str() {
        "help" => Ok(UserRequest::Command(BotCommand::Help)),
        "pwd" => Ok(UserRequest::Command(BotCommand::Pwd)),
        "status" => Ok(UserRequest::Command(BotCommand::Status)),
        "cancel" => Ok(UserRequest::Command(BotCommand::Cancel)),
        "cd" => Ok(UserRequest::Command(BotCommand::ChangeDirectory(
            require_unwrapped_rest("cd 需要目录参数", rest)?,
        ))),
        "agent" => Ok(UserRequest::Command(BotCommand::Agent(require_token(
            "agent 需要 agent 名称",
            rest,
        )?))),
        "permissions" => {
            let value = require_token("permissions 需要权限模式", rest)?;
            let mode = PermissionMode::parse(&value).ok_or_else(|| {
                BridgeError::CommandParse(
                    "权限模式必须是 approve-all / approve-reads / deny-all".to_string(),
                )
            })?;
            Ok(UserRequest::Command(BotCommand::Permissions(mode)))
        }
        "mode" => Ok(UserRequest::Command(BotCommand::Mode(require_token(
            "mode 需要 mode 值",
            rest,
        )?))),
        "model" => Ok(UserRequest::Command(BotCommand::Model(require_token(
            "model 需要模型名",
            rest,
        )?))),
        "set" | "option" => {
            let (key, remainder) = take_token_required("set 需要 key", rest)?;
            let value = require_rest("set 需要 value", remainder)?;
            Ok(UserRequest::Command(BotCommand::Option { key, value }))
        }
        "prompt" => Ok(UserRequest::Command(BotCommand::Prompt(require_rest(
            "prompt 需要文本",
            rest,
        )?))),
        "exec" => Ok(UserRequest::Command(BotCommand::Exec(require_rest(
            "exec 需要文本",
            rest,
        )?))),
        "shell" => Ok(UserRequest::Command(BotCommand::Shell(require_rest(
            "shell 需要系统命令",
            rest,
        )?))),
        "session" => parse_session_command(rest),
        other => Err(BridgeError::CommandParse(format!(
            "未知命令：{other}，请使用 /help"
        ))),
    }
}

fn parse_session_command(rest: &str) -> Result<UserRequest, BridgeError> {
    let (action, tail) = take_token(rest).ok_or_else(|| {
        BridgeError::CommandParse(
            "session 需要子命令，例如 new/use/show/close/list/history".to_string(),
        )
    })?;

    let command = match action.to_ascii_lowercase().as_str() {
        "new" => SessionCommand::New(normalize_session_name(optional_rest(tail))),
        "use" => SessionCommand::Use(normalize_session_name(optional_rest(tail))),
        "show" => SessionCommand::Show(normalize_session_name(optional_rest(tail))),
        "close" => SessionCommand::Close(normalize_session_name(optional_rest(tail))),
        "list" => SessionCommand::List,
        "history" => {
            let limit = optional_rest(tail)
                .map(|raw| raw.parse::<usize>())
                .transpose()
                .map_err(|_| BridgeError::CommandParse("history limit 必须是正整数".to_string()))?
                .unwrap_or(10);
            SessionCommand::History(limit)
        }
        other => {
            return Err(BridgeError::CommandParse(format!(
                "未知 session 子命令：{other}"
            )));
        }
    };

    Ok(UserRequest::Command(BotCommand::Session(command)))
}

fn take_token_required<'a>(message: &str, rest: &'a str) -> Result<(String, &'a str), BridgeError> {
    take_token(rest).ok_or_else(|| BridgeError::CommandParse(message.to_string()))
}

fn require_token(message: &str, rest: &str) -> Result<String, BridgeError> {
    take_token(rest)
        .map(|(token, _)| token)
        .ok_or_else(|| BridgeError::CommandParse(message.to_string()))
}

fn require_rest(message: &str, rest: &str) -> Result<String, BridgeError> {
    optional_rest(rest).ok_or_else(|| BridgeError::CommandParse(message.to_string()))
}

fn require_unwrapped_rest(message: &str, rest: &str) -> Result<String, BridgeError> {
    let value = require_rest(message, rest)?;
    Ok(strip_matching_quotes(&value))
}

fn optional_rest(rest: &str) -> Option<String> {
    let trimmed = rest.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn strip_matching_quotes(value: &str) -> String {
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        if (bytes[0] == b'"' && bytes[value.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[value.len() - 1] == b'\'')
        {
            return value[1..value.len() - 1].to_string();
        }
    }
    value.to_string()
}

fn take_token(input: &str) -> Option<(String, &str)> {
    let trimmed = input.trim_start();
    if trimmed.is_empty() {
        return None;
    }

    let mut chars = trimmed.char_indices();
    let (_, first_char) = chars.next()?;

    if first_char == '"' || first_char == '\'' {
        let quote = first_char;
        for (index, ch) in chars {
            if ch == quote {
                let token = trimmed[1..index].to_string();
                let remainder = &trimmed[index + ch.len_utf8()..];
                return Some((token, remainder));
            }
        }

        return Some((trimmed[1..].to_string(), ""));
    }

    for (index, ch) in trimmed.char_indices() {
        if ch.is_whitespace() {
            return Some((trimmed[..index].to_string(), &trimmed[index..]));
        }
    }

    Some((trimmed.to_string(), ""))
}

#[cfg(test)]
mod tests {
    use crate::domain::PermissionMode;

    use super::{BotCommand, SessionCommand, UserRequest, parse_user_request};

    #[test]
    fn regular_message_becomes_prompt() {
        let parsed = parse_user_request("/command", "修复一下测试").unwrap();
        assert_eq!(parsed, UserRequest::Prompt("修复一下测试".to_string()));
    }

    #[test]
    fn bare_command_returns_help() {
        let parsed = parse_user_request("/command", "/command").unwrap();
        assert_eq!(parsed, UserRequest::Command(BotCommand::Help));
    }

    #[test]
    fn slash_prefix_returns_help_for_single_slash() {
        let parsed = parse_user_request("/", "/").unwrap();
        assert_eq!(parsed, UserRequest::Command(BotCommand::Help));
    }

    #[test]
    fn slash_prefix_parses_cd_command() {
        let parsed = parse_user_request("/", "/cd /tmp/worktree").unwrap();
        assert_eq!(
            parsed,
            UserRequest::Command(BotCommand::ChangeDirectory("/tmp/worktree".to_string()))
        );
    }

    #[test]
    fn parses_cd_command() {
        let parsed =
            parse_user_request("/command", r#"/command cd "F:\coding workspace""#).unwrap();
        assert_eq!(
            parsed,
            UserRequest::Command(BotCommand::ChangeDirectory(
                r#"F:\coding workspace"#.to_string()
            ))
        );
    }

    #[test]
    fn parses_permission_command() {
        let parsed = parse_user_request("/command", "/command permissions approve-all").unwrap();
        assert_eq!(
            parsed,
            UserRequest::Command(BotCommand::Permissions(PermissionMode::ApproveAll))
        );
    }

    #[test]
    fn parses_session_commands() {
        let parsed = parse_user_request("/command", "/command session new backend").unwrap();
        assert_eq!(
            parsed,
            UserRequest::Command(BotCommand::Session(SessionCommand::New(Some(
                "backend".to_string()
            ))))
        );

        let parsed = parse_user_request("/command", "/command session use default").unwrap();
        assert_eq!(
            parsed,
            UserRequest::Command(BotCommand::Session(SessionCommand::Use(None)))
        );
    }

    #[test]
    fn parses_shell_and_exec_rest_without_retokenizing() {
        let shell =
            parse_user_request("/command", "/command shell cargo test -- --nocapture").unwrap();
        assert_eq!(
            shell,
            UserRequest::Command(BotCommand::Shell("cargo test -- --nocapture".to_string()))
        );

        let exec = parse_user_request("/command", "/command exec 检查这个仓库并总结").unwrap();
        assert_eq!(
            exec,
            UserRequest::Command(BotCommand::Exec("检查这个仓库并总结".to_string()))
        );
    }

    #[test]
    fn rejects_unknown_command() {
        let error = parse_user_request("/command", "/command nope").unwrap_err();
        assert!(error.user_message().contains("命令解析失败"));
    }

    #[test]
    fn rejects_control_characters_before_parsing() {
        let error = parse_user_request("/", "/cd /tmp/\u{001b}[31mboom").unwrap_err();
        assert_eq!(
            error.user_message(),
            "输入包含非法控制字符，请移除后重试。"
        );
    }

    #[test]
    fn rejects_bidi_override_characters_before_parsing() {
        let error = parse_user_request("/", "/cd /tmp/\u{202e}txt.exe").unwrap_err();
        assert_eq!(
            error.user_message(),
            "输入包含非法控制字符，请移除后重试。"
        );
    }
}
