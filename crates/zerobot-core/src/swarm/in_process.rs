use async_trait::async_trait;
use std::path::PathBuf;
use std::sync::Arc;

use super::{BackendType, TeammateBackend, TeammateConfig, TeammateHandle};
use super::mailbox::Mailbox;
use crate::agent_dispatch::AgentDispatcher;
use crate::error::{ZeroBotError, ZeroBotResult};
use crate::task::TaskManager;

/// 同进程 teammate 后端
pub struct InProcessBackend {
    dispatcher: Arc<AgentDispatcher>,
    task_manager: Arc<TaskManager>,
    mailbox: Mailbox,
}

impl InProcessBackend {
    pub fn new(
        dispatcher: Arc<AgentDispatcher>,
        task_manager: Arc<TaskManager>,
        mailbox_dir: PathBuf,
    ) -> Self {
        Self {
            dispatcher,
            task_manager,
            mailbox: Mailbox::new(mailbox_dir),
        }
    }
}

#[async_trait]
impl TeammateBackend for InProcessBackend {
    async fn spawn(&self, config: TeammateConfig) -> ZeroBotResult<TeammateHandle> {
        let request = crate::agent_dispatch::DispatchRequest {
            agent_type: config.agent_type,
            prompt: config.prompt,
            mode: crate::agent_dispatch::DispatchMode::Background {
                name: Some(config.agent_name.clone()),
            },
            model_override: config.model,
            tool_overrides: None,
            cwd: config.cwd,
            max_turns: None,
            isolation: None,
            depth: None,
        };

        let result = self.dispatcher.dispatch(request).await?;
        let task_id = match result {
            crate::agent_dispatch::DispatchResult::Background(id) => id,
            _ => return Err(ZeroBotError::Swarm("预期后台分发结果".to_string())),
        };

        Ok(TeammateHandle {
            agent_name: config.agent_name,
            team_name: config.team_name,
            backend_type: BackendType::InProcess,
            task_id,
        })
    }

    async fn send_message(&self, handle: &TeammateHandle, message: String) -> ZeroBotResult<()> {
        self.mailbox.send(&handle.agent_name, &handle.team_name, &message)
    }

    async fn terminate(&self, handle: &TeammateHandle) -> ZeroBotResult<()> {
        self.task_manager.cancel(&handle.task_id).await;
        Ok(())
    }

    async fn is_active(&self, handle: &TeammateHandle) -> ZeroBotResult<bool> {
        if let Some(state) = self.task_manager.get_state(&handle.task_id).await {
            Ok(matches!(state.status, crate::task::TaskStatus::Pending | crate::task::TaskStatus::Running))
        } else {
            Ok(false)
        }
    }
}
