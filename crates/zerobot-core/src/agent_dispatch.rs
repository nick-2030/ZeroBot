use std::path::PathBuf;
use std::sync::Arc;

use crate::agent::{Agent, AgentResult};
use crate::agents::AgentManager;
use crate::error::{ZeroBotError, ZeroBotResult};
use crate::hooks::HookManager;
use crate::interaction::InteractionHandler;
use crate::notification::NotificationBus;
use crate::provider::ProviderFactory;
use crate::session::{create_session_with_hooks, SessionKind, SessionStore};
use crate::task::{TaskId, TaskManager};
use crate::tool::ToolRegistry;
use tokio::sync::RwLock;

/// Agent 分发模式
#[derive(Debug, Clone)]
pub enum DispatchMode {
    Sync,
    Background { name: Option<String> },
    Fork,
    Teammate { team_name: String },
}

/// 工具限制
#[derive(Debug, Clone)]
pub struct ToolRestrictions {
    pub allowlist: Option<Vec<String>>,
    pub blocklist: Option<Vec<String>>,
}

/// 隔离模式
#[derive(Debug, Clone)]
pub enum IsolationMode {
    Worktree,
    Remote,
}

/// Agent 分发请求
#[derive(Debug, Clone)]
pub struct DispatchRequest {
    pub agent_type: String,
    pub prompt: String,
    pub mode: DispatchMode,
    pub model_override: Option<String>,
    pub tool_overrides: Option<ToolRestrictions>,
    pub cwd: Option<PathBuf>,
    pub max_turns: Option<u32>,
    pub isolation: Option<IsolationMode>,
    pub depth: Option<u32>,
}

/// Agent 分发结果
#[derive(Debug)]
pub enum DispatchResult {
    Sync(AgentResult),
    Background(TaskId),
    Fork(TaskId),
    Teammate { agent_name: String, team_name: String },
}

/// 统一的 Agent 分发器
pub struct AgentDispatcher {
    task_manager: Arc<TaskManager>,
    agent_manager: Arc<AgentManager>,
    notification_bus: Arc<NotificationBus>,
    settings: crate::config::Settings,
    provider_factory: ProviderFactory,
    fallback_model: String,
    store: Arc<dyn SessionStore>,
    cwd: PathBuf,
    hooks: HookManager,
    interaction: Option<Arc<dyn InteractionHandler>>,
    tool_approvals: Arc<RwLock<std::collections::HashSet<String>>>,
}

impl AgentDispatcher {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        task_manager: Arc<TaskManager>,
        agent_manager: Arc<AgentManager>,
        notification_bus: Arc<NotificationBus>,
        settings: crate::config::Settings,
        provider_factory: ProviderFactory,
        fallback_model: String,
        store: Arc<dyn SessionStore>,
        cwd: PathBuf,
        hooks: HookManager,
        interaction: Option<Arc<dyn InteractionHandler>>,
        tool_approvals: Arc<RwLock<std::collections::HashSet<String>>>,
    ) -> Self {
        Self {
            task_manager,
            agent_manager,
            notification_bus,
            settings,
            provider_factory,
            fallback_model,
            store,
            cwd,
            hooks,
            interaction,
            tool_approvals,
        }
    }

    pub async fn dispatch(&self, request: DispatchRequest) -> ZeroBotResult<DispatchResult> {
        let def = self.agent_manager.load(&request.agent_type)?;

        let model = request
            .model_override
            .or(def.model.clone())
            .unwrap_or_else(|| self.fallback_model.clone());

        let provider = (self.provider_factory)()?;

        let tools = ToolRegistry::with_builtin();

        let cwd = request.cwd.clone().unwrap_or_else(|| self.cwd.clone());

        match request.mode {
            DispatchMode::Sync => {
                let agent = Agent::new(
                    provider,
                    model,
                    self.settings.clone(),
                    self.store.clone(),
                    tools,
                    cwd.clone(),
                    self.hooks.clone(),
                    self.interaction.clone(),
                    None,
                    self.tool_approvals.clone(),
                    None,
                    None,
                    None,
                    None,
                    None,
                    request.max_turns,
                    None,
                );

                let session = create_session_with_hooks(
                    &*self.store,
                    &self.hooks,
                    def.name.clone(),
                    None,
                    SessionKind::Sub,
                )
                .await?;

                let result = agent.run_turn(&session.id, &request.prompt, None).await;
                match result {
                    Ok(msg) => Ok(DispatchResult::Sync(AgentResult::Done {
                        message: msg,
                        usage: Default::default(),
                    })),
                    Err(e) => Ok(DispatchResult::Sync(AgentResult::Failed(e.to_string()))),
                }
            }
            DispatchMode::Background { .. } => {
                let agent = Agent::new(
                    provider,
                    model,
                    self.settings.clone(),
                    self.store.clone(),
                    tools,
                    cwd,
                    self.hooks.clone(),
                    self.interaction.clone(),
                    None,
                    self.tool_approvals.clone(),
                    None,
                    None,
                    None,
                    None,
                    Some(request.agent_type.clone()),
                    request.max_turns,
                    None,
                );

                let session = create_session_with_hooks(
                    &*self.store,
                    &self.hooks,
                    def.name.clone(),
                    None,
                    SessionKind::Sub,
                )
                .await?;

                let task_id = agent
                    .run_background(
                        session.id,
                        request.prompt,
                        self.task_manager.clone(),
                        self.notification_bus.clone(),
                        None,
                    )
                    .await?;

                Ok(DispatchResult::Background(task_id))
            }
            DispatchMode::Fork => {
                Err(ZeroBotError::Agent("Fork 模式尚未实现".to_string()))
            }
            DispatchMode::Teammate { .. } => {
                Err(ZeroBotError::Agent("Teammate 模式尚未实现".to_string()))
            }
        }
    }

    pub async fn send_message(&self, _task_id: &TaskId, _message: String) -> ZeroBotResult<()> {
        Err(ZeroBotError::Agent("SendMessage 尚未实现".to_string()))
    }

    pub async fn terminate(&self, task_id: &TaskId) -> ZeroBotResult<()> {
        self.task_manager.cancel(task_id).await;
        Ok(())
    }

    pub fn task_manager(&self) -> &Arc<TaskManager> {
        &self.task_manager
    }

    pub fn notification_bus(&self) -> &Arc<NotificationBus> {
        &self.notification_bus
    }
}
