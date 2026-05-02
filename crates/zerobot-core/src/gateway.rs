use crate::agent::Agent;
use crate::bus::{InboundMessage, MessageBus, OutboundMessage};
use crate::channel::{build_channel_manager, ChannelManager};
use crate::config::{Settings, ToolApprovalMode};
use crate::cron::{CronJob, CronService};
use crate::error::ZeroBotResult;
use crate::heartbeat::{HeartbeatExecuteHandler, HeartbeatNotifyHandler, HeartbeatService};
use crate::hooks::HookManager;
use crate::plugin::PluginManager;
use crate::provider::ProviderFactory;
use crate::session::{create_session_with_hooks, SessionKind, SessionStore};
use crate::tool::{ToolRegistry, ToolRouteContext};
use futures::FutureExt;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio::time::{timeout, Duration};

#[derive(Clone)]
struct GatewayExecutor {
    settings: Settings,
    store: Arc<dyn SessionStore>,
    tools: ToolRegistry,
    hooks: HookManager,
    cwd: PathBuf,
    provider_factory: ProviderFactory,
    model: String,
    tool_approvals: Arc<RwLock<HashSet<String>>>,
    plugins: Option<Arc<PluginManager>>,
    outbound: mpsc::UnboundedSender<OutboundMessage>,
    sessions: Arc<Mutex<HashMap<String, String>>>,
}

impl GatewayExecutor {
    async fn get_or_create_session_id(&self, key: &str) -> ZeroBotResult<String> {
        if let Some(id) = self.sessions.lock().await.get(key).cloned() {
            return Ok(id);
        }
        let session = create_session_with_hooks(
            self.store.as_ref(),
            &self.hooks,
            key.to_string(),
            None,
            SessionKind::Main,
        )
        .await?;
        self.sessions
            .lock()
            .await
            .insert(key.to_string(), session.id.clone());
        Ok(session.id)
    }

    async fn run_turn(
        &self,
        session_key: &str,
        input: &str,
        route: Option<ToolRouteContext>,
    ) -> ZeroBotResult<String> {
        let session_id = self.get_or_create_session_id(session_key).await?;
        tracing::debug!(
            "gateway executor run_turn: session_key={}, session_id={}",
            session_key,
            session_id
        );
        let provider = (self.provider_factory)()?;
        let agent = Agent::new(
            provider,
            self.model.clone(),
            self.settings.clone(),
            self.store.clone(),
            self.tools.clone(),
            self.cwd.clone(),
            self.hooks.clone(),
            None,
            self.plugins.clone(),
            self.tool_approvals.clone(),
            route,
            Some(self.outbound.clone()),
            None,
            None,
            None,
            None,
            None,
        );
        agent.run_turn(&session_id, input, None).await
    }
}

pub struct GatewayRuntime {
    shutdown: Arc<AtomicBool>,
    inbound_rx: mpsc::UnboundedReceiver<InboundMessage>,
    channel_manager: ChannelManager,
    cron_service: Arc<CronService>,
    heartbeat_service: HeartbeatService,
    executor: GatewayExecutor,
    plugins: Option<Arc<PluginManager>>,
}

impl GatewayRuntime {
    pub async fn new(
        settings: Settings,
        cwd: PathBuf,
        store: Arc<dyn SessionStore>,
        tools: ToolRegistry,
        hooks: HookManager,
        plugins: Option<Arc<PluginManager>>,
        provider_factory: ProviderFactory,
        model: String,
        tool_approvals: Arc<RwLock<HashSet<String>>>,
    ) -> ZeroBotResult<Self> {
        tracing::info!("gateway runtime initializing");
        let mut gateway_settings = settings.clone();
        gateway_settings.tools.approval.default = ToolApprovalMode::Auto;
        gateway_settings.tools.approval.per_tool.clear();
        for required in ["cron", "message"] {
            if !gateway_settings
                .tools
                .enabled
                .iter()
                .any(|tool| tool == required)
            {
                gateway_settings.tools.enabled.push(required.to_string());
            }
        }

        let (bus, inbound_rx, outbound_rx) = MessageBus::new();
        let inbound_tx = bus.inbound_sender();
        let outbound_tx = bus.outbound_sender();

        let mut channel_manager = build_channel_manager(&gateway_settings, inbound_tx)?;
        channel_manager.start_all(outbound_rx).await?;
        tracing::info!(
            "gateway channels started: {:?}",
            channel_manager.enabled_channels()
        );

        let executor = GatewayExecutor {
            settings: gateway_settings.clone(),
            store,
            tools,
            hooks,
            cwd: cwd.clone(),
            provider_factory,
            model,
            tool_approvals,
            plugins,
            outbound: outbound_tx.clone(),
            sessions: Arc::new(Mutex::new(HashMap::new())),
        };

        let workspace_root = crate::workspace::resolve_workspace_root(&cwd);
        let db_path = crate::workspace::resolve_session_db_path(&workspace_root);
        let cron_export = gateway_settings
            .gateway
            .cron
            .export_json
            .as_ref()
            .map(PathBuf::from)
            .or_else(|| Some(workspace_root.join(".zerobot").join("cron-jobs.json")));
        let cron_service = Arc::new(
            CronService::new(
                db_path,
                cron_export,
                gateway_settings.gateway.cron.run_history_limit,
            )
            .await?,
        );

        {
            let executor_for_cron = executor.clone();
            let outbound_for_cron = outbound_tx.clone();
            cron_service.set_handler(Some(Arc::new(move |job: CronJob| {
                let executor = executor_for_cron.clone();
                let outbound = outbound_for_cron.clone();
                async move {
                    let route = job.payload.channel.clone().zip(job.payload.to.clone()).map(
                        |(channel, chat_id)| ToolRouteContext {
                            channel,
                            chat_id,
                            message_id: None,
                        },
                    );
                    let output = executor
                        .run_turn(
                            &format!("cron:{}", job.id),
                            &job.payload.message,
                            route.clone(),
                        )
                        .await?;
                    if job.payload.deliver && !output.trim().is_empty() {
                        if let (Some(channel), Some(to)) = (job.payload.channel, job.payload.to) {
                            let _ = outbound.send(OutboundMessage::new(channel, to, output));
                        }
                    }
                    Ok(())
                }
                .boxed()
            })));
        }
        cron_service.start().await?;
        tracing::info!("gateway cron service started");

        let heartbeat_service = {
            let hb_cfg = &gateway_settings.gateway.heartbeat;
            let target = hb_cfg.target.clone();
            let executor_for_hb = executor.clone();
            let route_target = target.clone();
            let exec: HeartbeatExecuteHandler = Arc::new(move |tasks: String| {
                let executor = executor_for_hb.clone();
                let target = route_target.clone();
                async move {
                    let route = target.clone().map(|t| ToolRouteContext {
                        channel: t.channel,
                        chat_id: t.chat_id,
                        message_id: None,
                    });
                    executor.run_turn("heartbeat", &tasks, route).await
                }
                .boxed()
            });
            let notify: Option<HeartbeatNotifyHandler> = target.map(|target| {
                let outbound = outbound_tx.clone();
                Arc::new(move |response: String| {
                    let outbound = outbound.clone();
                    let target = target.clone();
                    async move {
                        let _ = outbound.send(OutboundMessage::new(
                            target.channel.clone(),
                            target.chat_id.clone(),
                            response,
                        ));
                        Ok(())
                    }
                    .boxed()
                }) as HeartbeatNotifyHandler
            });
            HeartbeatService::new(
                cwd.clone(),
                executor.provider_factory.clone(),
                executor.model.clone(),
                hb_cfg.file.clone(),
                hb_cfg.interval_s,
                hb_cfg.enabled,
                Some(exec),
                notify,
            )
        };
        heartbeat_service.start().await?;
        tracing::info!(
            "gateway heartbeat service started: enabled={}",
            gateway_settings.gateway.heartbeat.enabled
        );

        Ok(Self {
            shutdown: Arc::new(AtomicBool::new(false)),
            inbound_rx,
            channel_manager,
            cron_service,
            heartbeat_service,
            plugins: executor.plugins.clone(),
            executor,
        })
    }

    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }

    pub async fn run(&mut self) -> ZeroBotResult<()> {
        tracing::info!("gateway event loop started");
        while !self.shutdown.load(Ordering::SeqCst) {
            let recv = timeout(Duration::from_millis(500), self.inbound_rx.recv()).await;
            let msg = match recv {
                Ok(Some(msg)) => msg,
                Ok(None) => break,
                Err(_) => continue,
            };
            tracing::info!(
                "gateway inbound received: source={}, channel={}, chat_id={}, session_key={}",
                msg.source,
                msg.channel,
                msg.chat_id,
                msg.session_key
            );

            let route = Some(ToolRouteContext {
                channel: msg.channel.clone(),
                chat_id: msg.chat_id.clone(),
                message_id: msg
                    .metadata
                    .get("message_id")
                    .and_then(|v| v.as_str())
                    .map(ToString::to_string),
            });

            match self
                .executor
                .run_turn(&msg.session_key, &msg.content, route)
                .await
            {
                Ok(output) => {
                    if !output.trim().is_empty() {
                        tracing::debug!(
                            "gateway outbound generated: channel={}, chat_id={}, bytes={}",
                            msg.channel,
                            msg.chat_id,
                            output.len()
                        );
                        let _ = self.executor.outbound.send(OutboundMessage {
                            channel: msg.channel,
                            chat_id: msg.chat_id,
                            content: output,
                            media: Vec::new(),
                            metadata: msg.metadata,
                        });
                    }
                }
                Err(err) => {
                    tracing::warn!(
                        "gateway run_turn failed: channel={}, chat_id={}, err={}",
                        msg.channel,
                        msg.chat_id,
                        err
                    );
                    let _ = self.executor.outbound.send(OutboundMessage::new(
                        msg.channel,
                        msg.chat_id,
                        format!("处理失败: {err}"),
                    ));
                }
            }
        }
        tracing::info!("gateway event loop stopped");
        self.stop().await
    }

    pub async fn stop(&mut self) -> ZeroBotResult<()> {
        tracing::info!("gateway stopping");
        self.shutdown.store(true, Ordering::SeqCst);
        self.heartbeat_service.stop().await;
        self.cron_service.stop().await;
        self.channel_manager.stop_all().await?;
        if let Some(plugins) = &self.plugins {
            plugins.shutdown().await;
        }
        tracing::info!("gateway stopped");
        Ok(())
    }
}
