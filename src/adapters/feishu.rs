use std::sync::Arc;

use async_trait::async_trait;
use open_lark::{client::ws_client::LarkWsClient, prelude::*};
use serde::Deserialize;
use serde_json::json;
use tracing::{error, warn};

use crate::{
    config::FeishuConfig,
    domain::{ConversationKey, InboundMessage, ReplyTarget},
    error::BridgeError,
    ports::{ChannelClient, ChannelRuntime, MessageHandler},
};

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
}

impl FeishuChannelClient {
    pub fn new(client: Arc<LarkClient>) -> Self {
        Self { client }
    }
}

#[async_trait]
impl ChannelClient for FeishuChannelClient {
    async fn send_text(&self, target: &ReplyTarget, text: &str) -> Result<(), BridgeError> {
        let content = json!({ "text": text }).to_string();
        let reply_request = CreateMessageRequest::builder()
            .msg_type("text")
            .content(content.clone())
            .build();

        match self
            .client
            .im
            .v1
            .message
            .reply(&target.reply_to_message_id, reply_request, None)
            .await
        {
            Ok(_) => Ok(()),
            Err(reply_error) => {
                warn!("feishu reply failed, falling back to chat send: {reply_error:?}");
                let body = CreateMessageRequestBody::builder()
                    .receive_id(&target.chat_id)
                    .msg_type("text")
                    .content(content)
                    .build();
                let fallback_request = CreateMessageRequest::builder()
                    .receive_id_type("chat_id")
                    .request_body(body)
                    .build();
                self.client
                    .im
                    .v1
                    .message
                    .create(fallback_request, None)
                    .await
                    .map(|_| ())
                    .map_err(|error| {
                        BridgeError::Channel(format!("failed to send reply to feishu: {error:?}"))
                    })
            }
        }
    }
}

pub struct FeishuLongConnectionRuntime {
    client: Arc<LarkClient>,
}

impl FeishuLongConnectionRuntime {
    pub fn new(client: Arc<LarkClient>) -> Self {
        Self { client }
    }
}

#[async_trait(?Send)]
impl ChannelRuntime for FeishuLongConnectionRuntime {
    async fn run(&self, handler: Arc<dyn MessageHandler>) -> Result<(), BridgeError> {
        let config = Arc::new(self.client.config.clone());
        let handler_for_messages = handler.clone();
        let dispatcher = EventDispatcherHandler::builder()
            .register_p2_im_message_receive_v1(move |event| {
                let handler = handler_for_messages.clone();
                tokio::spawn(async move {
                    match translate_inbound_event(&event) {
                        Ok(Some(message)) => {
                            if let Err(error) = handler.handle_message(message).await {
                                error!("failed to process inbound feishu message: {error}");
                            }
                        }
                        Ok(None) => {}
                        Err(error) => {
                            warn!("failed to translate inbound feishu event: {error}");
                        }
                    }
                });
            })
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

pub fn translate_inbound_event(
    event: &P2ImMessageReceiveV1,
) -> Result<Option<InboundMessage>, BridgeError> {
    if event.event.sender.sender_type != "user" {
        return Ok(None);
    }

    if event.event.message.message_type != "text" {
        return Ok(None);
    }

    let payload: TextPayload =
        serde_json::from_str(&event.event.message.content).map_err(|error| {
            BridgeError::Channel(format!("failed to parse feishu message content: {error}"))
        })?;

    let text = payload.text.unwrap_or_default().trim().to_string();
    if text.is_empty() {
        return Ok(None);
    }

    Ok(Some(InboundMessage {
        conversation: ConversationKey {
            tenant_key: event.event.sender.tenant_key.clone(),
            chat_id: event.event.message.chat_id.clone(),
            user_open_id: event.event.sender.sender_id.open_id.clone(),
            thread_id: event.event.message.thread_id.clone(),
        },
        reply_target: ReplyTarget {
            chat_id: event.event.message.chat_id.clone(),
            reply_to_message_id: event.event.message.message_id.clone(),
        },
        text,
    }))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::translate_inbound_event;
    use open_lark::service::im::v1::p2_im_message_receive_v1::P2ImMessageReceiveV1;

    #[test]
    fn translate_text_message_event() {
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

        let translated = translate_inbound_event(&event).unwrap().unwrap();
        assert_eq!(translated.text, "hello");
        assert_eq!(translated.reply_target.reply_to_message_id, "msg");
    }

    #[test]
    fn ignores_non_text_messages() {
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

        assert!(translate_inbound_event(&event).unwrap().is_none());
    }
}
