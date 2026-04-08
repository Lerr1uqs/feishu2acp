use std::{path::PathBuf, sync::Arc};

use async_trait::async_trait;
use open_lark::{
    client::ws_client::LarkWsClient,
    prelude::*,
    service::im::v1::p2_im_message_receive_v1::MentionEvent,
};
use reqwest::{Client as HttpClient, multipart};
use serde::Deserialize;
use serde_json::json;
use tokio::fs;
use tracing::{debug, error, info, warn};

use crate::{
    config::FeishuConfig,
    domain::{BinarySource, ConversationKey, InboundMessage, MessageBlock, ReplyTarget},
    error::BridgeError,
    ports::{ChannelClient, ChannelRuntime, MessageHandler},
    support::text_preview,
};

#[derive(Clone, Debug)]
struct MarkdownSettings {
    media_dir: PathBuf,
    max_markdown_bytes: usize,
    enable_markdown_input: bool,
    enable_markdown_output: bool,
}

impl From<&FeishuConfig> for MarkdownSettings {
    fn from(value: &FeishuConfig) -> Self {
        Self {
            media_dir: value.media_dir.clone(),
            max_markdown_bytes: value.max_markdown_bytes,
            enable_markdown_input: value.enable_markdown_input,
            enable_markdown_output: value.enable_markdown_output,
        }
    }
}

#[derive(Clone, Debug)]
pub struct FeishuMediaHttpClient {
    client: HttpClient,
    base_url: String,
    app_id: String,
    app_secret: String,
}

impl FeishuMediaHttpClient {
    pub fn new(config: &FeishuConfig) -> Self {
        Self {
            client: HttpClient::new(),
            base_url: "https://open.feishu.cn".to_string(),
            app_id: config.app_id.clone(),
            app_secret: config.app_secret.clone(),
        }
    }

    pub async fn upload_markdown_file(
        &self,
        file_name: &str,
        bytes: Vec<u8>,
    ) -> Result<String, BridgeError> {
        let token = self.tenant_access_token().await?;
        let form = multipart::Form::new()
            .text("file_type", markdown_file_type().to_string())
            .text("file_name", file_name.to_string())
            .part(
                "file",
                multipart::Part::bytes(bytes).file_name(file_name.to_string()),
            );
        let response = self
            .client
            .post(format!("{}/open-apis/im/v1/files", self.base_url))
            .bearer_auth(token)
            .multipart(form)
            .send()
            .await
            .map_err(|error| BridgeError::Channel(format!("failed to upload feishu file: {error}")))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|error| BridgeError::Channel(format!("failed to read feishu upload response: {error}")))?;
        if !status.is_success() {
            return Err(BridgeError::Channel(format!(
                "feishu file upload failed with status {}: {}",
                status, body
            )));
        }

        let parsed: FeishuDataResponse<FeishuFileUploadData> =
            serde_json::from_str(&body).map_err(|error| {
                BridgeError::Channel(format!(
                    "failed to parse feishu upload response: {error}; body={body}"
                ))
            })?;
        if parsed.code != 0 {
            return Err(BridgeError::Channel(format!(
                "feishu file upload failed: {} (code {})",
                parsed.msg, parsed.code
            )));
        }

        parsed
            .data
            .map(|data| data.file_key)
            .ok_or_else(|| BridgeError::Channel("feishu file upload returned no file_key".to_string()))
    }

    pub async fn download_file(&self, file_key: &str) -> Result<Vec<u8>, BridgeError> {
        let token = self.tenant_access_token().await?;
        let response = self
            .client
            .get(format!("{}/open-apis/im/v1/files/{}", self.base_url, file_key))
            .bearer_auth(token)
            .send()
            .await
            .map_err(|error| BridgeError::Channel(format!("failed to download feishu file: {error}")))?;
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response
            .bytes()
            .await
            .map_err(|error| BridgeError::Channel(format!("failed to read feishu download response: {error}")))?;
        if !status.is_success() {
            let body = String::from_utf8_lossy(&bytes);
            return Err(BridgeError::Channel(format!(
                "feishu file download failed with status {}: {}",
                status, body
            )));
        }

        let is_json = headers
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.contains("application/json"))
            .unwrap_or(false);
        if is_json {
            let body = String::from_utf8_lossy(&bytes);
            let parsed: FeishuDataResponse<serde_json::Value> =
                serde_json::from_str(&body).map_err(|error| {
                    BridgeError::Channel(format!(
                        "failed to parse feishu download error response: {error}; body={body}"
                    ))
                })?;
            if parsed.code != 0 {
                return Err(BridgeError::Channel(format!(
                    "feishu file download failed: {} (code {})",
                    parsed.msg, parsed.code
                )));
            }
        }

        Ok(bytes.to_vec())
    }

    async fn tenant_access_token(&self) -> Result<String, BridgeError> {
        let response = self
            .client
            .post(format!(
                "{}/open-apis/auth/v3/tenant_access_token/internal",
                self.base_url
            ))
            .json(&json!({
                "app_id": self.app_id,
                "app_secret": self.app_secret,
            }))
            .send()
            .await
            .map_err(|error| {
                BridgeError::Channel(format!("failed to request feishu tenant access token: {error}"))
            })?;
        let status = response.status();
        let body = response.text().await.map_err(|error| {
            BridgeError::Channel(format!("failed to read feishu token response: {error}"))
        })?;
        if !status.is_success() {
            return Err(BridgeError::Channel(format!(
                "feishu tenant access token request failed with status {}: {}",
                status, body
            )));
        }

        let parsed: FeishuTenantAccessTokenResponse =
            serde_json::from_str(&body).map_err(|error| {
                BridgeError::Channel(format!(
                    "failed to parse feishu tenant token response: {error}; body={body}"
                ))
            })?;
        if parsed.code != 0 {
            return Err(BridgeError::Channel(format!(
                "feishu tenant token request failed: {} (code {})",
                parsed.msg, parsed.code
            )));
        }

        parsed.tenant_access_token.ok_or_else(|| {
            BridgeError::Channel("feishu tenant token response missing tenant_access_token".to_string())
        })
    }
}

#[derive(Debug, Deserialize)]
struct FeishuTenantAccessTokenResponse {
    code: i32,
    msg: String,
    tenant_access_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FeishuDataResponse<T> {
    code: i32,
    msg: String,
    data: Option<T>,
}

#[derive(Debug, Deserialize)]
struct FeishuFileUploadData {
    file_key: String,
}

pub fn build_lark_client(config: &FeishuConfig) -> Arc<LarkClient> {
    Arc::new(
        LarkClient::builder(&config.app_id, &config.app_secret)
            .with_app_type(AppType::SelfBuild)
            .with_enable_token_cache(true)
            .build(),
    )
}

pub struct FeishuChannelClient {
    client: Arc<LarkClient>,
    typing_reaction_emoji: Option<String>,
    markdown: MarkdownSettings,
    media_http: FeishuMediaHttpClient,
}

impl FeishuChannelClient {
    pub fn new(client: Arc<LarkClient>, config: &FeishuConfig) -> Self {
        Self {
            client,
            typing_reaction_emoji: config.typing_reaction_emoji.clone(),
            markdown: MarkdownSettings::from(config),
            media_http: FeishuMediaHttpClient::new(config),
        }
    }
}

fn text_message_content(text: &str) -> String {
    json!({ "text": text }).to_string()
}

fn file_message_content(file_key: &str) -> String {
    json!({ "file_key": file_key }).to_string()
}

fn build_reply_request(msg_type: &str, content: String) -> CreateMessageRequest {
    // Feishu reply expects msg_type/content in the JSON body.
    // The SDK request builder also exposes query-param setters, but using them
    // for reply leads to runtime validation failures from the Feishu API.
    let body = CreateMessageRequestBody {
        receive_id: String::new(),
        msg_type: msg_type.to_string(),
        content,
        uuid: None,
    };

    CreateMessageRequest::builder().request_body(body).build()
}

fn build_chat_send_request(target: &ReplyTarget, msg_type: &str, content: String) -> CreateMessageRequest {
    // Fallback chat send still uses the normal create-message API, so it needs
    // receive_id_type in query params and the actual message payload in the body.
    let body = CreateMessageRequestBody::builder()
        .receive_id(&target.chat_id)
        .msg_type(msg_type)
        .content(content)
        .build();

    CreateMessageRequest::builder()
        .receive_id_type("chat_id")
        .request_body(body)
        .build()
}

#[async_trait]
impl ChannelClient for FeishuChannelClient {
    async fn react_typing(&self, target: &ReplyTarget) -> Result<(), BridgeError> {
        let Some(emoji_type) = self.typing_reaction_emoji.as_deref() else {
            return Ok(());
        };

        // The bridge uses a reaction as a lightweight typing signal. Keep it best-effort so
        // message handling still proceeds if the workspace or emoji code is not accepted.
        debug!(
            reply_to = %target.reply_to_message_id,
            emoji_type,
            "sending feishu typing reaction"
        );
        self.client
            .im
            .v1
            .message_reaction
            .create(&target.reply_to_message_id, emoji_type, None, None)
            .await
            .map(|_| {
                debug!(
                    reply_to = %target.reply_to_message_id,
                    emoji_type,
                    "feishu typing reaction sent"
                );
            })
            .map_err(|error| {
                BridgeError::Channel(format!(
                    "failed to send typing reaction to feishu: {error:?}"
                ))
            })
    }

    async fn send_message(
        &self,
        target: &ReplyTarget,
        blocks: &[MessageBlock],
    ) -> Result<(), BridgeError> {
        for block in blocks {
            match block {
                MessageBlock::Text { text } => {
                    self.send_text_block(target, text).await?;
                }
                MessageBlock::Document {
                    mime_type,
                    file_name,
                    source,
                    extracted_text,
                } => {
                    self.send_markdown_document_block(
                        target,
                        mime_type,
                        file_name,
                        source,
                        extracted_text.as_deref(),
                    )
                    .await?;
                }
                MessageBlock::Image { .. } => {
                    return Err(BridgeError::UnsupportedMessage);
                }
            }
        }

        Ok(())
    }
}

impl FeishuChannelClient {
    async fn send_text_block(&self, target: &ReplyTarget, text: &str) -> Result<(), BridgeError> {
        debug!(
            chat_id = %target.chat_id,
            reply_to = %target.reply_to_message_id,
            chars = text.chars().count(),
            preview = %text_preview(text, 120),
            "sending feishu text reply"
        );
        send_with_fallback(
            &self.client,
            target,
            "text",
            text_message_content(text),
        )
        .await
    }

    async fn send_markdown_document_block(
        &self,
        target: &ReplyTarget,
        mime_type: &str,
        file_name: &str,
        source: &BinarySource,
        extracted_text: Option<&str>,
    ) -> Result<(), BridgeError> {
        if !self.markdown.enable_markdown_output {
            return Err(BridgeError::Channel(
                "markdown 文件输出当前未启用。".to_string(),
            ));
        }

        if !is_markdown_mime(mime_type) {
            return Err(BridgeError::Channel(format!(
                "unsupported document mime type for feishu output: {mime_type}"
            )));
        }

        let bytes = match source {
            BinarySource::Bytes(bytes) => bytes.clone(),
            BinarySource::LocalPath(path) => fs::read(path).await.map_err(|error| {
                BridgeError::Channel(format!(
                    "failed to read markdown document {}: {error}",
                    path.display()
                ))
            })?,
        };
        validate_markdown_bytes(file_name, &bytes, self.markdown.max_markdown_bytes)?;
        if extracted_text.is_some() && !bytes.is_empty() {
            debug!(
                chat_id = %target.chat_id,
                reply_to = %target.reply_to_message_id,
                file_name,
                bytes = bytes.len(),
                "sending feishu markdown document reply"
            );
        }
        let transport_file_name = markdown_transport_file_name(file_name);

        let file_key = self
            .media_http
            .upload_markdown_file(&transport_file_name, bytes)
            .await?;

        send_with_fallback(
            &self.client,
            target,
            "file",
            file_message_content(&file_key),
        )
        .await
    }
}

async fn send_with_fallback(
    client: &LarkClient,
    target: &ReplyTarget,
    msg_type: &str,
    content: String,
) -> Result<(), BridgeError> {
    let reply_request = build_reply_request(msg_type, content.clone());

    match client
        .im
        .v1
        .message
        .reply(&target.reply_to_message_id, reply_request, None)
        .await
    {
        Ok(_) => Ok(()),
        Err(reply_error) => {
            warn!("feishu reply failed, falling back to chat send: {reply_error:?}");
            client
                .im
                .v1
                .message
                .create(build_chat_send_request(target, msg_type, content), None)
                .await
                .map(|_| ())
                .map_err(|error| {
                    BridgeError::Channel(format!("failed to send reply to feishu: {error:?}"))
                })
        }
    }
}

pub struct FeishuLongConnectionRuntime {
    client: Arc<LarkClient>,
    markdown: MarkdownSettings,
    media_http: FeishuMediaHttpClient,
    nickname: Option<String>,
}

impl FeishuLongConnectionRuntime {
    pub fn new(client: Arc<LarkClient>, config: &FeishuConfig) -> Self {
        Self {
            client,
            markdown: MarkdownSettings::from(config),
            media_http: FeishuMediaHttpClient::new(config),
            nickname: config.nickname.clone(),
        }
    }
}

#[async_trait(?Send)]
impl ChannelRuntime for FeishuLongConnectionRuntime {
    async fn run(&self, handler: Arc<dyn MessageHandler>) -> Result<(), BridgeError> {
        info!("starting feishu websocket runtime");
        let config = Arc::new(self.client.config.clone());
        let handler_for_messages = handler.clone();
        let client_for_messages = self.client.clone();
        let markdown_settings = self.markdown.clone();
        let media_http = self.media_http.clone();
        let nickname = self.nickname.clone();
        let dispatcher = EventDispatcherHandler::builder()
            .register_p2_im_message_receive_v1(move |event| {
                let handler = handler_for_messages.clone();
                let client = client_for_messages.clone();
                let markdown = markdown_settings.clone();
                let media_http = media_http.clone();
                let nickname = nickname.clone();
                tokio::spawn(async move {
                    match translate_inbound_event(
                        &client,
                        &media_http,
                        &markdown,
                        nickname.as_deref(),
                        &event,
                    )
                    .await
                    {
                        Ok(Some(message)) => {
                            if let Err(error) = handler.handle_message(message).await {
                                error!("failed to process inbound feishu message: {error}");
                            }
                        }
                        Ok(None) => {}
                        Err(error) => {
                            warn!("failed to translate inbound feishu event: {error}");
                            if let Some(target) = reply_target_for_event(&event) {
                                if let Err(send_error) =
                                    send_with_fallback(
                                        &client,
                                        &target,
                                        "text",
                                        text_message_content(&error.user_message()),
                                    )
                                    .await
                                {
                                    warn!(
                                        "failed to send feishu translation error reply: {send_error}"
                                    );
                                }
                            }
                        }
                    }
                });
            })
            .map_err(BridgeError::Channel)?
            // Read receipts are expected background traffic and should not be surfaced as errors.
            // open-lark has seen two event-name variants in the wild, so the vendored dispatcher
            // resolves both aliases to this no-op handler.
            .register_p2_im_message_read_v1(|_event| {})
            .map_err(BridgeError::Channel)?
            .build();

        LarkWsClient::open(config, dispatcher)
            .await
            .map_err(|error| {
                BridgeError::Channel(format!("failed to open feishu websocket: {error}"))
            })
    }
}

#[derive(Debug, Deserialize)]
struct TextPayload {
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FilePayload {
    file_key: Option<String>,
    file_name: Option<String>,
    file_type: Option<String>,
    file_size: Option<usize>,
}

async fn translate_inbound_event(
    client: &LarkClient,
    media_http: &FeishuMediaHttpClient,
    markdown: &MarkdownSettings,
    nickname: Option<&str>,
    event: &P2ImMessageReceiveV1,
) -> Result<Option<InboundMessage>, BridgeError> {
    if event.event.sender.sender_type != "user" {
        debug!(
            sender_type = %event.event.sender.sender_type,
            message_id = %event.event.message.message_id,
            "ignoring non-user feishu event"
        );
        return Ok(None);
    }

    match event.event.message.message_type.as_str() {
        "text" => translate_text_event(event, nickname),
        "file" => translate_file_event(client, media_http, markdown, event).await,
        other => {
            debug!(
                message_id = %event.event.message.message_id,
                message_type = other,
                "ignoring unsupported feishu message"
            );
            Ok(None)
        }
    }
}

fn translate_text_event(
    event: &P2ImMessageReceiveV1,
    nickname: Option<&str>,
) -> Result<Option<InboundMessage>, BridgeError> {
    let payload: TextPayload =
        serde_json::from_str(&event.event.message.content).map_err(|error| {
            BridgeError::Channel(format!("failed to parse feishu message content: {error}"))
        })?;

    let Some(text) = normalize_inbound_text(event, payload.text.unwrap_or_default(), nickname) else {
        return Ok(None);
    };

    if event.event.message.chat_type == "group" && nickname.is_some() {
        debug!(
            message_id = %event.event.message.message_id,
            chat_id = %event.event.message.chat_id,
            thread_id = event.event.message.thread_id.as_deref().unwrap_or("-"),
            preview = %text_preview(&text, 120),
            "translated feishu inbound group text message after nickname filtering"
        );
    }

    if text.is_empty() {
        debug!(
            message_id = %event.event.message.message_id,
            "ignoring empty feishu text message"
        );
        return Ok(None);
    }

    debug!(
        message_id = %event.event.message.message_id,
        chat_id = %event.event.message.chat_id,
        thread_id = event.event.message.thread_id.as_deref().unwrap_or("-"),
        preview = %text_preview(&text, 120),
        "translated feishu inbound text message"
    );

    Ok(Some(InboundMessage {
        conversation: conversation_key_for_event(event),
        reply_target: reply_target_for_event(event).expect("message events always include target"),
        blocks: vec![MessageBlock::text(text)],
    }))
}

fn normalize_inbound_text(
    event: &P2ImMessageReceiveV1,
    text: String,
    nickname: Option<&str>,
) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }

    if event.event.message.chat_type != "group" {
        return Some(trimmed.to_string());
    }

    let Some(nickname) = nickname
        .map(str::trim)
        .filter(|value| !value.is_empty()) else {
        return Some(trimmed.to_string());
    };

    extract_prefixed_group_text(trimmed, nickname, event.event.message.mentions.as_deref())
}

fn extract_prefixed_group_text(
    text: &str,
    nickname: &str,
    mentions: Option<&[MentionEvent]>,
) -> Option<String> {
    if let Some(stripped) = strip_prefixed_text(text, &format!("@{nickname}")) {
        return Some(stripped);
    }

    let first_mention = mentions
        .and_then(|items| items.first())
        .filter(|mention| mention.name.trim() == nickname)?;

    if let Some(stripped) = strip_prefixed_text(text, &first_mention.key) {
        return Some(stripped);
    }

    let rest = strip_leading_at_tag(text)?;
    strip_required_whitespace(rest)
}

fn strip_prefixed_text(text: &str, prefix: &str) -> Option<String> {
    let rest = text.strip_prefix(prefix)?;
    strip_required_whitespace(rest)
}

fn strip_leading_at_tag(text: &str) -> Option<&str> {
    if !text.starts_with("<at") {
        return None;
    }

    let end = text.find("</at>")? + "</at>".len();
    Some(&text[end..])
}

fn strip_required_whitespace(value: &str) -> Option<String> {
    let first = value.chars().next()?;
    if !first.is_whitespace() {
        return None;
    }

    let trimmed = value.trim_start();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

async fn translate_file_event(
    _client: &LarkClient,
    media_http: &FeishuMediaHttpClient,
    markdown: &MarkdownSettings,
    event: &P2ImMessageReceiveV1,
) -> Result<Option<InboundMessage>, BridgeError> {
    if !markdown.enable_markdown_input {
        return Err(BridgeError::InputRejected(
            "当前机器人未启用 markdown 文件输入。".to_string(),
        ));
    }

    let payload: FilePayload =
        serde_json::from_str(&event.event.message.content).map_err(|error| {
            BridgeError::Channel(format!("failed to parse feishu file content: {error}"))
        })?;
    let file_key = payload.file_key.as_deref().ok_or_else(|| {
        BridgeError::Channel("feishu file payload missing file_key".to_string())
    })?;
    if let Some(file_size) = payload.file_size {
        if file_size > markdown.max_markdown_bytes {
            return Err(BridgeError::InputRejected(format!(
                "markdown 文档过大，已超过 {} KB 限制。",
                markdown.max_markdown_bytes / 1024
            )));
        }
    }

    let bytes = media_http.download_file(file_key).await?;

    let file_name = resolve_markdown_file_name(
        event,
        payload.file_name.as_deref(),
        payload.file_type.as_deref(),
    )?;
    validate_markdown_bytes(&file_name, &bytes, markdown.max_markdown_bytes)?;
    let text = String::from_utf8(bytes.clone()).map_err(|error| {
        BridgeError::InputRejected(format!(
            "markdown 文档不是有效的 UTF-8 文本：{error}"
        ))
    })?;

    let local_path = store_markdown_document(markdown, &event.event.message.message_id, &file_name, &bytes)
        .await?;

    debug!(
        message_id = %event.event.message.message_id,
        chat_id = %event.event.message.chat_id,
        file_name,
        bytes = bytes.len(),
        "translated feishu inbound markdown file"
    );

    Ok(Some(InboundMessage {
        conversation: conversation_key_for_event(event),
        reply_target: reply_target_for_event(event).expect("message events always include target"),
        blocks: vec![MessageBlock::Document {
            mime_type: "text/markdown".to_string(),
            file_name,
            source: BinarySource::LocalPath(local_path),
            extracted_text: Some(text),
        }],
    }))
}

fn conversation_key_for_event(event: &P2ImMessageReceiveV1) -> ConversationKey {
    ConversationKey {
        tenant_key: event.event.sender.tenant_key.clone(),
        chat_id: event.event.message.chat_id.clone(),
        user_open_id: event.event.sender.sender_id.open_id.clone(),
        thread_id: event.event.message.thread_id.clone(),
    }
}

fn reply_target_for_event(event: &P2ImMessageReceiveV1) -> Option<ReplyTarget> {
    Some(ReplyTarget {
        chat_id: event.event.message.chat_id.clone(),
        reply_to_message_id: event.event.message.message_id.clone(),
    })
}

async fn store_markdown_document(
    markdown: &MarkdownSettings,
    message_id: &str,
    file_name: &str,
    bytes: &[u8],
) -> Result<PathBuf, BridgeError> {
    let dir = markdown.media_dir.join("inbound");
    fs::create_dir_all(&dir).await.map_err(|error| {
        BridgeError::Channel(format!(
            "failed to create feishu media directory {}: {error}",
            dir.display()
        ))
    })?;
    let path = dir.join(format!(
        "{}-{}",
        sanitize_file_name(message_id),
        sanitize_file_name(file_name)
    ));
    fs::write(&path, bytes).await.map_err(|error| {
        BridgeError::Channel(format!(
            "failed to store markdown document {}: {error}",
            path.display()
        ))
    })?;
    Ok(path)
}

fn resolve_markdown_file_name(
    event: &P2ImMessageReceiveV1,
    file_name: Option<&str>,
    file_type: Option<&str>,
) -> Result<String, BridgeError> {
    match file_name {
        Some(name) if is_markdown_file_name(name) => {
            Ok(normalize_markdown_file_name(&sanitize_file_name(name)))
        }
        Some(name) if is_markdown_file_type(file_type) => {
            let sanitized = sanitize_file_name(name);
            if is_markdown_file_name(&sanitized) {
                Ok(normalize_markdown_file_name(&sanitized))
            } else {
                Ok(format!("{sanitized}.md"))
            }
        }
        Some(name) => Err(BridgeError::InputRejected(format!(
            "当前只支持 .md / .markdown 文件，收到 `{name}`。"
        ))),
        None if is_markdown_file_type(file_type) || file_type.is_none() => Ok(format!(
            "{}.md",
            sanitize_file_name(&event.event.message.message_id)
        )),
        None => Err(BridgeError::InputRejected(
            "当前只支持 markdown 文件输入。".to_string(),
        )),
    }
}

fn validate_markdown_bytes(
    file_name: &str,
    bytes: &[u8],
    max_markdown_bytes: usize,
) -> Result<(), BridgeError> {
    if bytes.len() > max_markdown_bytes {
        return Err(BridgeError::InputRejected(format!(
            "markdown 文档 `{file_name}` 过大，已超过 {} KB 限制。",
            max_markdown_bytes / 1024
        )));
    }
    if bytes.contains(&0) {
        return Err(BridgeError::InputRejected(format!(
            "markdown 文档 `{file_name}` 看起来不是纯文本文件。"
        )));
    }
    Ok(())
}

fn is_markdown_mime(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "text/markdown" | "text/x-markdown"
    )
}

fn is_markdown_file_name(value: &str) -> bool {
    let lower = value.trim().to_ascii_lowercase();
    lower.ends_with(".md")
        || lower.ends_with(".markdown")
        || lower.ends_with(".md.txt")
        || lower.ends_with(".markdown.txt")
}

fn is_markdown_file_type(value: Option<&str>) -> bool {
    matches!(
        value.unwrap_or_default().trim().to_ascii_lowercase().as_str(),
        "md" | "markdown" | "text/markdown" | "text/x-markdown"
    )
}

fn markdown_file_type() -> &'static str {
    "stream"
}

fn normalize_markdown_file_name(value: &str) -> String {
    let lower = value.to_ascii_lowercase();
    if lower.ends_with(".markdown.txt") {
        value[..value.len() - 4].to_string()
    } else if lower.ends_with(".md.txt") {
        value[..value.len() - 4].to_string()
    } else {
        value.to_string()
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

fn sanitize_file_name(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    let trimmed = sanitized.trim_matches('_');
    if trimmed.is_empty() {
        "document.md".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use open_lark::event::dispatcher::EventDispatcherHandler;
    use serde_json::json;

    use crate::{config::FeishuConfig, domain::MessageBlock};

    use super::{
        FeishuMediaHttpClient, MarkdownSettings, build_chat_send_request, build_lark_client,
        build_reply_request, file_message_content, is_markdown_file_name,
        markdown_transport_file_name, normalize_markdown_file_name, sanitize_file_name,
        text_message_content, translate_inbound_event,
    };
    use open_lark::service::im::v1::{
        p2_im_message_read_v1::P2ImMessageReadV1,
        p2_im_message_receive_v1::P2ImMessageReceiveV1,
    };

    fn markdown_settings() -> MarkdownSettings {
        MarkdownSettings {
            media_dir: std::env::temp_dir(),
            max_markdown_bytes: 1024 * 1024,
            enable_markdown_input: true,
            enable_markdown_output: true,
        }
    }

    fn feishu_config() -> FeishuConfig {
        FeishuConfig {
            app_id: String::new(),
            app_secret: String::new(),
            nickname: None,
            typing_reaction_emoji: None,
            media_dir: std::env::temp_dir(),
            max_markdown_bytes: 1024 * 1024,
            enable_markdown_input: true,
            enable_markdown_output: true,
        }
    }

    #[tokio::test]
    async fn translate_text_message_event() {
        let event: P2ImMessageReceiveV1 = serde_json::from_value(json!({
            "schema": "2.0",
            "header": {
                "event_id": "evt-1",
                "token": "",
                "create_time": "1",
                "event_type": "im.message.receive_v1",
                "tenant_key": "tenant",
                "app_id": "app"
            },
            "event": {
                "sender": {
                    "sender_id": {
                        "union_id": "union",
                        "user_id": "user",
                        "open_id": "open"
                    },
                    "sender_type": "user",
                    "tenant_key": "tenant"
                },
                "message": {
                    "message_id": "msg",
                    "chat_id": "chat",
                    "chat_type": "p2p",
                    "message_type": "text",
                    "content": "{\"text\":\"hello\"}",
                    "create_time": "1",
                    "update_time": "1"
                }
            }
        }))
        .unwrap();

        let translated = translate_inbound_event(
            &build_lark_client(&feishu_config()),
            &FeishuMediaHttpClient::new(&feishu_config()),
            &markdown_settings(),
            None,
            &event,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            translated.blocks,
            vec![MessageBlock::text("hello".to_string())]
        );
        assert_eq!(translated.reply_target.reply_to_message_id, "msg");
    }

    #[tokio::test]
    async fn translate_group_message_strips_configured_nickname_prefix() {
        let event: P2ImMessageReceiveV1 = serde_json::from_value(json!({
            "schema": "2.0",
            "header": {
                "event_id": "evt-mention",
                "token": "",
                "create_time": "1",
                "event_type": "im.message.receive_v1",
                "tenant_key": "tenant",
                "app_id": "app"
            },
            "event": {
                "sender": {
                    "sender_id": {
                        "union_id": "union",
                        "user_id": "user",
                        "open_id": "open"
                    },
                    "sender_type": "user",
                    "tenant_key": "tenant"
                },
                "message": {
                    "message_id": "msg-group",
                    "chat_id": "chat",
                    "chat_type": "group",
                    "message_type": "text",
                    "content": "{\"text\":\"@藤田琴音 /pwd\"}",
                    "create_time": "1",
                    "update_time": "1",
                    "mentions": []
                }
            }
        }))
        .unwrap();

        let translated = translate_inbound_event(
            &build_lark_client(&feishu_config()),
            &FeishuMediaHttpClient::new(&feishu_config()),
            &markdown_settings(),
            Some("藤田琴音"),
            &event,
        )
        .await
        .unwrap()
        .unwrap();

        assert_eq!(translated.blocks, vec![MessageBlock::text("/pwd")]);
    }

    #[tokio::test]
    async fn translate_group_message_ignores_text_without_required_nickname_prefix() {
        let event: P2ImMessageReceiveV1 = serde_json::from_value(json!({
            "schema": "2.0",
            "header": {
                "event_id": "evt-group-plain",
                "token": "",
                "create_time": "1",
                "event_type": "im.message.receive_v1",
                "tenant_key": "tenant",
                "app_id": "app"
            },
            "event": {
                "sender": {
                    "sender_id": {
                        "union_id": "union",
                        "user_id": "user",
                        "open_id": "open"
                    },
                    "sender_type": "user",
                    "tenant_key": "tenant"
                },
                "message": {
                    "message_id": "msg-group-plain",
                    "chat_id": "chat",
                    "chat_type": "group",
                    "message_type": "text",
                    "content": "{\"text\":\"/pwd\"}",
                    "create_time": "1",
                    "update_time": "1"
                }
            }
        }))
        .unwrap();

        let translated = translate_inbound_event(
            &build_lark_client(&feishu_config()),
            &FeishuMediaHttpClient::new(&feishu_config()),
            &markdown_settings(),
            Some("藤田琴音"),
            &event,
        )
        .await
        .unwrap();

        assert!(translated.is_none());
    }

    #[tokio::test]
    async fn translate_group_message_accepts_first_mention_tag_for_configured_nickname() {
        let event: P2ImMessageReceiveV1 = serde_json::from_value(json!({
            "schema": "2.0",
            "header": {
                "event_id": "evt-group-at-tag",
                "token": "",
                "create_time": "1",
                "event_type": "im.message.receive_v1",
                "tenant_key": "tenant",
                "app_id": "app"
            },
            "event": {
                "sender": {
                    "sender_id": {
                        "union_id": "union",
                        "user_id": "user",
                        "open_id": "open"
                    },
                    "sender_type": "user",
                    "tenant_key": "tenant"
                },
                "message": {
                    "message_id": "msg-group-at-tag",
                    "chat_id": "chat",
                    "chat_type": "group",
                    "message_type": "text",
                    "content": "{\"text\":\"<at user_id=\\\"ou_bot\\\"></at> /pwd\"}",
                    "create_time": "1",
                    "update_time": "1",
                    "mentions": [{
                        "key": "@_user_1",
                        "id": {
                            "union_id": "union-bot",
                            "user_id": "user-bot",
                            "open_id": "ou_bot"
                        },
                        "name": "藤田琴音",
                        "tenant_key": "tenant"
                    }]
                }
            }
        }))
        .unwrap();

        let translated = translate_inbound_event(
            &build_lark_client(&feishu_config()),
            &FeishuMediaHttpClient::new(&feishu_config()),
            &markdown_settings(),
            Some("藤田琴音"),
            &event,
        )
        .await
        .unwrap()
        .unwrap();

        assert_eq!(translated.blocks, vec![MessageBlock::text("/pwd")]);
    }

    #[tokio::test]
    async fn ignores_non_text_messages() {
        let event: P2ImMessageReceiveV1 = serde_json::from_value(json!({
            "schema": "2.0",
            "header": {
                "event_id": "evt-1",
                "token": "",
                "create_time": "1",
                "event_type": "im.message.receive_v1",
                "tenant_key": "tenant",
                "app_id": "app"
            },
            "event": {
                "sender": {
                    "sender_id": {
                        "union_id": "union",
                        "user_id": "user",
                        "open_id": "open"
                    },
                    "sender_type": "user",
                    "tenant_key": "tenant"
                },
                "message": {
                    "message_id": "msg",
                    "chat_id": "chat",
                    "chat_type": "p2p",
                    "message_type": "image",
                    "content": "{}",
                    "create_time": "1",
                    "update_time": "1"
                }
            }
        }))
        .unwrap();

        assert!(
            translate_inbound_event(
                &build_lark_client(&feishu_config()),
                &FeishuMediaHttpClient::new(&feishu_config()),
                &markdown_settings(),
                None,
                &event
            )
            .await
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn reply_request_serializes_text_into_request_body() {
        let request = build_reply_request("text", text_message_content("hello"));
        let body: serde_json::Value = serde_json::from_slice(&request.api_req.body).unwrap();

        assert_eq!(body["msg_type"], "text");
        assert_eq!(body["content"], text_message_content("hello"));
        assert!(request.api_req.query_params.get("msg_type").is_none());
        assert!(request.api_req.query_params.get("content").is_none());
    }

    #[test]
    fn fallback_chat_send_request_keeps_chat_target_and_body() {
        let request = build_chat_send_request(
            &crate::domain::ReplyTarget {
                chat_id: "chat-123".to_string(),
                reply_to_message_id: "msg-123".to_string(),
            },
            "text",
            text_message_content("world"),
        );
        let body: serde_json::Value = serde_json::from_slice(&request.api_req.body).unwrap();

        assert_eq!(
            request.api_req.query_params.get("receive_id_type"),
            Some(&"chat_id".to_string())
        );
        assert_eq!(body["receive_id"], "chat-123");
        assert_eq!(body["msg_type"], "text");
        assert_eq!(body["content"], text_message_content("world"));
    }

    #[test]
    fn reply_request_supports_file_payloads() {
        let request = build_reply_request("file", file_message_content("file-123"));
        let body: serde_json::Value = serde_json::from_slice(&request.api_req.body).unwrap();

        assert_eq!(body["msg_type"], "file");
        assert_eq!(body["content"], file_message_content("file-123"));
    }

    #[test]
    fn markdown_helpers_sanitize_and_validate_names() {
        assert!(is_markdown_file_name("README.md"));
        assert!(is_markdown_file_name("guide.markdown"));
        assert!(is_markdown_file_name("guide.md.txt"));
        assert_eq!(sanitize_file_name("design doc.md"), "design_doc.md");
        assert_eq!(normalize_markdown_file_name("guide.md.txt"), "guide.md");
        assert_eq!(
            markdown_transport_file_name("guide.md"),
            "guide.md.txt"
        );
    }

    #[test]
    fn message_read_events_dispatch_without_missing_handler_error() {
        let called = Arc::new(AtomicBool::new(false));
        let called_for_handler = called.clone();
        let dispatcher = EventDispatcherHandler::builder()
            .register_p2_im_message_read_v1(move |_event: P2ImMessageReadV1| {
                called_for_handler.store(true, Ordering::SeqCst);
            })
            .unwrap()
            .build();

        let payload = serde_json::to_vec(&json!({
            "schema": "2.0",
            "header": {
                "event_id": "evt-read-1",
                "token": "",
                "create_time": "1",
                "event_type": "im.message.message_read_v1",
                "tenant_key": "tenant",
                "app_id": "app"
            },
            "event": {
                "reader": {
                    "read_time": "1",
                    "reader_id": {
                        "open_id": "open",
                        "union_id": null,
                        "user_id": null
                    },
                    "tenant_key": "tenant"
                },
                "message_id_list": ["msg-1"]
            }
        }))
        .unwrap();

        assert!(dispatcher.do_without_validation(payload).is_ok());
        assert!(called.load(Ordering::SeqCst));
    }

    #[test]
    fn message_read_events_also_accept_documented_alias() {
        let called = Arc::new(AtomicBool::new(false));
        let called_for_handler = called.clone();
        let dispatcher = EventDispatcherHandler::builder()
            .register_p2_im_message_read_v1(move |_event: P2ImMessageReadV1| {
                called_for_handler.store(true, Ordering::SeqCst);
            })
            .unwrap()
            .build();

        let payload = serde_json::to_vec(&json!({
            "schema": "2.0",
            "header": {
                "event_id": "evt-read-2",
                "token": "",
                "create_time": "1",
                "event_type": "im.message.read_v1",
                "tenant_key": "tenant",
                "app_id": "app"
            },
            "event": {
                "reader": {
                    "read_time": "1",
                    "reader_id": {
                        "open_id": "open",
                        "union_id": null,
                        "user_id": null
                    },
                    "tenant_key": "tenant"
                },
                "message_id_list": ["msg-2"]
            }
        }))
        .unwrap();

        assert!(dispatcher.do_without_validation(payload).is_ok());
        assert!(called.load(Ordering::SeqCst));
    }
}
