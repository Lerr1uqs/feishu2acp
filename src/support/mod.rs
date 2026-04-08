use std::{
    env,
    path::{Path, PathBuf},
};

use path_clean::PathClean;

use crate::error::BridgeError;

pub fn parse_argument_list(value: &str) -> Result<Vec<String>, BridgeError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }

    if trimmed.starts_with('[') {
        let parsed: Vec<String> = serde_json::from_str(trimmed)
            .map_err(|error| BridgeError::Config(format!("invalid JSON argument list: {error}")))?;
        return Ok(parsed);
    }

    if let Some(tokens) = shlex::split(trimmed) {
        return Ok(tokens);
    }

    Ok(trimmed
        .split_whitespace()
        .map(ToString::to_string)
        .collect::<Vec<_>>())
}

pub fn default_data_dir() -> PathBuf {
    if cfg!(windows) {
        if let Ok(appdata) = env::var("APPDATA") {
            return PathBuf::from(appdata).join("feishu2acp");
        }
        if let Ok(user_profile) = env::var("USERPROFILE") {
            return PathBuf::from(user_profile)
                .join("AppData")
                .join("Roaming")
                .join("feishu2acp");
        }
    }

    env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".feishu2acp")
}

pub fn chunk_text(text: &str, max_chars: usize) -> Vec<String> {
    if max_chars == 0 {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut current = String::new();

    for line in text.lines() {
        let line_len = line.chars().count();
        let current_len = current.chars().count();
        let separator = if current.is_empty() { 0 } else { 1 };

        if current_len + separator + line_len > max_chars && !current.is_empty() {
            chunks.push(current.clone());
            current.clear();
        }

        if line_len > max_chars {
            for piece in slice_chars(line, max_chars) {
                if !current.is_empty() {
                    chunks.push(current.clone());
                    current.clear();
                }
                chunks.push(piece);
            }
            continue;
        }

        if !current.is_empty() {
            current.push('\n');
        }
        current.push_str(line);
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    if chunks.is_empty() {
        chunks.push(String::new());
    }

    chunks
}

pub fn finalize_chunk_labels(chunks: Vec<String>) -> Vec<String> {
    let total = chunks.len();
    if total <= 1 {
        return chunks;
    }

    chunks
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| format!("[{}/{}]\n{}", index + 1, total, chunk))
        .collect()
}

fn slice_chars(value: &str, max_chars: usize) -> Vec<String> {
    let mut items = Vec::new();
    let mut current = String::new();

    for ch in value.chars() {
        if current.chars().count() >= max_chars {
            items.push(current);
            current = String::new();
        }
        current.push(ch);
    }

    if !current.is_empty() {
        items.push(current);
    }

    items
}

pub fn resolve_workspace(base: &Path, raw: &str) -> PathBuf {
    let candidate = PathBuf::from(raw);
    let absolute = if candidate.is_absolute() {
        candidate
    } else {
        base.join(candidate)
    };
    absolute.clean()
}

pub fn normalize_session_name(value: Option<String>) -> Option<String> {
    match value {
        None => None,
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("default") || trimmed == "-" {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
    }
}

pub fn text_preview(value: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }

    let normalized = value.replace('\n', "\\n");
    let mut preview = normalized.chars().take(max_chars).collect::<String>();
    if normalized.chars().count() > max_chars {
        preview.push_str("...");
    }
    preview
}

#[cfg(test)]
mod tests {
    use super::{
        chunk_text, finalize_chunk_labels, normalize_session_name, parse_argument_list,
        text_preview,
    };

    #[test]
    fn parse_argument_list_supports_json_array() {
        let parsed = parse_argument_list(r#"["acpx@latest","--yes"]"#).unwrap();
        assert_eq!(parsed, vec!["acpx@latest", "--yes"]);
    }

    #[test]
    fn parse_argument_list_supports_shell_words() {
        let parsed = parse_argument_list(r#"--foo "hello world""#).unwrap();
        assert_eq!(parsed, vec!["--foo", "hello world"]);
    }

    #[test]
    fn chunk_text_labels_multiple_chunks() {
        let chunks = finalize_chunk_labels(chunk_text("a\nb\nc", 1));
        assert_eq!(chunks, vec!["[1/3]\na", "[2/3]\nb", "[3/3]\nc"]);
    }

    #[test]
    fn normalize_session_name_maps_default_tokens_to_none() {
        assert_eq!(normalize_session_name(Some("default".to_string())), None);
        assert_eq!(normalize_session_name(Some("-".to_string())), None);
        assert_eq!(
            normalize_session_name(Some("backend".to_string())),
            Some("backend".to_string())
        );
    }

    #[test]
    fn text_preview_truncates_and_normalizes_newlines() {
        assert_eq!(text_preview("hello\nworld", 7), "hello\\n...");
        assert_eq!(text_preview("short", 20), "short");
    }
}
