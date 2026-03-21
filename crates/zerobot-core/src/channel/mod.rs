use crate::bus::{InboundMessage, OutboundMessage};
use crate::config::Settings;
use crate::error::{ZeroBotError, ZeroBotResult};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

pub mod feishu;

#[async_trait]
pub trait ChatChannel: Send + Sync {
    fn name(&self) -> &str;
    async fn start(&self) -> ZeroBotResult<()>;
    async fn stop(&self) -> ZeroBotResult<()>;
    async fn send(&self, msg: OutboundMessage) -> ZeroBotResult<()>;
}

pub struct ChannelManager {
    channels: HashMap<String, Arc<dyn ChatChannel>>,
    dispatch_task: Option<JoinHandle<()>>,
}

impl ChannelManager {
    pub fn new() -> Self {
        Self {
            channels: HashMap::new(),
            dispatch_task: None,
        }
    }

    pub fn register<C: ChatChannel + 'static>(&mut self, channel: C) {
        self.channels
            .insert(channel.name().to_string(), Arc::new(channel));
    }

    pub fn enabled_channels(&self) -> Vec<String> {
        let mut names = self.channels.keys().cloned().collect::<Vec<_>>();
        names.sort();
        names
    }

    pub async fn start_all(
        &mut self,
        mut outbound_rx: mpsc::UnboundedReceiver<OutboundMessage>,
    ) -> ZeroBotResult<()> {
        let enabled = self.enabled_channels();
        tracing::info!("channel manager starting: {:?}", enabled);
        for channel in self.channels.values() {
            channel.start().await?;
        }

        let channels = self.channels.clone();
        self.dispatch_task = Some(tokio::spawn(async move {
            while let Some(msg) = outbound_rx.recv().await {
                if let Some(ch) = channels.get(&msg.channel) {
                    if let Err(err) = ch.send(msg).await {
                        tracing::warn!("channel send failed: {}", err);
                    }
                } else {
                    tracing::warn!("channel send dropped: unknown channel {}", msg.channel);
                }
            }
            tracing::info!("channel manager outbound dispatcher stopped");
        }));

        Ok(())
    }

    pub async fn stop_all(&mut self) -> ZeroBotResult<()> {
        tracing::info!("channel manager stopping");
        if let Some(task) = self.dispatch_task.take() {
            task.abort();
        }
        for channel in self.channels.values() {
            channel.stop().await?;
        }
        tracing::info!("channel manager stopped");
        Ok(())
    }
}

pub fn build_channel_manager(
    settings: &Settings,
    inbound_tx: mpsc::UnboundedSender<InboundMessage>,
) -> ZeroBotResult<ChannelManager> {
    let mut manager = ChannelManager::new();

    if settings.channels.feishu.enabled {
        let feishu = feishu::FeishuChannel::new(settings.channels.feishu.clone(), inbound_tx);
        manager.register(feishu);
    }

    if manager.channels.is_empty() {
        return Err(ZeroBotError::Config(
            "gateway 未启用任何 channels.*.enabled=true".to_string(),
        ));
    }

    Ok(manager)
}
