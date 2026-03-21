use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use tokio::sync::mpsc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboundMessage {
    pub source: String,
    pub session_key: String,
    pub channel: String,
    pub sender_id: String,
    pub chat_id: String,
    pub content: String,
    #[serde(default)]
    pub metadata: JsonValue,
    #[serde(default)]
    pub created_at: i64,
}

impl InboundMessage {
    pub fn new(
        source: impl Into<String>,
        session_key: impl Into<String>,
        channel: impl Into<String>,
        sender_id: impl Into<String>,
        chat_id: impl Into<String>,
        content: impl Into<String>,
        metadata: JsonValue,
    ) -> Self {
        Self {
            source: source.into(),
            session_key: session_key.into(),
            channel: channel.into(),
            sender_id: sender_id.into(),
            chat_id: chat_id.into(),
            content: content.into(),
            metadata,
            created_at: Utc::now().timestamp(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboundMessage {
    pub channel: String,
    pub chat_id: String,
    pub content: String,
    #[serde(default)]
    pub media: Vec<String>,
    #[serde(default)]
    pub metadata: JsonValue,
}

impl OutboundMessage {
    pub fn new(
        channel: impl Into<String>,
        chat_id: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self {
            channel: channel.into(),
            chat_id: chat_id.into(),
            content: content.into(),
            media: Vec::new(),
            metadata: JsonValue::Object(Default::default()),
        }
    }
}

#[derive(Clone)]
pub struct MessageBus {
    inbound_tx: mpsc::UnboundedSender<InboundMessage>,
    outbound_tx: mpsc::UnboundedSender<OutboundMessage>,
}

impl MessageBus {
    pub fn new() -> (
        Self,
        mpsc::UnboundedReceiver<InboundMessage>,
        mpsc::UnboundedReceiver<OutboundMessage>,
    ) {
        let (inbound_tx, inbound_rx) = mpsc::unbounded_channel();
        let (outbound_tx, outbound_rx) = mpsc::unbounded_channel();
        (
            Self {
                inbound_tx,
                outbound_tx,
            },
            inbound_rx,
            outbound_rx,
        )
    }

    pub fn publish_inbound(&self, msg: InboundMessage) -> Result<(), String> {
        self.inbound_tx.send(msg).map_err(|err| err.to_string())
    }

    pub fn publish_outbound(&self, msg: OutboundMessage) -> Result<(), String> {
        self.outbound_tx.send(msg).map_err(|err| err.to_string())
    }

    pub fn outbound_sender(&self) -> mpsc::UnboundedSender<OutboundMessage> {
        self.outbound_tx.clone()
    }

    pub fn inbound_sender(&self) -> mpsc::UnboundedSender<InboundMessage> {
        self.inbound_tx.clone()
    }
}
