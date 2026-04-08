use std::{env, fs, path::Path};

use anyhow::{Context, bail};
use feishu2acp::{
    adapters::feishu::{FeishuMediaHttpClient, build_lark_client},
    config::AppConfig,
};
use open_lark::prelude::*;
use serde_json::json;

const DEFAULT_CHAT_ID: &str = "oc_f5f9c8e4001155b3d3fd395426388ce4";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    let config = AppConfig::from_env()?;
    let client = build_lark_client(&config.feishu);
    let media_http = FeishuMediaHttpClient::new(&config.feishu);
    let args = parse_args(env::args().skip(1))?;
    let payload = build_payload(args.file.as_deref())?;
    let upload = media_http
        .upload_markdown_file(
            &markdown_transport_file_name(&payload.file_name),
            payload.bytes,
        )
        .await
        .with_context(|| format!("failed to upload {}", payload.file_name))?;

    send_chat_message(
        &client,
        &args.chat_id,
        "file",
        json!({ "file_key": upload }).to_string(),
    )
    .await?;

    if let Some(note) = args.note {
        send_chat_message(
            &client,
            &args.chat_id,
            "text",
            json!({ "text": note }).to_string(),
        )
        .await?;
    }

    println!(
        "sent markdown file `{}` to chat `{}`",
        payload.file_name, args.chat_id
    );
    Ok(())
}

struct Args {
    chat_id: String,
    file: Option<String>,
    note: Option<String>,
}

struct MarkdownPayload {
    file_name: String,
    bytes: Vec<u8>,
}

fn parse_args<I>(mut args: I) -> anyhow::Result<Args>
where
    I: Iterator<Item = String>,
{
    let mut parsed = Args {
        chat_id: DEFAULT_CHAT_ID.to_string(),
        file: None,
        note: None,
    };

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--chat" => parsed.chat_id = next_arg(&mut args, "--chat")?,
            "--file" => parsed.file = Some(next_arg(&mut args, "--file")?),
            "--note" => parsed.note = Some(next_arg(&mut args, "--note")?),
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            other => bail!("unknown argument: {other}"),
        }
    }

    Ok(parsed)
}

fn next_arg<I>(args: &mut I, flag: &str) -> anyhow::Result<String>
where
    I: Iterator<Item = String>,
{
    args.next()
        .with_context(|| format!("{flag} requires a value"))
}

fn build_payload(file: Option<&str>) -> anyhow::Result<MarkdownPayload> {
    match file {
        Some(path) => {
            let bytes = fs::read(path).with_context(|| format!("failed to read {path}"))?;
            let file_name = Path::new(path)
                .file_name()
                .and_then(|value| value.to_str())
                .context("file path must end with a valid UTF-8 file name")?
                .to_string();
            Ok(MarkdownPayload { file_name, bytes })
        }
        None => Ok(MarkdownPayload {
            file_name: "feishu2acp-markdown-smoke.md".to_string(),
            bytes: sample_markdown().into_bytes(),
        }),
    }
}

fn markdown_transport_file_name(file_name: &str) -> String {
    let lower = file_name.to_ascii_lowercase();
    if lower.ends_with(".md.txt") || lower.ends_with(".markdown.txt") {
        file_name.to_string()
    } else if lower.ends_with(".md") || lower.ends_with(".markdown") {
        format!("{file_name}.txt")
    } else if lower.ends_with(".txt") {
        file_name.to_string()
    } else {
        format!("{file_name}.md.txt")
    }
}

fn sample_markdown() -> String {
    concat!(
        "# feishu2acp markdown smoke test\n\n",
        "- source: `src/bin/send_markdown_file.rs`\n",
        "- expectation: bridge ingests this as a markdown document block\n",
        "- follow-up: send `请总结这个文档` to verify session context\n"
    )
    .to_string()
}

async fn send_chat_message(
    client: &LarkClient,
    chat_id: &str,
    msg_type: &str,
    content: String,
) -> anyhow::Result<()> {
    let body = CreateMessageRequestBody::builder()
        .receive_id(chat_id)
        .msg_type(msg_type)
        .content(content)
        .build();
    let request = CreateMessageRequest::builder()
        .receive_id_type("chat_id")
        .request_body(body)
        .build();

    client
        .im
        .v1
        .message
        .create(request, None)
        .await
        .with_context(|| format!("failed to send {msg_type} message to chat {chat_id}"))?;

    Ok(())
}

fn print_usage() {
    eprintln!(
        "Usage: cargo run --bin send_markdown_file -- [--file path/to/file.md] [--note \"请总结这个文档\"] [--chat oc_xxx]"
    );
    eprintln!("Defaults to a built-in smoke-test markdown file and chat `{DEFAULT_CHAT_ID}`.");
}
