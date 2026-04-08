use crate::domain::{
    ConversationBinding, SessionHistoryEntry, SessionRecord, SessionStatus, SessionSummary,
    ShellOutput,
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
            "title: {}\n",
            "first_user: {}\n",
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
        record.title.as_deref().unwrap_or("-"),
        record.first_user_preview.as_deref().unwrap_or("-"),
        record.created_at,
        record.last_used_at,
        record.last_prompt_at.as_deref().unwrap_or("-"),
        if record.closed { "yes" } else { "no" },
        record.model.as_deref().unwrap_or("-"),
        record.mode.as_deref().unwrap_or("-")
    )
}

pub fn session_list_text(records: &[SessionRecord], current: &ConversationBinding) -> String {
    if records.is_empty() {
        return "当前 agent 没有任何 acpx session。".to_string();
    }

    let current_index = current_session_index(records, current);
    let mut lines = vec!["sessions".to_string()];
    for (index, record) in records.iter().enumerate() {
        let marker = if Some(index) == current_index { "*" } else { "-" };
        lines.push(format!(
            "{marker} {} | {} | {} | {}",
            record.name.as_deref().unwrap_or("default"),
            record.record_id,
            record.cwd.display(),
            if record.closed { "closed" } else { "open" }
        ));
        if let Some((source, summary)) = session_record_summary(record) {
            lines.push(format!("  [{source}] {summary}"));
        }
    }
    lines.join("\n")
}

fn current_session_index(records: &[SessionRecord], current: &ConversationBinding) -> Option<usize> {
    records
        .iter()
        .enumerate()
        .filter(|(_, record)| {
            record.cwd == current.cwd && record.name.as_deref() == current.session_name.as_deref()
        })
        // The binding only stores cwd + session name, so if ACPX keeps historical duplicates we
        // mark the newest matching record and prefer an open one over an older closed record.
        .max_by_key(|(_, record)| (u8::from(!record.closed), record.last_used_at.as_str()))
        .map(|(index, _)| index)
}

fn session_record_summary(record: &SessionRecord) -> Option<(&'static str, String)> {
    let (source, raw) = if let Some(title) = record.title.as_deref() {
        ("title", title)
    } else {
        ("first", record.first_user_preview.as_deref()?)
    };
    let compact = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.is_empty() {
        return None;
    }

    const MAX_SUMMARY_CHARS: usize = 72;
    let mut summary = compact.chars().take(MAX_SUMMARY_CHARS).collect::<String>();
    if compact.chars().count() > MAX_SUMMARY_CHARS {
        summary.push_str("...");
    }
    Some((source, summary))
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
        ConversationBinding, PermissionMode, SessionHistoryEntry, SessionRecord, SessionStatus,
        SessionSummary, ShellOutput,
    };

    use super::{
        binding_text, history_text, session_list_text, session_record_text, session_summary_text,
        shell_output_text, status_text,
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
            title: Some("Fix reply routing".to_string()),
            first_user_preview: Some("修一下 feishu reply body".to_string()),
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
        assert!(session_record_text(&record).contains("Fix reply routing"));
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
            title: None,
            first_user_preview: Some("修一下飞书回复的 body 参数问题".to_string()),
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
        let current = ConversationBinding {
            cwd: PathBuf::from("/repo"),
            agent: "codex".to_string(),
            session_name: None,
            permission_mode: PermissionMode::ApproveReads,
        };

        let rendered = session_list_text(&records, &current);
        assert!(rendered.contains("*"));
        assert!(rendered.contains("[first] 修一下飞书回复的 body 参数问题"));
        assert!(history_text(&history).contains("done"));
        assert!(shell_output_text(&shell).contains("exit_code: 0"));
    }

    #[test]
    fn session_list_marks_only_latest_matching_record_and_prefers_title() {
        let records = vec![
            SessionRecord {
                record_id: "rec-old".to_string(),
                acp_session_id: "acp-old".to_string(),
                agent_session_id: None,
                agent: "codex".to_string(),
                cwd: PathBuf::from("/repo"),
                name: None,
                title: Some("old closed session".to_string()),
                first_user_preview: Some("older".to_string()),
                created_at: "2026".to_string(),
                last_used_at: "2026-01-01T00:00:00Z".to_string(),
                last_prompt_at: None,
                closed: true,
                model: None,
                mode: None,
            },
            SessionRecord {
                record_id: "rec-current".to_string(),
                acp_session_id: "acp-current".to_string(),
                agent_session_id: None,
                agent: "codex".to_string(),
                cwd: PathBuf::from("/repo"),
                name: None,
                title: Some("current open session".to_string()),
                first_user_preview: Some("newer".to_string()),
                created_at: "2026".to_string(),
                last_used_at: "2026-01-01T00:02:00Z".to_string(),
                last_prompt_at: None,
                closed: false,
                model: None,
                mode: None,
            },
        ];
        let current = ConversationBinding {
            cwd: PathBuf::from("/repo"),
            agent: "codex".to_string(),
            session_name: None,
            permission_mode: PermissionMode::ApproveReads,
        };

        let rendered = session_list_text(&records, &current);
        assert_eq!(rendered.matches('*').count(), 1);
        assert!(rendered.contains("* default | rec-current"));
        assert!(rendered.contains("[title] current open session"));
    }
}
