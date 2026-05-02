pub mod in_process;
pub mod mailbox;
pub mod tools;

use async_trait::async_trait;
use crate::error::ZeroBotResult;
use crate::task::TaskId;

/// Teammate 配置
#[derive(Debug, Clone)]
pub struct TeammateConfig {
    pub agent_name: String,
    pub team_name: String,
    pub agent_type: String,
    pub prompt: String,
    pub model: Option<String>,
    pub cwd: Option<std::path::PathBuf>,
}

/// Teammate 句柄
#[derive(Debug, Clone)]
pub struct TeammateHandle {
    pub agent_name: String,
    pub team_name: String,
    pub backend_type: BackendType,
    pub task_id: TaskId,
}

/// 后端类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BackendType {
    InProcess,
    Tmux,
    External,
}

/// Teammate 后端 trait
#[async_trait]
pub trait TeammateBackend: Send + Sync {
    async fn spawn(&self, config: TeammateConfig) -> ZeroBotResult<TeammateHandle>;
    async fn send_message(&self, handle: &TeammateHandle, message: String) -> ZeroBotResult<()>;
    async fn terminate(&self, handle: &TeammateHandle) -> ZeroBotResult<()>;
    async fn is_active(&self, handle: &TeammateHandle) -> ZeroBotResult<bool>;
}

/// Swarm 管理器
pub struct SwarmManager {
    backends: std::collections::HashMap<BackendType, Box<dyn TeammateBackend>>,
    default_backend: BackendType,
    active_teammates: tokio::sync::RwLock<std::collections::HashMap<String, TeammateHandle>>,
}

impl SwarmManager {
    pub fn new(default_backend: BackendType) -> Self {
        Self {
            backends: std::collections::HashMap::new(),
            default_backend,
            active_teammates: tokio::sync::RwLock::new(std::collections::HashMap::new()),
        }
    }

    pub fn register_backend(&mut self, backend_type: BackendType, backend: Box<dyn TeammateBackend>) {
        self.backends.insert(backend_type, backend);
    }

    pub async fn spawn_teammate(&self, config: TeammateConfig) -> ZeroBotResult<TeammateHandle> {
        let backend = self.backends.get(&self.default_backend)
            .ok_or_else(|| crate::error::ZeroBotError::Swarm(format!("后端 {:?} 未注册", self.default_backend)))?;

        let handle = backend.spawn(config.clone()).await?;
        let key = format!("{}@{}", config.agent_name, config.team_name);
        self.active_teammates.write().await.insert(key, handle.clone());
        Ok(handle)
    }

    pub async fn send_message(&self, handle: &TeammateHandle, message: String) -> ZeroBotResult<()> {
        let backend = self.backends.get(&handle.backend_type)
            .ok_or_else(|| crate::error::ZeroBotError::Swarm(format!("后端 {:?} 未注册", handle.backend_type)))?;
        backend.send_message(handle, message).await
    }

    pub async fn terminate(&self, handle: &TeammateHandle) -> ZeroBotResult<()> {
        let backend = self.backends.get(&handle.backend_type)
            .ok_or_else(|| crate::error::ZeroBotError::Swarm(format!("后端 {:?} 未注册", handle.backend_type)))?;
        backend.terminate(handle).await?;

        let key = format!("{}@{}", handle.agent_name, handle.team_name);
        self.active_teammates.write().await.remove(&key);
        Ok(())
    }

    pub async fn list_active(&self) -> Vec<TeammateHandle> {
        self.active_teammates.read().await.values().cloned().collect()
    }
}
