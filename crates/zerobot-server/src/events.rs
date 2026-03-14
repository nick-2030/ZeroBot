use std::collections::HashMap;
use tokio::sync::{broadcast, Mutex};

use zerobot_core::SessionId;

#[derive(Debug, Clone, serde::Serialize)]
pub struct ServerEvent {
    pub event_type: String,
    pub data: serde_json::Value,
}

#[derive(Default)]
pub struct EventBus {
    channels: Mutex<HashMap<SessionId, broadcast::Sender<ServerEvent>>>,
}

impl EventBus {
    pub async fn subscribe(&self, session_id: &SessionId) -> broadcast::Receiver<ServerEvent> {
        let mut channels = self.channels.lock().await;
        let sender = channels
            .entry(session_id.clone())
            .or_insert_with(|| {
                let (tx, _rx) = broadcast::channel(128);
                tx
            })
            .clone();
        sender.subscribe()
    }

    pub async fn publish(&self, session_id: &SessionId, event: ServerEvent) {
        let mut channels = self.channels.lock().await;
        let sender = channels
            .entry(session_id.clone())
            .or_insert_with(|| {
                let (tx, _rx) = broadcast::channel(128);
                tx
            })
            .clone();
        let _ = sender.send(event);
    }
}
