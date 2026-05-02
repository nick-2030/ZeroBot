use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

/// 任务 ID，前缀区分类型: "a_" agent, "b_" shell, "c_" cron, "w_" workflow
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskId(String);

impl TaskId {
    pub fn new(prefix: &str) -> Self {
        Self(format!("{}{}", prefix, uuid::Uuid::new_v4().as_simple()))
    }

    pub fn from_string(s: String) -> Self {
        Self(s)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for TaskId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TaskType {
    Agent,
    Shell,
    Cron,
    Workflow,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TaskStatus {
    Pending,
    Running,
    Completed,
    Failed(String),
    Killed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskState {
    pub id: TaskId,
    pub task_type: TaskType,
    pub status: TaskStatus,
    pub description: String,
    pub parent_task_id: Option<TaskId>,
    pub agent_type: Option<String>,
    #[serde(skip)]
    pub started_at: Option<Instant>,
    pub output_file: Option<PathBuf>,
    pub notified: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskUsage {
    pub total_tokens: u64,
    pub tool_uses: u32,
    pub duration_ms: u64,
}

/// 管理所有活跃任务的生命周期
pub struct TaskManager {
    tasks: Arc<RwLock<HashMap<TaskId, TaskState>>>,
    abort_tokens: Arc<RwLock<HashMap<TaskId, CancellationToken>>>,
}

impl TaskManager {
    pub fn new() -> Self {
        Self {
            tasks: Arc::new(RwLock::new(HashMap::new())),
            abort_tokens: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// 注册新任务，返回 TaskId 和 CancellationToken
    pub async fn register(
        &self,
        task_type: TaskType,
        description: String,
        parent: Option<TaskId>,
        agent_type: Option<String>,
    ) -> (TaskId, CancellationToken) {
        let prefix = match task_type {
            TaskType::Agent => "a_",
            TaskType::Shell => "b_",
            TaskType::Cron => "c_",
            TaskType::Workflow => "w_",
        };
        let id = TaskId::new(prefix);
        let cancel_token = CancellationToken::new();

        let state = TaskState {
            id: id.clone(),
            task_type,
            status: TaskStatus::Pending,
            description,
            parent_task_id: parent,
            agent_type,
            started_at: None,
            output_file: None,
            notified: false,
        };

        self.tasks.write().await.insert(id.clone(), state);
        self.abort_tokens
            .write()
            .await
            .insert(id.clone(), cancel_token.clone());

        (id, cancel_token)
    }

    /// 更新任务状态
    pub async fn update_status(&self, id: &TaskId, status: TaskStatus) {
        if let Some(task) = self.tasks.write().await.get_mut(id) {
            task.status = status;
        }
    }

    /// 标记任务已通知
    pub async fn mark_notified(&self, id: &TaskId) {
        if let Some(task) = self.tasks.write().await.get_mut(id) {
            task.notified = true;
        }
    }

    /// 获取任务状态
    pub async fn get_state(&self, id: &TaskId) -> Option<TaskState> {
        self.tasks.read().await.get(id).cloned()
    }

    /// 按状态列出任务
    pub async fn list_by_status(&self, status: &TaskStatus) -> Vec<TaskState> {
        self.tasks
            .read()
            .await
            .values()
            .filter(|t| std::mem::discriminant(&t.status) == std::mem::discriminant(status))
            .cloned()
            .collect()
    }

    /// 列出某个父任务的所有子任务
    pub async fn list_children(&self, parent_id: &TaskId) -> Vec<TaskState> {
        self.tasks
            .read()
            .await
            .values()
            .filter(|t| t.parent_task_id.as_ref() == Some(parent_id))
            .cloned()
            .collect()
    }

    /// 取消指定任务
    pub async fn cancel(&self, id: &TaskId) {
        if let Some(token) = self.abort_tokens.read().await.get(id) {
            token.cancel();
        }
        self.update_status(id, TaskStatus::Killed).await;
    }

    /// 取消所有任务
    pub async fn cancel_all(&self) {
        let tokens = self.abort_tokens.read().await;
        for token in tokens.values() {
            token.cancel();
        }
        drop(tokens);

        let mut tasks = self.tasks.write().await;
        for task in tasks.values_mut() {
            if matches!(task.status, TaskStatus::Pending | TaskStatus::Running) {
                task.status = TaskStatus::Killed;
            }
        }
    }

    /// 移除已完成/失败/终止的任务
    pub async fn cleanup(&self, id: &TaskId) {
        self.tasks.write().await.remove(id);
        self.abort_tokens.write().await.remove(id);
    }
}

impl Default for TaskManager {
    fn default() -> Self {
        Self::new()
    }
}
