use std::path::Path;

use crate::domain::{
    ConversationBinding, PromptResponse, SessionHistoryEntry, SessionRecord, SessionStatus,
    SessionSummary, ShellOutput,
};

pub fn help_text(prefix: &str) -> String {
    format!(
        concat!(
            "可用命令：\n",
            "{prefix} pwd\n",
            "{prefix} cd <dir>\n",
            "{prefix} agent <agent>\n",
            "{prefix} permissions <approve-all|approve-reads|deny-all>\n",
            "{prefix} session new [name]\n",
            "{prefix} session use [name|default]\n",
            "{prefix} session show [name]\n",
            "{prefix} session close [name]\n",
            "{prefix} session list\n",
            "{prefix} session history [limit]\n",
            "{prefix} status\n",
            "{prefix} mode <mode>\n",
            "{prefix} model <model>\n",
            "{prefix} set <key> <value>\n",
            "{prefix} exec <text>\n",
            "{prefix} shell <command>\n",
            "{prefix} cancel\n",
            "\n",
            "普通文本消息会直接发送给当前 Codex 会话。"
        ),
        prefix = prefix
    )
}

pub fn binding_text(binding: &ConversationBinding) -> String {
    format!(
        "当前上下文\ncwd: {}\nagent: {}\nsession: {}\npermissions: {}",
        binding.cwd.display(),
        binding.agent,
        binding.session_name.as_deref().unwrap_or("default"),
        binding.permission_mode.as_str()
    )
}

pub fn session_summary_text(summary: &SessionSummary, action: &str) -> String {
    format!(
        "{}\nrecord: {}\nsession: {}",
        action,
        summary.record_id,
        summary.name.as_deref().unwrap_or("default")
    )
}

pub fn session_record_text(record: &SessionRecord) -> String {
    format!(
        concat!(
            "session detail\n",
            "record: {}\n",
            "acp_session: {}\n",
            "agent_session: {}\n",
            "agent: {}\n",
            "cwd: {}\n",
            "name: {}\n",
            "created_at: {}\n",
            "last_used_at: {}\n",
            "last_prompt_at: {}\n",
            "closed: {}\n",
            "model: {}\n",
            "mode: {}"
        ),
        record.record_id,
        record.acp_session_id,
        record.agent_session_id.as_deref().unwrap_or("-"),
        record.agent,
        record.cwd.display(),
        record.name.as_deref().unwrap_or("default"),
        record.created_at,
        record.last_used_at,
        record.last_prompt_at.as_deref().unwrap_or("-"),
        if record.closed { "yes" } else { "no" },
        record.model.as_deref().unwrap_or("-"),
        record.mode.as_deref().unwrap_or("-")
    )
}

pub fn session_list_text(records: &[SessionRecord], current_cwd: &Path) -> String {
    if records.is_empty() {
        return "当前 agent 没有任何 acpx session。".to_string();
    }

    let mut lines = vec!["sessions".to_string()];
    for record in records {
        let marker = if record.cwd == current_cwd { "*" } else { "-" };
        lines.push(format!(
            "{marker} {} | {} | {} | {}",
            record.name.as_deref().unwrap_or("default"),
            record.record_id,
            record.cwd.display(),
            if record.closed { "closed" } else { "open" }
        ));
    }
    lines.join("\n")
}

pub fn history_text(entries: &[SessionHistoryEntry]) -> String {
    if entries.is_empty() {
        return "当前 session 没有历史记录。".to_string();
    }

    let mut lines = vec!["session history".to_string()];
    for entry in entries {
        lines.push(format!(
            "{} | {} | {}",
            entry.timestamp, entry.role, entry.text_preview
        ));
    }
    lines.join("\n")
}

pub fn status_text(status: &SessionStatus) -> String {
    format!(
        concat!(
            "status\n",
            "session: {}\n",
            "agent: {}\n",
            "pid: {}\n",
            "status: {}\n",
            "model: {}\n",
            "mode: {}\n",
            "uptime: {}\n",
            "last_prompt_time: {}\n",
            "exit_code: {}\n",
            "signal: {}"
        ),
        status.session_id.as_deref().unwrap_or("-"),
        status.agent_command,
        status
            .pid
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string()),
        status.status,
        status.model.as_deref().unwrap_or("-"),
        status.mode.as_deref().unwrap_or("-"),
        status.uptime.as_deref().unwrap_or("-"),
        status.last_prompt_time.as_deref().unwrap_or("-"),
        status
            .exit_code
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string()),
        status.signal.as_deref().unwrap_or("-")
    )
}

pub fn prompt_text(response: &PromptResponse) -> String {
    if response.text.trim().is_empty() {
        "Codex 已完成，但没有返回可显示的文本。".to_string()
    } else {
        response.text.trim().to_string()
    }
}

pub fn shell_output_text(output: &ShellOutput) -> String {
    let stdout = if output.stdout.trim().is_empty() {
        "[empty]".to_string()
    } else {
        output.stdout.trim().to_string()
    };
    let stderr = if output.stderr.trim().is_empty() {
        "[empty]".to_string()
    } else {
        output.stderr.trim().to_string()
    };

    format!(
        concat!(
            "shell result\n",
            "cwd: {}\n",
            "command: {}\n",
            "exit_code: {}\n",
            "stdout:\n{}\n",
            "stderr:\n{}"
        ),
        output.cwd.display(),
        output.command,
        output.exit_code,
        stdout,
        stderr
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::domain::{
        ConversationBinding, PermissionMode, PromptResponse, SessionHistoryEntry, SessionRecord,
        SessionStatus, SessionSummary, ShellOutput,
    };

    use super::{
        binding_text, history_text, prompt_text, session_list_text, session_record_text,
        session_summary_text, shell_output_text, status_text,
    };

    #[test]
    fn renders_binding() {
        let binding = ConversationBinding {
            cwd: PathBuf::from("/repo"),
            agent: "codex".to_string(),
            session_name: Some("backend".to_string()),
            permission_mode: PermissionMode::ApproveReads,
        };
        let rendered = binding_text(&binding);
        assert!(rendered.contains("/repo"));
        assert!(rendered.contains("backend"));
    }

    #[test]
    fn renders_session_summary() {
        let summary = SessionSummary {
            record_id: "rec-1".to_string(),
            name: Some("docs".to_string()),
            created: true,
            acp_session_id: None,
            agent_session_id: None,
        };
        assert!(session_summary_text(&summary, "session ready").contains("rec-1"));
    }

    #[test]
    fn renders_session_record_and_status() {
        let record = SessionRecord {
            record_id: "rec-1".to_string(),
            acp_session_id: "acp-1".to_string(),
            agent_session_id: Some("agent-1".to_string()),
            agent: "codex".to_string(),
            cwd: PathBuf::from("/repo"),
            name: Some("docs".to_string()),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            last_used_at: "2026-01-01T00:01:00Z".to_string(),
            last_prompt_at: Some("2026-01-01T00:01:00Z".to_string()),
            closed: false,
            model: Some("gpt-5.4".to_string()),
            mode: Some("auto".to_string()),
        };
        let status = SessionStatus {
            status: "running".to_string(),
            session_id: Some("rec-1".to_string()),
            agent_command: "codex".to_string(),
            pid: Some(123),
            model: Some("gpt-5.4".to_string()),
            mode: Some("auto".to_string()),
            uptime: Some("00:00:10".to_string()),
            last_prompt_time: Some("2026-01-01T00:01:00Z".to_string()),
            exit_code: None,
            signal: None,
            agent_session_id: None,
        };

        assert!(session_record_text(&record).contains("acp-1"));
        assert!(status_text(&status).contains("running"));
    }

    #[test]
    fn renders_lists_history_and_shell_outputs() {
        let records = vec![SessionRecord {
            record_id: "rec-1".to_string(),
            acp_session_id: "acp-1".to_string(),
            agent_session_id: None,
            agent: "codex".to_string(),
            cwd: PathBuf::from("/repo"),
            name: None,
            created_at: "2026".to_string(),
            last_used_at: "2026".to_string(),
            last_prompt_at: None,
            closed: false,
            model: None,
            mode: None,
        }];
        let history = vec![SessionHistoryEntry {
            role: "assistant".to_string(),
            timestamp: "2026".to_string(),
            text_preview: "done".to_string(),
        }];
        let shell = ShellOutput {
            command: "pwd".to_string(),
            cwd: PathBuf::from("/repo"),
            exit_code: 0,
            stdout: "/repo".to_string(),
            stderr: String::new(),
        };

        assert!(session_list_text(&records, std::path::Path::new("/repo")).contains("*"));
        assert!(history_text(&history).contains("done"));
        assert!(shell_output_text(&shell).contains("exit_code: 0"));
    }

    #[test]
    fn prompt_text_uses_fallback_for_empty_output() {
        let empty = PromptResponse {
            text: "   ".to_string(),
        };
        let rich = PromptResponse {
            text: "hello".to_string(),
        };

        assert!(prompt_text(&empty).contains("没有返回"));
        assert_eq!(prompt_text(&rich), "hello");
    }
}
