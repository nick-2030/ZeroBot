# ZeroBot 多智能体重构 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将 ZeroBot 从简单同步 subagent 模式升级为支持后台执行、agent 间协调、编排模式和团队协作的完整多智能体框架。

**Architecture:** 引入 TaskManager 统一管理后台任务，AgentDispatcher 提供 sync/async/fork/teammate 四种分发路径，NotificationBus 实现进程内通知，工具系统增加 toolset 分组。

**Tech Stack:** Rust, tokio, tokio-util (CancellationToken), sqlx (SQLite), serde, async-trait

**Spec:** `docs/superpowers/specs/2026-05-02-multi-agent-redesign-design.md`

---

## Phase 1: 基础设施

### Task 1: 添加 tokio-util 依赖

**Files:**
- Modify: `Cargo.toml` (workspace root)

- [ ] **Step 1: 在 workspace Cargo.toml 添加 tokio-util 依赖**

在 `/Volumes/nick-disk/projects/ai/ZeroBot/Cargo.toml` 的 `[workspace.dependencies]` 中添加：

```toml
tokio-util = { version = "0.7", features = ["rt"] }
```

在 `/Volumes/nick-disk/projects/ai/ZeroBot/crates/zerobot-core/Cargo.toml` 的 `[dependencies]` 中添加：

```toml
tokio-util = { workspace = true }
```

- [ ] **Step 2: 验证依赖可用**

Run: `cd /Volumes/nick-disk/projects/ai/ZeroBot && cargo check -p zerobot-core`
Expected: 编译通过（可能有 warning 但无 error）

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml crates/zerobot-core/Cargo.toml
git commit -m "chore: 添加 tokio-util 依赖用于 CancellationToken"
```

---

### Task 2: 新增错误变体

**Files:**
- Modify: `crates/zerobot-core/src/error.rs:6-25`

- [ ] **Step 1: 添加新的错误变体**

在 `ZeroBotError` enum 中添加：

```rust
#[error("任务错误: {0}")]
Task(String),

#[error("编排深度超限: {0}")]
OrchestrationDepthExceeded(String),

#[error("通知错误: {0}")]
Notification(String),

#[error("Kanban 错误: {0}")]
Kanban(String),

#[error("Swarm 错误: {0}")]
Swarm(String),
```

- [ ] **Step 2: 验证编译**

Run: `cargo check -p zerobot-core`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add crates/zerobot-core/src/error.rs
git commit -m "feat: 添加 Task/Kanban/Swarm 相关错误变体"
```

---

### Task 3: Task 状态机模块

**Files:**
- Create: `crates/zerobot-core/src/task.rs`
- Modify: `crates/zerobot-core/src/lib.rs:1-30`

- [ ] **Step 1: 创建 task.rs**

创建 `/Volumes/nick-disk/projects/ai/ZeroBot/crates/zerobot-core/src/task.rs`：

```rust
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::error::{ZeroBotError, ZeroBotResult};

/// 任务 ID，前缀区分类型: "a_" agent, "b_" shell, "c_" cron, "w_" workflow
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskId(String);

impl TaskId {
    pub fn new(prefix: &str) -> Self {
        Self(format!("{}{}", prefix, uuid::Uuid::new_v4().as_simple()))
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
```

- [ ] **Step 2: 在 lib.rs 注册模块**

在 `crates/zerobot-core/src/lib.rs` 的模块声明区（约 line 30）添加：

```rust
pub mod task;
```

在 re-exports 区添加：

```rust
pub use task::{TaskId, TaskManager, TaskState, TaskStatus, TaskType, TaskUsage};
```

- [ ] **Step 3: 验证编译**

Run: `cargo check -p zerobot-core`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add crates/zerobot-core/src/task.rs crates/zerobot-core/src/lib.rs
git commit -m "feat: 添加 Task 状态机模块 (TaskId, TaskState, TaskManager)"
```

---

### Task 4: 通知系统模块

**Files:**
- Create: `crates/zerobot-core/src/notification.rs`
- Modify: `crates/zerobot-core/src/lib.rs`

- [ ] **Step 1: 创建 notification.rs**

创建 `/Volumes/nick-disk/projects/ai/ZeroBot/crates/zerobot-core/src/notification.rs`：

```rust
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::{mpsc, RwLock};

use crate::task::{TaskId, TaskUsage};

#[derive(Debug, Clone)]
pub struct Notification {
    pub task_id: TaskId,
    pub agent_type: String,
    pub description: String,
    pub status: NotificationStatus,
    pub result: Option<String>,
    pub usage: Option<TaskUsage>,
    pub timestamp: Instant,
}

#[derive(Debug, Clone)]
pub enum NotificationStatus {
    Completed,
    Failed(String),
    Killed,
    Progress { summary: String },
}

pub type NotificationSender = mpsc::UnboundedSender<Notification>;
pub type NotificationReceiver = mpsc::UnboundedReceiver<Notification>;

/// 进程内通知总线，按 parent_task_id 分发通知
pub struct NotificationBus {
    channels: Arc<RwLock<HashMap<TaskId, NotificationSender>>>,
}

impl NotificationBus {
    pub fn new() -> Self {
        Self {
            channels: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// 注册一个 parent task 来接收通知
    pub async fn subscribe(&self, parent_task_id: TaskId) -> NotificationReceiver {
        let (tx, rx) = mpsc::unbounded_channel();
        self.channels.write().await.insert(parent_task_id, tx);
        rx
    }

    /// 发送通知给指定 parent
    pub async fn notify(&self, parent_task_id: &TaskId, notification: Notification) {
        if let Some(tx) = self.channels.read().await.get(parent_task_id) {
            let _ = tx.send(notification);
        }
    }

    /// 取消订阅
    pub async fn unsubscribe(&self, parent_task_id: &TaskId) {
        self.channels.write().await.remove(parent_task_id);
    }
}

impl Default for NotificationBus {
    fn default() -> Self {
        Self::new()
    }
}

/// 将通知格式化为结构化消息，注入到 agent 的消息流中
pub fn format_notification_as_message(notification: &Notification) -> String {
    let status_str = match &notification.status {
        NotificationStatus::Completed => "completed",
        NotificationStatus::Failed(reason) => {
            return format!(
                "<task-notification>\n<task-id>{}</task-id>\n<agent-type>{}</agent-type>\n<status>failed</status>\n<error>{}</error>\n</task-notification>",
                notification.task_id, notification.agent_type, reason
            );
        }
        NotificationStatus::Killed => "killed",
        NotificationStatus::Progress { summary } => {
            return format!(
                "<task-notification>\n<task-id>{}</task-id>\n<agent-type>{}</agent-type>\n<status>progress</status>\n<summary>{}</summary>\n</task-notification>",
                notification.task_id, notification.agent_type, summary
            );
        }
    };

    let mut msg = format!(
        "<task-notification>\n<task-id>{}</task-id>\n<agent-type>{}</agent-type>\n<status>{}</status>",
        notification.task_id, notification.agent_type, status_str
    );

    if let Some(desc) = Some(&notification.description) {
        msg.push_str(&format!("\n<description>{}</description>", desc));
    }

    if let Some(result) = &notification.result {
        msg.push_str(&format!("\n<result>{}</result>", result));
    }

    if let Some(usage) = &notification.usage {
        msg.push_str(&format!(
            "\n<usage>\n  <total_tokens>{}</total_tokens>\n  <tool_uses>{}</tool_uses>\n  <duration_ms>{}</duration_ms>\n</usage>",
            usage.total_tokens, usage.tool_uses, usage.duration_ms
        ));
    }

    msg.push_str("\n</task-notification>");
    msg
}
```

- [ ] **Step 2: 在 lib.rs 注册模块**

在 `crates/zerobot-core/src/lib.rs` 的模块声明区添加：

```rust
pub mod notification;
```

在 re-exports 区添加：

```rust
pub use notification::{Notification, NotificationBus, NotificationSender, NotificationStatus};
```

- [ ] **Step 3: 验证编译**

Run: `cargo check -p zerobot-core`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add crates/zerobot-core/src/notification.rs crates/zerobot-core/src/lib.rs
git commit -m "feat: 添加通知系统模块 (NotificationBus, NotificationSender)"
```

---

### Task 5: Agent struct 新增字段

**Files:**
- Modify: `crates/zerobot-core/src/agent.rs:89-103` (Agent struct)
- Modify: `crates/zerobot-core/src/agent.rs:106-118` (Agent::new)

- [ ] **Step 1: 在 Agent struct 添加新字段**

在 `crates/zerobot-core/src/agent.rs` 的 `Agent` struct（line 89）中，在 `denial_counts: DenialCounts,` 之后添加：

```rust
    // 多智能体支持
    task_id: Option<TaskId>,
    parent_task_id: Option<TaskId>,
    abort_token: CancellationToken,
    notification_tx: Option<NotificationSender>,
    agent_type: String,
    iteration_budget: Option<u32>,
```

在文件顶部 imports 区添加：

```rust
use tokio_util::sync::CancellationToken;
use crate::notification::{NotificationSender, NotificationBus, Notification, NotificationStatus, format_notification_as_message};
use crate::task::{TaskId, TaskManager, TaskStatus, TaskUsage};
```

- [ ] **Step 2: 更新 Agent::new() 构造函数**

在 `Agent::new()`（line 106）的参数列表末尾添加新参数：

```rust
    task_id: Option<TaskId>,
    parent_task_id: Option<TaskId>,
    agent_type: Option<String>,
    iteration_budget: Option<u32>,
    notification_tx: Option<NotificationSender>,
```

在函数体中初始化新字段（在 `denial_counts` 之后）：

```rust
        task_id,
        parent_task_id,
        abort_token: CancellationToken::new(),
        notification_tx,
        agent_type: agent_type.unwrap_or_else(|| "default".to_string()),
        iteration_budget,
```

- [ ] **Step 3: 更新所有 Agent::new() 的调用方**

搜索代码库中所有调用 `Agent::new(` 的地方，添加新的参数。需要更新的位置（通过 grep 确认）：

1. `crates/zerobot-core/src/tool.rs` 中 SubagentTool::run() 创建 Agent 的地方
2. `crates/zerobot-core/src/gateway.rs` 中 GatewayExecutor::run_turn() 创建 Agent 的地方
3. 其他可能的调用方

对每个调用方，添加参数 `None, None, None, None, None`（使用默认值）。

- [ ] **Step 4: 验证编译**

Run: `cargo check -p zerobot-core`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/zerobot-core/src/agent.rs crates/zerobot-core/src/tool.rs crates/zerobot-core/src/gateway.rs
git commit -m "feat: Agent struct 新增 task_id, abort_token, notification_tx 等字段"
```

---

### Task 6: Agent::run_turn 增加中断检查和迭代预算

**Files:**
- Modify: `crates/zerobot-core/src/agent.rs:232-236` (主循环开始处)

- [ ] **Step 1: 在 run_turn 主循环添加中断检查**

在 `agent.rs` 的 `run_turn()` 方法中，主循环 `loop {` （line 232）之后、step counter 之前添加：

```rust
            // 检查中断令牌
            if self.abort_token.is_cancelled() {
                let _ = self.emit(&events, AgentEvent::Stop).await;
                return Ok("任务被中断".to_string());
            }
```

- [ ] **Step 2: 添加迭代预算检查**

在 step counter 检查（line 233-236）之后添加迭代预算检查：

```rust
            // 迭代预算检查
            if let Some(budget) = self.iteration_budget {
                if step >= budget {
                    let _ = self.emit(&events, AgentEvent::Stop).await;
                    return Ok(format!("迭代预算耗尽 ({} 步)", budget));
                }
            }
```

- [ ] **Step 3: 在 tool 执行后发送进度通知**

在 tool call 执行完毕后（line 708 附近，`partition_tool_calls` 执行完之后）添加：

```rust
            // 发送进度通知
            if let Some(ref tx) = self.notification_tx {
                let notification = Notification {
                    task_id: self.task_id.clone().unwrap_or_else(|| TaskId::new("a_")),
                    agent_type: self.agent_type.clone(),
                    description: format!("已完成 {} 步", step),
                    status: NotificationStatus::Progress {
                        summary: format!("步骤 {} 完成", step),
                    },
                    result: None,
                    usage: None,
                    timestamp: std::time::Instant::now(),
                };
                let _ = tx.send(notification);
            }
```

- [ ] **Step 4: 验证编译**

Run: `cargo check -p zerobot-core`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/zerobot-core/src/agent.rs
git commit -m "feat: Agent::run_turn 增加中断检查和迭代预算"
```

---

### Task 7: Agent::run_background 方法

**Files:**
- Modify: `crates/zerobot-core/src/agent.rs` (在 run_turn 之后添加)

- [ ] **Step 1: 添加 AgentResult 枚举**

在 `agent.rs` 中（`Agent` struct 之前）添加：

```rust
#[derive(Debug)]
pub enum AgentResult {
    Done { message: String, usage: TaskUsage },
    Interrupted { reason: String },
    BudgetExhausted { steps_used: u32 },
    Failed(String),
}
```

- [ ] **Step 2: 添加 run_background 方法**

在 `Agent` impl 块中（`run_turn` 之后）添加：

```rust
    /// 后台运行 agent，返回 TaskId
    pub async fn run_background(
        mut self,
        session_id: String,
        input: String,
        task_manager: Arc<TaskManager>,
        notification_bus: Arc<NotificationBus>,
        parent_task_id: Option<TaskId>,
    ) -> ZeroBotResult<TaskId> {
        let (task_id, cancel_token) = task_manager
            .register(
                crate::task::TaskType::Agent,
                input.clone(),
                parent_task_id.clone(),
                Some(self.agent_type.clone()),
            )
            .await;

        // 注入 task 相关字段
        self.task_id = Some(task_id.clone());
        self.parent_task_id = parent_task_id.clone();
        self.abort_token = cancel_token;

        // 创建通知发送器
        let (notification_tx, mut notification_rx) = tokio::sync::mpsc::unbounded_channel();
        self.notification_tx = Some(notification_tx);

        // 如果有 parent，订阅通知总线
        let notification_bus_clone = notification_bus.clone();
        let task_id_clone = task_id.clone();

        task_manager.update_status(&task_id, TaskStatus::Running).await;

        // spawn 后台任务
        let agent_type = self.agent_type.clone();
        tokio::spawn(async move {
            let result = self.run_turn(&session_id, &input, None).await;

            let (status, result_msg) = match &result {
                Ok(msg) => (TaskStatus::Completed, Some(msg.clone())),
                Err(e) => (TaskStatus::Failed(e.to_string()), Some(e.to_string())),
            };

            task_manager.update_status(&task_id_clone, status).await;

            // 通过通知总线通知 parent
            if let Some(ref parent_id) = parent_task_id {
                let notification = Notification {
                    task_id: task_id_clone.clone(),
                    agent_type,
                    description: input,
                    status: match result {
                        Ok(_) => NotificationStatus::Completed,
                        Err(ref e) => NotificationStatus::Failed(e.to_string()),
                    },
                    result: result_msg,
                    usage: None,
                    timestamp: std::time::Instant::now(),
                };
                notification_bus_clone.notify(parent_id, notification).await;
            }
        });

        Ok(task_id)
    }

    /// 中断 agent
    pub fn abort(&self) {
        self.abort_token.cancel();
    }

    /// 获取 TaskId
    pub fn task_id(&self) -> Option<&TaskId> {
        self.task_id.as_ref()
    }
```

- [ ] **Step 3: 验证编译**

Run: `cargo check -p zerobot-core`
Expected: PASS（可能有 unused warning）

- [ ] **Step 4: Commit**

```bash
git add crates/zerobot-core/src/agent.rs
git commit -m "feat: 添加 Agent::run_background() 后台执行方法和 AgentResult"
```

---

## Phase 2: Agent 分发

### Task 8: AgentDispatcher 模块

**Files:**
- Create: `crates/zerobot-core/src/agent_dispatch.rs`
- Modify: `crates/zerobot-core/src/lib.rs`

- [ ] **Step 1: 创建 agent_dispatch.rs**

创建 `/Volumes/nick-disk/projects/ai/ZeroBot/crates/zerobot-core/src/agent_dispatch.rs`：

```rust
use std::path::PathBuf;
use std::sync::Arc;

use crate::agent::{Agent, AgentResult};
use crate::agents::{AgentDefinition, AgentManager};
use crate::error::{ZeroBotError, ZeroBotResult};
use crate::notification::{NotificationBus, NotificationSender};
use crate::task::{TaskId, TaskManager};
use crate::tool::ToolRegistry;

/// Agent 分发模式
#[derive(Debug, Clone)]
pub enum DispatchMode {
    /// 同步阻塞等待结果
    Sync,
    /// 后台执行，结果通过通知回报
    Background { name: Option<String> },
    /// 继承父上下文，后台执行（TODO: Phase 2 后续）
    Fork,
    /// 团队模式（TODO: Phase 6 Swarm）
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
    /// 编排深度（Task 15 会添加此字段，初始阶段可省略）
    pub depth: Option<u32>,
}

/// Agent 分发结果
#[derive(Debug)]
pub enum DispatchResult {
    Sync(AgentResult),
    Background(TaskId),
    Fork(TaskId),
    Teammate {
        agent_name: String,
        team_name: String,
    },
}

/// 统一的 Agent 分发器
pub struct AgentDispatcher {
    task_manager: Arc<TaskManager>,
    agent_manager: Arc<AgentManager>,
    notification_bus: Arc<NotificationBus>,
    settings: crate::config::Settings,
    provider_factory: crate::provider::ProviderFactory,
    fallback_model: String,
    store: Arc<dyn crate::session::SessionStore>,
    cwd: PathBuf,
    hooks: crate::hooks::HookManager,
    interaction: Option<Arc<dyn crate::interaction::InteractionHandler>>,
    tool_approvals: Arc<tokio::sync::RwLock<std::collections::HashSet<String>>>,
}

impl AgentDispatcher {
    pub fn new(
        task_manager: Arc<TaskManager>,
        agent_manager: Arc<AgentManager>,
        notification_bus: Arc<NotificationBus>,
        settings: crate::config::Settings,
        provider_factory: crate::provider::ProviderFactory,
        fallback_model: String,
        store: Arc<dyn crate::session::SessionStore>,
        cwd: PathBuf,
        hooks: crate::hooks::HookManager,
        interaction: Option<Arc<dyn crate::interaction::InteractionHandler>>,
        tool_approvals: Arc<tokio::sync::RwLock<std::collections::HashSet<String>>>,
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

    /// 分发 agent 请求
    pub async fn dispatch(&self, request: DispatchRequest) -> ZeroBotResult<DispatchResult> {
        // 1. 加载 AgentDefinition
        let def = self.agent_manager.load(&request.agent_type)?;

        // 2. 解析模型
        let model = request
            .model_override
            .or(def.model.clone())
            .unwrap_or_else(|| self.fallback_model.clone());

        // 3. 构建 provider
        let provider = (self.provider_factory)()?;

        // 4. 构建工具注册表
        let mut tools = ToolRegistry::with_builtin(self.cwd.clone(), self.settings.tools.enabled);
        let tools = tools;

        // 5. 根据模式分发
        match request.mode {
            DispatchMode::Sync => {
                let mut agent = Agent::new(
                    provider,
                    model,
                    self.settings.clone(),
                    self.store.clone(),
                    tools,
                    request.cwd.clone().unwrap_or_else(|| self.cwd.clone()),
                    self.hooks.clone(),
                    self.interaction.clone(),
                    None, // plugins
                    self.tool_approvals.clone(),
                    None, // task_id
                    None, // parent_task_id
                    None, // agent_type
                    request.max_turns,
                    None, // notification_tx
                );

                let session = crate::session::create_session_with_hooks(
                    &*self.store,
                    Some(&def.name),
                    None,
                    crate::session::SessionKind::Sub,
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
                    request.cwd.clone().unwrap_or_else(|| self.cwd.clone()),
                    self.hooks.clone(),
                    self.interaction.clone(),
                    None,
                    self.tool_approvals.clone(),
                    None,
                    None,
                    Some(request.agent_type.clone()),
                    request.max_turns,
                    None,
                );

                let session = crate::session::create_session_with_hooks(
                    &*self.store,
                    Some(&def.name),
                    None,
                    crate::session::SessionKind::Sub,
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
                // TODO: Phase 2 后续实现 fork 模式
                Err(ZeroBotError::Agent("Fork 模式尚未实现".to_string()))
            }
            DispatchMode::Teammate { .. } => {
                // TODO: Phase 6 Swarm 实现
                Err(ZeroBotError::Agent("Teammate 模式尚未实现".to_string()))
            }
        }
    }

    /// 向后台 agent 发送消息
    pub async fn send_message(&self, _task_id: &TaskId, _message: String) -> ZeroBotResult<()> {
        // TODO: 实现 SendMessage 语义
        Err(ZeroBotError::Agent("SendMessage 尚未实现".to_string()))
    }

    /// 终止 agent
    pub async fn terminate(&self, task_id: &TaskId) -> ZeroBotResult<()> {
        self.task_manager.cancel(task_id).await;
        Ok(())
    }
}
```

- [ ] **Step 2: 在 lib.rs 注册模块**

在 `crates/zerobot-core/src/lib.rs` 的模块声明区添加：

```rust
pub mod agent_dispatch;
```

在 re-exports 区添加：

```rust
pub use agent_dispatch::{AgentDispatcher, DispatchMode, DispatchRequest, DispatchResult, ToolRestrictions, IsolationMode};
```

- [ ] **Step 3: 验证编译**

Run: `cargo check -p zerobot-core`
Expected: PASS（可能有 unused import warning）

- [ ] **Step 4: Commit**

```bash
git add crates/zerobot-core/src/agent_dispatch.rs crates/zerobot-core/src/lib.rs
git commit -m "feat: 添加 AgentDispatcher 统一 agent 分发模块"
```

---

### Task 9: AgentDispatcherTool 替换 SubagentTool

**Files:**
- Modify: `crates/zerobot-core/src/tool.rs:467-600` (SubagentTool)
- Modify: `crates/zerobot-core/src/tool.rs:363-380` (with_builtin)

- [ ] **Step 1: 创建 AgentDispatcherTool**

在 `tool.rs` 中，在原 `SubagentTool` 附近添加新的 `AgentDispatcherTool`：

```rust
/// 统一的 Agent 分发工具，替代 SubagentTool
pub struct AgentDispatcherTool {
    dispatcher: Arc<crate::agent_dispatch::AgentDispatcher>,
}

impl AgentDispatcherTool {
    pub fn new(dispatcher: Arc<crate::agent_dispatch::AgentDispatcher>) -> Self {
        Self { dispatcher }
    }
}

#[derive(Debug, serde::Deserialize)]
struct AgentToolArgs {
    description: String,
    prompt: String,
    #[serde(default)]
    subagent_type: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    run_in_background: Option<bool>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    team_name: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    max_turns: Option<u32>,
}

#[async_trait]
impl Tool for AgentDispatcherTool {
    fn name(&self) -> &str {
        "agent"
    }

    fn description(&self) -> &str {
        "分发任务给子 agent 执行。支持同步（等待结果）和后台（异步执行，结果通过通知回报）两种模式。"
    }

    fn parameters(&self) -> serde_json::JsonValue {
        serde_json::json!({
            "type": "object",
            "properties": {
                "description": {
                    "type": "string",
                    "description": "3-5 个词的任务描述"
                },
                "prompt": {
                    "type": "string",
                    "description": "要交给 agent 执行的详细任务描述"
                },
                "subagent_type": {
                    "type": "string",
                    "description": "Agent 定义名称，如 'plan', 'review', 'execute' 或自定义 agent 名"
                },
                "model": {
                    "type": "string",
                    "description": "模型覆盖，留空使用默认模型"
                },
                "run_in_background": {
                    "type": "boolean",
                    "description": "是否后台执行。true = 异步执行并通知结果，false = 同步阻塞等待（默认）"
                },
                "name": {
                    "type": "string",
                    "description": "后台 agent 的可寻址名称，用于 send_message"
                },
                "team_name": {
                    "type": "string",
                    "description": "团队名称，用于 swarm 模式"
                },
                "cwd": {
                    "type": "string",
                    "description": "工作目录覆盖"
                },
                "max_turns": {
                    "type": "number",
                    "description": "最大迭代轮次"
                }
            },
            "required": ["description", "prompt"]
        })
    }

    async fn run(
        &self,
        _ctx: &crate::tool::ToolContext,
        args: serde_json::Value,
    ) -> crate::error::ZeroBotResult<crate::tool::ToolOutput> {
        let args: AgentToolArgs = serde_json::from_value(args)
            .map_err(|e| crate::error::ZeroBotError::Tool(format!("参数解析失败: {}", e)))?;

        let mode = if args.run_in_background.unwrap_or(false) {
            crate::agent_dispatch::DispatchMode::Background { name: args.name }
        } else {
            crate::agent_dispatch::DispatchMode::Sync
        };

        let request = crate::agent_dispatch::DispatchRequest {
            agent_type: args.subagent_type.unwrap_or_else(|| "general-purpose".to_string()),
            prompt: args.prompt,
            mode,
            model_override: args.model,
            tool_overrides: None,
            cwd: args.cwd.map(std::path::PathBuf::from),
            max_turns: args.max_turns,
            isolation: None,
        };

        let result = self.dispatcher.dispatch(request).await?;

        match result {
            crate::agent_dispatch::DispatchResult::Sync(agent_result) => {
                match agent_result {
                    AgentResult::Done { message, .. } => Ok(crate::tool::ToolOutput::new(message)),
                    AgentResult::Failed(e) => Ok(crate::tool::ToolOutput::new(format!("Agent 执行失败: {}", e))),
                    AgentResult::Interrupted { reason } => Ok(crate::tool::ToolOutput::new(format!("Agent 被中断: {}", reason))),
                    AgentResult::BudgetExhausted { steps_used } => Ok(crate::tool::ToolOutput::new(format!("Agent 迭代预算耗尽 ({} 步)", steps_used))),
                }
            }
            crate::agent_dispatch::DispatchResult::Background(task_id) => {
                Ok(crate::tool::ToolOutput::new(format!(
                    "Agent 已作为后台任务分发: {}",
                    task_id
                )))
            }
            _ => Ok(crate::tool::ToolOutput::new("分发完成".to_string())),
        }
    }
}
```

- [ ] **Step 2: 注册 AgentDispatcherTool**

注意：`AgentDispatcherTool` 需要 `AgentDispatcher` 实例，不能在 `with_builtin()` 中静态注册。需要在 SDK/CLI 层创建 dispatcher 后动态注册。

在 `tool.rs` 的 `ToolRegistry` impl 中添加辅助方法：

```rust
    /// 注册 agent 分发工具
    pub fn with_agent_dispatcher(mut self, dispatcher: Arc<crate::agent_dispatch::AgentDispatcher>) -> Self {
        self.register(crate::tool::AgentDispatcherTool::new(dispatcher));
        self
    }
```

- [ ] **Step 3: 保留 SubagentTool 作为兼容**

保留原有的 `SubagentTool` 不删除，但标记为 deprecated。后续调用方逐步迁移到 `AgentDispatcherTool`。

- [ ] **Step 4: 验证编译**

Run: `cargo check -p zerobot-core`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/zerobot-core/src/tool.rs
git commit -m "feat: 添加 AgentDispatcherTool 替代 SubagentTool"
```

---

### Task 10: SendMessageTool

**Files:**
- Modify: `crates/zerobot-core/src/tool.rs` (在 AgentDispatcherTool 之后)

- [ ] **Step 1: 添加 SendMessageTool**

在 `tool.rs` 中添加：

```rust
/// 向后台 agent 发送后续消息
pub struct SendMessageTool {
    dispatcher: Arc<crate::agent_dispatch::AgentDispatcher>,
}

impl SendMessageTool {
    pub fn new(dispatcher: Arc<crate::agent_dispatch::AgentDispatcher>) -> Self {
        Self { dispatcher }
    }
}

#[derive(Debug, serde::Deserialize)]
struct SendMessageArgs {
    task_id: String,
    message: String,
}

#[async_trait]
impl Tool for SendMessageTool {
    fn name(&self) -> &str {
        "send_message"
    }

    fn description(&self) -> &str {
        "向已运行的后台 agent 发送后续消息，继续其工作。"
    }

    fn parameters(&self) -> serde_json::JsonValue {
        serde_json::json!({
            "type": "object",
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "后台 agent 的 TaskId"
                },
                "message": {
                    "type": "string",
                    "description": "要发送的消息内容"
                }
            },
            "required": ["task_id", "message"]
        })
    }

    async fn run(
        &self,
        _ctx: &crate::tool::ToolContext,
        args: serde_json::Value,
    ) -> crate::error::ZeroBotResult<crate::tool::ToolOutput> {
        let args: SendMessageArgs = serde_json::from_value(args)
            .map_err(|e| crate::error::ZeroBotError::Tool(format!("参数解析失败: {}", e)))?;

        let task_id = crate::task::TaskId::from_string(args.task_id);
        self.dispatcher.send_message(&task_id, args.message).await?;

        Ok(crate::tool::ToolOutput::new("消息已发送".to_string()))
    }
}
```

- [ ] **Step 2: 在 TaskId 添加 from_string 方法**

在 `task.rs` 的 `TaskId` impl 中添加：

```rust
    pub fn from_string(s: String) -> Self {
        Self(s)
    }
```

- [ ] **Step 3: 注册 SendMessageTool**

在 `ToolRegistry` impl 中添加：

```rust
    pub fn with_send_message(mut self, dispatcher: Arc<crate::agent_dispatch::AgentDispatcher>) -> Self {
        self.register(SendMessageTool::new(dispatcher));
        self
    }
```

- [ ] **Step 4: 验证编译**

Run: `cargo check -p zerobot-core`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/zerobot-core/src/tool.rs crates/zerobot-core/src/task.rs
git commit -m "feat: 添加 SendMessageTool 向后台 agent 发送消息"
```

---

## Phase 3: 工具系统重构

### Task 11: Toolset 模块

**Files:**
- Create: `crates/zerobot-core/src/toolset.rs`
- Modify: `crates/zerobot-core/src/lib.rs`

- [ ] **Step 1: 创建 toolset.rs**

创建 `/Volumes/nick-disk/projects/ai/ZeroBot/crates/zerobot-core/src/toolset.rs`：

```rust
use std::collections::{HashMap, HashSet};

use crate::error::{ZeroBotError, ZeroBotResult};

/// 工具集定义
#[derive(Debug, Clone)]
pub struct ToolsetDefinition {
    pub name: String,
    pub description: String,
    pub tools: Vec<String>,
    pub includes: Vec<String>,
}

/// 工具集注册表
pub struct ToolsetRegistry {
    toolsets: HashMap<String, ToolsetDefinition>,
}

impl ToolsetRegistry {
    pub fn new() -> Self {
        Self {
            toolsets: HashMap::new(),
        }
    }

    /// 注册工具集
    pub fn register(&mut self, def: ToolsetDefinition) {
        self.toolsets.insert(def.name.clone(), def);
    }

    /// 解析工具集，展开 includes 递归，返回去重后的工具名列表
    pub fn resolve(&self, name: &str) -> ZeroBotResult<Vec<String>> {
        let mut result = HashSet::new();
        let mut visited = HashSet::new();
        self.resolve_recursive(name, &mut result, &mut visited)?;
        Ok(result.into_iter().collect())
    }

    /// 解析多个工具集，合并去重
    pub fn resolve_many(&self, names: &[String]) -> ZeroBotResult<Vec<String>> {
        let mut result = HashSet::new();
        let mut visited = HashSet::new();
        for name in names {
            self.resolve_recursive(name, &mut result, &mut visited)?;
        }
        Ok(result.into_iter().collect())
    }

    fn resolve_recursive(
        &self,
        name: &str,
        result: &mut HashSet<String>,
        visited: &mut HashSet<String>,
    ) -> ZeroBotResult<()> {
        if visited.contains(name) {
            return Ok(()); // 防止循环引用
        }
        visited.insert(name.to_string());

        let def = self.toolsets.get(name).ok_or_else(|| {
            ZeroBotError::Config(format!("未知工具集: {}", name))
        })?;

        // 先解析 includes
        for include in &def.includes {
            self.resolve_recursive(include, result, visited)?;
        }

        // 再添加自己的工具
        for tool in &def.tools {
            result.insert(tool.clone());
        }

        Ok(())
    }

    /// 列出所有已注册的工具集
    pub fn list(&self) -> Vec<&ToolsetDefinition> {
        self.toolsets.values().collect()
    }
}

impl Default for ToolsetRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// 创建内置工具集注册表
pub fn builtin_toolsets() -> ToolsetRegistry {
    let mut registry = ToolsetRegistry::new();

    registry.register(ToolsetDefinition {
        name: "filesystem".to_string(),
        description: "文件读写操作".to_string(),
        tools: vec![
            "read".to_string(),
            "write".to_string(),
            "edit".to_string(),
            "apply_patch".to_string(),
            "patch".to_string(),
            "glob".to_string(),
            "grep".to_string(),
        ],
        includes: vec![],
    });

    registry.register(ToolsetDefinition {
        name: "shell".to_string(),
        description: "Shell 命令执行".to_string(),
        tools: vec!["bash".to_string(), "shell".to_string()],
        includes: vec![],
    });

    registry.register(ToolsetDefinition {
        name: "code".to_string(),
        description: "代码分析和修改".to_string(),
        tools: vec!["bash".to_string()],
        includes: vec!["filesystem".to_string()],
    });

    registry.register(ToolsetDefinition {
        name: "web".to_string(),
        description: "网络搜索和抓取".to_string(),
        tools: vec!["web_search".to_string(), "web_fetch".to_string()],
        includes: vec![],
    });

    registry.register(ToolsetDefinition {
        name: "task".to_string(),
        description: "任务管理".to_string(),
        tools: vec!["todo_read".to_string(), "todo_write".to_string()],
        includes: vec![],
    });

    registry.register(ToolsetDefinition {
        name: "agent".to_string(),
        description: "多智能体调度".to_string(),
        tools: vec!["agent".to_string(), "send_message".to_string()],
        includes: vec![],
    });

    registry.register(ToolsetDefinition {
        name: "kanban".to_string(),
        description: "看板任务协调".to_string(),
        tools: vec![
            "kanban_create".to_string(),
            "kanban_show".to_string(),
            "kanban_complete".to_string(),
            "kanban_block".to_string(),
            "kanban_comment".to_string(),
        ],
        includes: vec![],
    });

    registry.register(ToolsetDefinition {
        name: "swarm".to_string(),
        description: "团队协作".to_string(),
        tools: vec![
            "spawn_teammate".to_string(),
            "send_teammate_message".to_string(),
            "list_teammates".to_string(),
        ],
        includes: vec![],
    });

    registry
}
```

- [ ] **Step 2: 在 lib.rs 注册模块**

在 `crates/zerobot-core/src/lib.rs` 的模块声明区添加：

```rust
pub mod toolset;
```

在 re-exports 区添加：

```rust
pub use toolset::{ToolsetDefinition, ToolsetRegistry, builtin_toolsets};
```

- [ ] **Step 3: 验证编译**

Run: `cargo check -p zerobot-core`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add crates/zerobot-core/src/toolset.rs crates/zerobot-core/src/lib.rs
git commit -m "feat: 添加 Toolset 工具集模块和内置工具集定义"
```

---

### Task 12: ToolRegistry 增强

**Files:**
- Modify: `crates/zerobot-core/src/tool.rs:272-360` (ToolRegistry impl)

- [ ] **Step 1: 添加 toolset 过滤方法**

在 `ToolRegistry` impl 块中添加：

```rust
    /// 按工具名白名单过滤，返回新的 ToolRegistry
    pub fn with_allowlist(&self, allowed: &[String]) -> Self {
        let allowed_set: HashSet<String> = allowed.iter().cloned().collect();
        let filtered: HashMap<String, Arc<dyn Tool>> = self
            .tools
            .iter()
            .filter(|(name, _)| allowed_set.contains(*name))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        Self {
            tools: filtered,
            plugins: self.plugins.clone(),
            memory_manager: self.memory_manager.clone(),
        }
    }

    /// 按工具名黑名单过滤，返回新的 ToolRegistry
    pub fn with_blocklist(&self, blocked: &[String]) -> Self {
        let blocked_set: HashSet<String> = blocked.iter().cloned().collect();
        let filtered: HashMap<String, Arc<dyn Tool>> = self
            .tools
            .iter()
            .filter(|(name, _)| !blocked_set.contains(*name))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        Self {
            tools: filtered,
            plugins: self.plugins.clone(),
            memory_manager: self.memory_manager.clone(),
        }
    }

    /// 按 toolset 名称过滤，返回新的 ToolRegistry
    pub fn with_toolsets(
        &self,
        toolset_registry: &crate::toolset::ToolsetRegistry,
        names: &[String],
    ) -> crate::error::ZeroBotResult<Self> {
        let allowed = toolset_registry.resolve_many(names)?;
        Ok(self.with_allowlist(&allowed))
    }
```

确保在文件顶部有 `use std::collections::HashSet;` import。

- [ ] **Step 2: 验证编译**

Run: `cargo check -p zerobot-core`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add crates/zerobot-core/src/tool.rs
git commit -m "feat: ToolRegistry 增加 with_allowlist/blocklist/toolsets 过滤方法"
```

---

## Phase 4: Coordinator 模式

### Task 13: AgentDefinition 扩展

**Files:**
- Modify: `crates/zerobot-core/src/agents.rs:8-15` (AgentDefinition struct)
- Modify: `crates/zerobot-core/src/agents.rs:113-130` (AgentFrontmatter)
- Modify: `crates/zerobot-core/src/agents.rs:132-175` (parse_agent_file)

- [ ] **Step 1: 添加 AgentRole 枚举**

在 `agents.rs` 文件顶部添加：

```rust
use crate::agent_dispatch::IsolationMode;

/// Agent 角色
#[derive(Debug, Clone, Default)]
pub enum AgentRole {
    /// 普通 worker：执行具体任务
    #[default]
    Worker,
    /// Coordinator：编排多个 worker
    Coordinator,
    /// Orchestrator：可递归派发的编排器
    Orchestrator { max_depth: u32 },
}
```

- [ ] **Step 2: 扩展 AgentDefinition struct**

在 `AgentDefinition` struct（line 8）中添加新字段：

```rust
    /// Agent 角色
    #[serde(default)]
    pub role: AgentRole,
    /// 工具集限制
    pub toolsets: Option<Vec<String>>,
    /// 最大迭代轮次
    pub max_turns: Option<u32>,
    /// 是否默认后台运行
    #[serde(default)]
    pub background: bool,
    /// 是否跳过 CLAUDE.md 等上下文
    #[serde(default)]
    pub omit_context: bool,
    /// 隔离模式
    pub isolation: Option<IsolationMode>,
```

- [ ] **Step 3: 扩展 AgentFrontmatter**

在 `AgentFrontmatter` struct（line 113）中添加对应字段：

```rust
    #[serde(default)]
    pub role: Option<String>,
    pub toolsets: Option<Vec<String>>,
    pub max_turns: Option<u32>,
    #[serde(default)]
    pub background: Option<bool>,
    #[serde(default)]
    pub omit_context: Option<bool>,
    pub isolation: Option<String>,
```

- [ ] **Step 4: 更新 parse_agent_file**

在 `parse_agent_file()`（line 132）中，将 frontmatter 的新字段映射到 `AgentDefinition`：

```rust
    let role = match fm.role.as_deref() {
        Some("coordinator") => AgentRole::Coordinator,
        Some("orchestrator") => AgentRole::Orchestrator { max_depth: 3 },
        _ => AgentRole::Worker,
    };

    let isolation = match fm.isolation.as_deref() {
        Some("worktree") => Some(IsolationMode::Worktree),
        Some("remote") => Some(IsolationMode::Remote),
        _ => None,
    };
```

在构建 `AgentDefinition` 时添加新字段。

- [ ] **Step 5: 更新 builtin_agents()**

在 `builtin_agents()`（line 220）中，为每个内置 agent 添加默认值：

```rust
    role: AgentRole::Worker,
    toolsets: None,
    max_turns: None,
    background: false,
    omit_context: false,
    isolation: None,
```

- [ ] **Step 6: 验证编译**

Run: `cargo check -p zerobot-core`
Expected: PASS

- [ ] **Step 7: Commit**

```bash
git add crates/zerobot-core/src/agents.rs
git commit -m "feat: AgentDefinition 扩展 role, toolsets, max_turns 等字段"
```

---

### Task 14: Coordinator 系统提示

**Files:**
- Create: `crates/zerobot-core/prompts/modes/coordinator.md`
- Modify: `crates/zerobot-core/src/agents.rs:220-265` (builtin_agents)

- [ ] **Step 1: 创建 coordinator.md**

创建 `/Volumes/nick-disk/projects/ai/ZeroBot/crates/zerobot-core/prompts/modes/coordinator.md`：

```markdown
你是一个编排器（Coordinator）。你的职责是将复杂任务分解为子任务，分发给 worker agent，综合结果。

## 核心规则

1. **你不能自己执行文件编辑或 bash 命令**，必须委托给 worker agent
2. **每个 worker 任务必须有明确的、可验证的交付物**
3. **如果 worker 失败，分析原因并重新分发**
4. **保持对全局进度的掌控**，使用 todo_write 跟踪子任务状态

## 工作流程

### 阶段 1: 研究（并行）
- 将独立的探索/研究任务分发给多个 worker（后台并行）
- 每个 worker 专注于一个子问题
- 使用 `agent` 工具的 `run_in_background: true` 并行派发
- 等待所有研究 worker 完成（通过 task-notification 收到结果）

### 阶段 2: 综合
- 综合分析所有 worker 的发现
- 形成统一的理解和方案
- 识别需要实现的具体模块

### 阶段 3: 实现
- 将实现任务分发给 worker
- 每个 worker 负责一个独立的实现模块
- 确保 worker 之间没有冲突的文件修改

### 阶段 4: 验证
- 分发验证任务给 worker
- 检查实现是否符合预期
- 汇总验证结果，报告最终状态

## 工具使用

- `agent`: 分发任务给 worker（支持 sync 和 background 模式）
- `send_message`: 给已运行的 worker 发送后续指令
- `todo_read`/`todo_write`: 跟踪整体进度
- `read`: 只读查看文件（用于综合分析）
```

- [ ] **Step 2: 注册 coordinator 内置 agent**

在 `agents.rs` 的 `builtin_agents()` 中添加 coordinator：

```rust
    let coordinator = builtin_agent(
        "coordinator",
        "编排多个 worker agent 完成复杂任务",
        include_str!("../prompts/modes/coordinator.md"),
        Some(vec!["agent".to_string(), "send_message".to_string(), "todo_read".to_string(), "todo_write".to_string(), "read".to_string()]),
    );
    // 设置 role 为 Coordinator
    let mut coordinator = coordinator;
    coordinator.role = AgentRole::Coordinator;

    agents.push(coordinator);
```

- [ ] **Step 3: 验证编译**

Run: `cargo check -p zerobot-core`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add crates/zerobot-core/prompts/modes/coordinator.md crates/zerobot-core/src/agents.rs
git commit -m "feat: 添加 Coordinator 内置 agent 和 4 阶段编排系统提示"
```

---

### Task 15: Coordinator 递归深度限制

**Files:**
- Modify: `crates/zerobot-core/src/config.rs:364-375` (AgentSettings)
- Modify: `crates/zerobot-core/src/agent_dispatch.rs` (dispatch 方法)

- [ ] **Step 1: 在 AgentSettings 添加编排深度配置**

在 `config.rs` 的 `AgentSettings` struct（line 364）中添加：

```rust
    /// 编排器最大递归深度
    #[serde(default = "default_max_orchestration_depth")]
    pub max_orchestration_depth: u32,
```

添加默认值函数：

```rust
fn default_max_orchestration_depth() -> u32 {
    3
}
```

- [ ] **Step 2: 在 dispatch 中检查深度**

在 `agent_dispatch.rs` 的 `DispatchRequest` 中添加可选的 `depth` 字段：

```rust
    pub depth: Option<u32>,
```

在 `dispatch()` 方法中，当 agent 的 role 是 Orchestrator 时检查深度：

```rust
    // 在 dispatch() 方法开始处
    if let Some(depth) = request.depth {
        if depth >= self.settings.agent.max_orchestration_depth {
            return Err(ZeroBotError::OrchestrationDepthExceeded(
                format!("编排深度 {} 超过限制 {}", depth, self.settings.agent.max_orchestration_depth)
            ));
        }
    }
```

- [ ] **Step 3: 验证编译**

Run: `cargo check -p zerobot-core`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add crates/zerobot-core/src/config.rs crates/zerobot-core/src/agent_dispatch.rs
git commit -m "feat: 添加编排器递归深度限制配置和检查"
```

---

## Phase 5: Kanban 模块

### Task 16: KanbanManager 和数据模型

**Files:**
- Create: `crates/zerobot-core/src/kanban.rs`
- Modify: `crates/zerobot-core/src/lib.rs`
- Modify: `crates/zerobot-core/src/session.rs:200-250` (SqliteSessionStore::init)

- [ ] **Step 1: 创建 kanban.rs**

创建 `/Volumes/nick-disk/projects/ai/ZeroBot/crates/zerobot-core/src/kanban.rs`：

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::error::{ZeroBotError, ZeroBotResult};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum KanbanStatus {
    Todo,
    InProgress,
    Blocked { reason: String },
    Completed,
    Cancelled,
}

impl KanbanStatus {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Todo => "todo",
            Self::InProgress => "in_progress",
            Self::Blocked { .. } => "blocked",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KanbanTask {
    pub id: String,
    pub title: String,
    pub description: String,
    pub status: KanbanStatus,
    pub assignee: Option<String>,
    pub parent_id: Option<String>,
    pub metadata: JsonValue,
    pub summary: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewKanbanTask {
    pub title: String,
    pub description: String,
    pub assignee: Option<String>,
    pub parent_id: Option<String>,
    pub metadata: Option<JsonValue>,
}

#[derive(Debug, Clone, Default)]
pub struct KanbanFilter {
    pub status: Option<String>,
    pub assignee: Option<String>,
    pub parent_id: Option<String>,
}

/// Kanban 任务管理器
pub struct KanbanManager {
    db: sqlx::SqlitePool,
}

impl KanbanManager {
    pub fn new(db: sqlx::SqlitePool) -> Self {
        Self { db }
    }

    /// 初始化数据库表
    pub async fn init(&self) -> ZeroBotResult<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS kanban_tasks (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                description TEXT NOT NULL DEFAULT '',
                status TEXT NOT NULL DEFAULT 'todo',
                blocked_reason TEXT,
                assignee TEXT,
                parent_id TEXT,
                metadata TEXT NOT NULL DEFAULT '{}',
                summary TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS kanban_comments (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                task_id TEXT NOT NULL,
                comment TEXT NOT NULL,
                created_at TEXT NOT NULL,
                FOREIGN KEY (task_id) REFERENCES kanban_tasks(id)
            );
            "#,
        )
        .execute(&self.db)
        .await
        .map_err(|e| ZeroBotError::Kanban(format!("初始化数据库失败: {}", e)))?;

        Ok(())
    }

    /// 创建任务
    pub async fn create(&self, task: NewKanbanTask) -> ZeroBotResult<KanbanTask> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now();
        let metadata = task.metadata.unwrap_or_else(|| serde_json::json!({}));

        sqlx::query(
            "INSERT INTO kanban_tasks (id, title, description, status, assignee, parent_id, metadata, created_at, updated_at) VALUES (?, ?, ?, 'todo', ?, ?, ?, ?, ?)"
        )
        .bind(&id)
        .bind(&task.title)
        .bind(&task.description)
        .bind(&task.assignee)
        .bind(&task.parent_id)
        .bind(metadata.to_string())
        .bind(now.to_rfc3339())
        .bind(now.to_rfc3339())
        .execute(&self.db)
        .await
        .map_err(|e| ZeroBotError::Kanban(format!("创建任务失败: {}", e)))?;

        Ok(KanbanTask {
            id,
            title: task.title,
            description: task.description,
            status: KanbanStatus::Todo,
            assignee: task.assignee,
            parent_id: task.parent_id,
            metadata,
            summary: None,
            created_at: now,
            updated_at: now,
        })
    }

    /// 获取任务
    pub async fn get(&self, id: &str) -> ZeroBotResult<Option<KanbanTask>> {
        let row = sqlx::query_as::<_, (String, String, String, String, Option<String>, Option<String>, Option<String>, Option<String>, String, String)>(
            "SELECT id, title, description, status, assignee, parent_id, metadata, summary, created_at, updated_at FROM kanban_tasks WHERE id = ?"
        )
        .bind(id)
        .fetch_optional(&self.db)
        .await
        .map_err(|e| ZeroBotError::Kanban(format!("查询任务失败: {}", e)))?;

        Ok(row.map(|r| self.row_to_task(r)))
    }

    /// 列出任务
    pub async fn list(&self, filter: KanbanFilter) -> ZeroBotResult<Vec<KanbanTask>> {
        let mut sql = String::from(
            "SELECT id, title, description, status, assignee, parent_id, metadata, summary, created_at, updated_at FROM kanban_tasks WHERE 1=1"
        );
        let mut binds: Vec<String> = Vec::new();

        if let Some(status) = &filter.status {
            sql.push_str(" AND status = ?");
            binds.push(status.clone());
        }
        if let Some(assignee) = &filter.assignee {
            sql.push_str(" AND assignee = ?");
            binds.push(assignee.clone());
        }
        if let Some(parent_id) = &filter.parent_id {
            sql.push_str(" AND parent_id = ?");
            binds.push(parent_id.clone());
        }

        sql.push_str(" ORDER BY created_at DESC");

        let mut query = sqlx::query_as::<_, (String, String, String, String, Option<String>, Option<String>, Option<String>, Option<String>, String, String)>(&sql);
        for bind in &binds {
            query = query.bind(bind);
        }

        let rows = query
            .fetch_all(&self.db)
            .await
            .map_err(|e| ZeroBotError::Kanban(format!("列出任务失败: {}", e)))?;

        Ok(rows.into_iter().map(|r| self.row_to_task(r)).collect())
    }

    /// 更新任务状态
    pub async fn update_status(&self, id: &str, status: KanbanStatus) -> ZeroBotResult<()> {
        let (status_str, blocked_reason) = match &status {
            KanbanStatus::Blocked { reason } => ("blocked", Some(reason.clone())),
            _ => (status.as_str(), None),
        };

        sqlx::query("UPDATE kanban_tasks SET status = ?, blocked_reason = ?, updated_at = ? WHERE id = ?")
            .bind(status_str)
            .bind(blocked_reason)
            .bind(Utc::now().to_rfc3339())
            .bind(id)
            .execute(&self.db)
            .await
            .map_err(|e| ZeroBotError::Kanban(format!("更新状态失败: {}", e)))?;

        Ok(())
    }

    /// 分配任务
    pub async fn assign(&self, id: &str, agent: &str) -> ZeroBotResult<()> {
        sqlx::query("UPDATE kanban_tasks SET assignee = ?, updated_at = ? WHERE id = ?")
            .bind(agent)
            .bind(Utc::now().to_rfc3339())
            .bind(id)
            .execute(&self.db)
            .await
            .map_err(|e| ZeroBotError::Kanban(format!("分配任务失败: {}", e)))?;

        Ok(())
    }

    /// 添加评论
    pub async fn comment(&self, id: &str, comment: &str) -> ZeroBotResult<()> {
        sqlx::query("INSERT INTO kanban_comments (task_id, comment, created_at) VALUES (?, ?, ?)")
            .bind(id)
            .bind(comment)
            .bind(Utc::now().to_rfc3339())
            .execute(&self.db)
            .await
            .map_err(|e| ZeroBotError::Kanban(format!("添加评论失败: {}", e)))?;

        Ok(())
    }

    /// 完成任务
    pub async fn complete(&self, id: &str, summary: &str, metadata: JsonValue) -> ZeroBotResult<()> {
        sqlx::query("UPDATE kanban_tasks SET status = 'completed', summary = ?, metadata = ?, updated_at = ? WHERE id = ?")
            .bind(summary)
            .bind(metadata.to_string())
            .bind(Utc::now().to_rfc3339())
            .bind(id)
            .execute(&self.db)
            .await
            .map_err(|e| ZeroBotError::Kanban(format!("完成任务失败: {}", e)))?;

        Ok(())
    }

    fn row_to_task(&self, row: (String, String, String, String, Option<String>, Option<String>, Option<String>, Option<String>, String, String)) -> KanbanTask {
        let (id, title, description, status, assignee, parent_id, metadata, summary, created_at, updated_at) = row;

        let status = match status.as_str() {
            "todo" => KanbanStatus::Todo,
            "in_progress" => KanbanStatus::InProgress,
            "blocked" => KanbanStatus::Blocked {
                reason: String::new(), // blocked_reason 需要额外查询
            },
            "completed" => KanbanStatus::Completed,
            "cancelled" => KanbanStatus::Cancelled,
            _ => KanbanStatus::Todo,
        };

        let metadata: JsonValue = metadata
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| serde_json::json!({}));

        KanbanTask {
            id,
            title,
            description,
            status,
            assignee,
            parent_id,
            metadata,
            summary,
            created_at: DateTime::parse_from_rfc3339(&created_at)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now()),
            updated_at: DateTime::parse_from_rfc3339(&updated_at)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now()),
        }
    }
}
```

- [ ] **Step 2: 在 lib.rs 注册模块**

在 `crates/zerobot-core/src/lib.rs` 添加：

```rust
pub mod kanban;
```

```rust
pub use kanban::{KanbanManager, KanbanTask, KanbanStatus, NewKanbanTask, KanbanFilter};
```

- [ ] **Step 3: 验证编译**

Run: `cargo check -p zerobot-core`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add crates/zerobot-core/src/kanban.rs crates/zerobot-core/src/lib.rs
git commit -m "feat: 添加 KanbanManager 看板任务管理模块"
```

---

### Task 17: Kanban 工具集

**Files:**
- Modify: `crates/zerobot-core/src/kanban.rs` (在末尾添加工具实现)

- [ ] **Step 1: 添加 Kanban 工具**

在 `kanban.rs` 末尾添加 5 个工具 struct 和 `Tool` trait 实现：

```rust
// ============ Kanban Tools ============

use async_trait::async_trait;
use crate::tool::{Tool, ToolContext, ToolOutput};

use crate::error::{ZeroBotError, ZeroBotResult};

/// 创建看板任务
pub struct KanbanCreateTool {
    manager: std::sync::Arc<KanbanManager>,
}

impl KanbanCreateTool {
    pub fn new(manager: std::sync::Arc<KanbanManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for KanbanCreateTool {
    fn name(&self) -> &str { "kanban_create" }
    fn description(&self) -> &str { "创建新的看板任务" }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "title": { "type": "string", "description": "任务标题" },
                "description": { "type": "string", "description": "任务详细描述" },
                "assignee": { "type": "string", "description": "分配给的 agent 名称" },
                "parent_id": { "type": "string", "description": "父任务 ID（任务依赖）" }
            },
            "required": ["title"]
        })
    }
    async fn run(&self, _ctx: &ToolContext, args: serde_json::Value) -> ZeroBotResult<ToolOutput> {
        let title = args["title"].as_str().ok_or_else(|| ZeroBotError::Tool("缺少 title".into()))?;
        let description = args["description"].as_str().unwrap_or("").to_string();
        let assignee = args["assignee"].as_str().map(|s| s.to_string());
        let parent_id = args["parent_id"].as_str().map(|s| s.to_string());

        let task = self.manager.create(NewKanbanTask {
            title: title.to_string(),
            description,
            assignee,
            parent_id,
            metadata: None,
        }).await?;

        Ok(ToolOutput::new(serde_json::to_string_pretty(&task).unwrap_or_default()))
    }
}

/// 查看看板任务
pub struct KanbanShowTool {
    manager: std::sync::Arc<KanbanManager>,
}

impl KanbanShowTool {
    pub fn new(manager: std::sync::Arc<KanbanManager>) -> Self { Self { manager } }
}

#[async_trait]
impl Tool for KanbanShowTool {
    fn name(&self) -> &str { "kanban_show" }
    fn description(&self) -> &str { "查看看板任务列表或单个任务详情" }
    fn is_read_only(&self) -> bool { true }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "任务 ID（查看单个）" },
                "status": { "type": "string", "description": "按状态过滤" },
                "assignee": { "type": "string", "description": "按分配者过滤" }
            }
        })
    }
    async fn run(&self, _ctx: &ToolContext, args: serde_json::Value) -> ZeroBotResult<ToolOutput> {
        if let Some(id) = args["id"].as_str() {
            let task = self.manager.get(id).await?;
            return Ok(ToolOutput::new(serde_json::to_string_pretty(&task).unwrap_or_default()));
        }

        let filter = KanbanFilter {
            status: args["status"].as_str().map(|s| s.to_string()),
            assignee: args["assignee"].as_str().map(|s| s.to_string()),
            parent_id: None,
        };
        let tasks = self.manager.list(filter).await?;
        Ok(ToolOutput::new(serde_json::to_string_pretty(&tasks).unwrap_or_default()))
    }
}

/// 完成看板任务
pub struct KanbanCompleteTool {
    manager: std::sync::Arc<KanbanManager>,
}

impl KanbanCompleteTool {
    pub fn new(manager: std::sync::Arc<KanbanManager>) -> Self { Self { manager } }
}

#[async_trait]
impl Tool for KanbanCompleteTool {
    fn name(&self) -> &str { "kanban_complete" }
    fn description(&self) -> &str { "标记看板任务为已完成" }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "任务 ID" },
                "summary": { "type": "string", "description": "完成摘要" },
                "metadata": { "type": "object", "description": "结构化元数据" }
            },
            "required": ["id", "summary"]
        })
    }
    async fn run(&self, _ctx: &ToolContext, args: serde_json::Value) -> ZeroBotResult<ToolOutput> {
        let id = args["id"].as_str().ok_or_else(|| ZeroBotError::Tool("缺少 id".into()))?;
        let summary = args["summary"].as_str().ok_or_else(|| ZeroBotError::Tool("缺少 summary".into()))?;
        let metadata = args.get("metadata").cloned().unwrap_or_else(|| serde_json::json!({}));

        self.manager.complete(id, summary, metadata).await?;
        Ok(ToolOutput::new(format!("任务 {} 已完成", id)))
    }
}

/// 阻塞看板任务
pub struct KanbanBlockTool {
    manager: std::sync::Arc<KanbanManager>,
}

impl KanbanBlockTool {
    pub fn new(manager: std::sync::Arc<KanbanManager>) -> Self { Self { manager } }
}

#[async_trait]
impl Tool for KanbanBlockTool {
    fn name(&self) -> &str { "kanban_block" }
    fn description(&self) -> &str { "标记看板任务为阻塞状态" }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "任务 ID" },
                "reason": { "type": "string", "description": "阻塞原因" }
            },
            "required": ["id", "reason"]
        })
    }
    async fn run(&self, _ctx: &ToolContext, args: serde_json::Value) -> ZeroBotResult<ToolOutput> {
        let id = args["id"].as_str().ok_or_else(|| ZeroBotError::Tool("缺少 id".into()))?;
        let reason = args["reason"].as_str().ok_or_else(|| ZeroBotError::Tool("缺少 reason".into()))?;

        self.manager.update_status(id, KanbanStatus::Blocked { reason: reason.to_string() }).await?;
        Ok(ToolOutput::new(format!("任务 {} 已阻塞: {}", id, reason)))
    }
}

/// 看板任务评论
pub struct KanbanCommentTool {
    manager: std::sync::Arc<KanbanManager>,
}

impl KanbanCommentTool {
    pub fn new(manager: std::sync::Arc<KanbanManager>) -> Self { Self { manager } }
}

#[async_trait]
impl Tool for KanbanCommentTool {
    fn name(&self) -> &str { "kanban_comment" }
    fn description(&self) -> &str { "为看板任务添加评论" }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "任务 ID" },
                "comment": { "type": "string", "description": "评论内容" }
            },
            "required": ["id", "comment"]
        })
    }
    async fn run(&self, _ctx: &ToolContext, args: serde_json::Value) -> ZeroBotResult<ToolOutput> {
        let id = args["id"].as_str().ok_or_else(|| ZeroBotError::Tool("缺少 id".into()))?;
        let comment = args["comment"].as_str().ok_or_else(|| ZeroBotError::Tool("缺少 comment".into()))?;

        self.manager.comment(id, comment).await?;
        Ok(ToolOutput::new(format!("已为任务 {} 添加评论", id)))
    }
}
```

- [ ] **Step 2: 验证编译**

Run: `cargo check -p zerobot-core`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add crates/zerobot-core/src/kanban.rs
git commit -m "feat: 添加 Kanban 工具集 (create, show, complete, block, comment)"
```

---

### Task 18: Settings 添加 Kanban 和 Swarm 配置

**Files:**
- Modify: `crates/zerobot-core/src/config.rs:11-48` (Settings struct)
- Modify: `crates/zerobot-core/src/config.rs` (添加新 Settings struct)

- [ ] **Step 1: 添加 KanbanSettings 和 SwarmSettings**

在 `config.rs` 中添加：

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KanbanSettings {
    /// 是否启用 Kanban 模式
    #[serde(default)]
    pub enabled: bool,
    /// 调度 tick 间隔（秒）
    #[serde(default = "default_kanban_tick_interval")]
    pub tick_interval_secs: u64,
}

fn default_kanban_tick_interval() -> u64 { 60 }

impl Default for KanbanSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            tick_interval_secs: 60,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwarmSettings {
    /// 是否启用 Swarm 模式
    #[serde(default)]
    pub enabled: bool,
    /// 默认后端类型: "in_process", "tmux", "external"
    #[serde(default = "default_swarm_backend")]
    pub default_backend: String,
    /// 邮箱目录
    #[serde(default = "default_mailbox_dir")]
    pub mailbox_dir: String,
}

fn default_swarm_backend() -> String { "in_process".to_string() }
fn default_mailbox_dir() -> String { "~/.zerobot/mailbox".to_string() }

impl Default for SwarmSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            default_backend: "in_process".to_string(),
            mailbox_dir: "~/.zerobot/mailbox".to_string(),
        }
    }
}
```

- [ ] **Step 2: 在 Settings struct 添加新字段**

在 `Settings` struct（line 11）中添加：

```rust
    pub kanban: KanbanSettings,
    pub swarm: SwarmSettings,
```

- [ ] **Step 3: 验证编译**

Run: `cargo check -p zerobot-core`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add crates/zerobot-core/src/config.rs
git commit -m "feat: Settings 添加 Kanban 和 Swarm 配置节"
```

---

## Phase 6: Swarm 模块

### Task 19: Swarm 后端抽象和 InProcessBackend

**Files:**
- Create: `crates/zerobot-core/src/swarm/mod.rs`
- Create: `crates/zerobot-core/src/swarm/in_process.rs`
- Create: `crates/zerobot-core/src/swarm/mailbox.rs`
- Modify: `crates/zerobot-core/src/lib.rs`

- [ ] **Step 1: 创建 swarm/mod.rs**

创建 `/Volumes/nick-disk/projects/ai/ZeroBot/crates/zerobot-core/src/swarm/mod.rs`：

```rust
pub mod in_process;
pub mod mailbox;

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
```

- [ ] **Step 2: 创建 swarm/mailbox.rs**

创建 `/Volumes/nick-disk/projects/ai/ZeroBot/crates/zerobot-core/src/swarm/mailbox.rs`：

```rust
use std::path::PathBuf;
use crate::error::{ZeroBotError, ZeroBotResult};

/// 文件系统邮箱 IPC
pub struct Mailbox {
    dir: PathBuf,
}

impl Mailbox {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn mailbox_path(&self, agent_name: &str, team_name: &str) -> PathBuf {
        self.dir.join(format!("{}_{}.jsonl", team_name, agent_name))
    }

    /// 发送消息到 teammate 的邮箱
    pub fn send(&self, agent_name: &str, team_name: &str, message: &str) -> ZeroBotResult<()> {
        std::fs::create_dir_all(&self.dir)
            .map_err(|e| ZeroBotError::Swarm(format!("创建邮箱目录失败: {}", e)))?;

        let path = self.mailbox_path(agent_name, team_name);
        let entry = serde_json::json!({
            "message": message,
            "timestamp": chrono::Utc::now().to_rfc3339(),
        });

        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| ZeroBotError::Swarm(format!("打开邮箱文件失败: {}", e)))?;

        writeln!(file, "{}", entry)
            .map_err(|e| ZeroBotError::Swarm(format!("写入邮箱失败: {}", e)))?;

        Ok(())
    }

    /// 读取并清空自己的邮箱
    pub fn drain(&self, agent_name: &str, team_name: &str) -> ZeroBotResult<Vec<String>> {
        let path = self.mailbox_path(agent_name, team_name);
        if !path.exists() {
            return Ok(Vec::new());
        }

        let content = std::fs::read_to_string(&path)
            .map_err(|e| ZeroBotError::Swarm(format!("读取邮箱失败: {}", e)))?;

        let messages: Vec<String> = content
            .lines()
            .filter_map(|line| {
                serde_json::from_str::<serde_json::Value>(line)
                    .ok()
                    .and_then(|v| v["message"].as_str().map(|s| s.to_string()))
            })
            .collect();

        // 清空邮箱
        std::fs::remove_file(&path)
            .map_err(|e| ZeroBotError::Swarm(format!("清空邮箱失败: {}", e)))?;

        Ok(messages)
    }
}
```

- [ ] **Step 3: 创建 swarm/in_process.rs**

创建 `/Volumes/nick-disk/projects/ai/ZeroBot/crates/zerobot-core/src/swarm/in_process.rs`：

```rust
use async_trait::async_trait;
use std::path::PathBuf;
use std::sync::Arc;

use super::{BackendType, TeammateBackend, TeammateConfig, TeammateHandle};
use super::mailbox::Mailbox;
use crate::agent_dispatch::AgentDispatcher;
use crate::error::{ZeroBotError, ZeroBotResult};
use crate::task::{TaskId, TaskManager};

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
```

- [ ] **Step 4: 在 lib.rs 注册模块**

在 `crates/zerobot-core/src/lib.rs` 添加：

```rust
pub mod swarm;
```

```rust
pub use swarm::{SwarmManager, TeammateBackend, TeammateConfig, TeammateHandle, BackendType};
```

- [ ] **Step 5: 验证编译**

Run: `cargo check -p zerobot-core`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/zerobot-core/src/swarm/
git commit -m "feat: 添加 Swarm 模块 (TeammateBackend, InProcessBackend, Mailbox IPC)"
```

---

### Task 20: Swarm 工具集

**Files:**
- Create: `crates/zerobot-core/src/swarm/tools.rs`
- Modify: `crates/zerobot-core/src/swarm/mod.rs`

- [ ] **Step 1: 创建 swarm/tools.rs**

创建 `/Volumes/nick-disk/projects/ai/ZeroBot/crates/zerobot-core/src/swarm/tools.rs`：

```rust
use std::sync::Arc;
use async_trait::async_trait;
use crate::error::{ZeroBotError, ZeroBotResult};
use crate::tool::{Tool, ToolContext, ToolOutput};
use super::{SwarmManager, TeammateConfig};

/// 生成 teammate
pub struct SpawnTeammateTool {
    manager: Arc<SwarmManager>,
}

impl SpawnTeammateTool {
    pub fn new(manager: Arc<SwarmManager>) -> Self { Self { manager } }
}

#[async_trait]
impl Tool for SpawnTeammateTool {
    fn name(&self) -> &str { "spawn_teammate" }
    fn description(&self) -> &str { "生成一个新的 teammate agent" }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "agent_name": { "type": "string", "description": "teammate 名称" },
                "team_name": { "type": "string", "description": "团队名称" },
                "agent_type": { "type": "string", "description": "agent 定义名称" },
                "prompt": { "type": "string", "description": "初始任务描述" }
            },
            "required": ["agent_name", "team_name", "agent_type", "prompt"]
        })
    }
    async fn run(&self, _ctx: &ToolContext, args: serde_json::Value) -> ZeroBotResult<ToolOutput> {
        let config = TeammateConfig {
            agent_name: args["agent_name"].as_str().ok_or_else(|| ZeroBotError::Tool("缺少 agent_name".into()))?.to_string(),
            team_name: args["team_name"].as_str().ok_or_else(|| ZeroBotError::Tool("缺少 team_name".into()))?.to_string(),
            agent_type: args["agent_type"].as_str().ok_or_else(|| ZeroBotError::Tool("缺少 agent_type".into()))?.to_string(),
            prompt: args["prompt"].as_str().ok_or_else(|| ZeroBotError::Tool("缺少 prompt".into()))?.to_string(),
            model: args["model"].as_str().map(|s| s.to_string()),
            cwd: args["cwd"].as_str().map(std::path::PathBuf::from),
        };
        let handle = self.manager.spawn_teammate(config).await?;
        Ok(ToolOutput::new(format!("Teammate {}@{} 已生成 (task: {})", handle.agent_name, handle.team_name, handle.task_id)))
    }
}

/// 向 teammate 发送消息
pub struct SendTeammateMessageTool {
    manager: Arc<SwarmManager>,
}

impl SendTeammateMessageTool {
    pub fn new(manager: Arc<SwarmManager>) -> Self { Self { manager } }
}

#[async_trait]
impl Tool for SendTeammateMessageTool {
    fn name(&self) -> &str { "send_teammate_message" }
    fn description(&self) -> &str { "向 teammate 发送消息" }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "agent_name": { "type": "string", "description": "teammate 名称" },
                "team_name": { "type": "string", "description": "团队名称" },
                "message": { "type": "string", "description": "消息内容" }
            },
            "required": ["agent_name", "team_name", "message"]
        })
    }
    async fn run(&self, _ctx: &ToolContext, args: serde_json::Value) -> ZeroBotResult<ToolOutput> {
        let agent_name = args["agent_name"].as_str().ok_or_else(|| ZeroBotError::Tool("缺少 agent_name".into()))?;
        let team_name = args["team_name"].as_str().ok_or_else(|| ZeroBotError::Tool("缺少 team_name".into()))?;
        let message = args["message"].as_str().ok_or_else(|| ZeroBotError::Tool("缺少 message".into()))?;

        // 查找 handle
        let active = self.manager.list_active().await;
        let handle = active.iter().find(|h| h.agent_name == agent_name && h.team_name == team_name)
            .ok_or_else(|| ZeroBotError::Swarm(format!("Teammate {}@{} 未找到", agent_name, team_name)))?;

        self.manager.send_message(handle, message.to_string()).await?;
        Ok(ToolOutput::new("消息已发送".to_string()))
    }
}

/// 列出活跃的 teammate
pub struct ListTeammatesTool {
    manager: Arc<SwarmManager>,
}

impl ListTeammatesTool {
    pub fn new(manager: Arc<SwarmManager>) -> Self { Self { manager } }
}

#[async_trait]
impl Tool for ListTeammatesTool {
    fn name(&self) -> &str { "list_teammates" }
    fn description(&self) -> &str { "列出所有活跃的 teammate" }
    fn is_read_only(&self) -> bool { true }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {} })
    }
    async fn run(&self, _ctx: &ToolContext, _args: serde_json::Value) -> ZeroBotResult<ToolOutput> {
        let teammates = self.manager.list_active().await;
        if teammates.is_empty() {
            return Ok(ToolOutput::new("没有活跃的 teammate".to_string()));
        }
        let list: Vec<String> = teammates.iter()
            .map(|h| format!("- {}@{} (task: {}, backend: {:?})", h.agent_name, h.team_name, h.task_id, h.backend_type))
            .collect();
        Ok(ToolOutput::new(format!("活跃的 teammate:\n{}", list.join("\n"))))
    }
}
```

- [ ] **Step 2: 在 swarm/mod.rs 注册工具模块**

在 `swarm/mod.rs` 顶部添加：

```rust
pub mod tools;
```

- [ ] **Step 3: 验证编译**

Run: `cargo check -p zerobot-core`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add crates/zerobot-core/src/swarm/tools.rs crates/zerobot-core/src/swarm/mod.rs
git commit -m "feat: 添加 Swarm 工具集 (spawn_teammate, send_message, list)"
```

---

### Task 21: Gateway Kanban 调度集成

**Files:**
- Modify: `crates/zerobot-core/src/gateway.rs:87-94` (GatewayRuntime struct)
- Modify: `crates/zerobot-core/src/gateway.rs:267-330` (run loop)

- [ ] **Step 1: 在 GatewayRuntime 添加 KanbanManager**

在 `GatewayRuntime` struct（line 87）中添加：

```rust
    kanban_manager: Option<Arc<kanban::KanbanManager>>,
    agent_dispatcher: Option<Arc<agent_dispatch::AgentDispatcher>>,
```

- [ ] **Step 2: 在 run() 循环中添加 kanban tick**

在 `GatewayRuntime::run()` 的主循环中（line 269），在 inbound message 处理之前添加 kanban 调度逻辑：

```rust
        // Kanban 调度 tick
        if let (Some(kanban), Some(dispatcher)) = (&self.kanban_manager, &self.agent_dispatcher) {
            let todo_tasks = kanban.list(kanban::KanbanFilter {
                status: Some("todo".to_string()),
                assignee: None,
                parent_id: None,
            }).await.unwrap_or_default();

            for task in todo_tasks {
                if let Some(assignee) = &task.assignee {
                    let request = agent_dispatch::DispatchRequest {
                        agent_type: assignee.clone(),
                        prompt: format!("执行看板任务: {}\n\n{}", task.title, task.description),
                        mode: agent_dispatch::DispatchMode::Background { name: Some(assignee.clone()) },
                        model_override: None,
                        tool_overrides: None,
                        cwd: None,
                        max_turns: None,
                        isolation: None,
                        depth: None,
                    };

                    if let Err(e) = dispatcher.dispatch(request).await {
                        tracing::warn!("Kanban 调度失败: {}", e);
                    } else {
                        let _ = kanban.update_status(&task.id, kanban::KanbanStatus::InProgress).await;
                    }
                }
            }
        }
```

- [ ] **Step 3: 验证编译**

Run: `cargo check -p zerobot-core`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add crates/zerobot-core/src/gateway.rs
git commit -m "feat: Gateway 集成 Kanban 自动调度 tick"
```

---

## Phase 7: 集成和测试

### Task 22: 端到端集成验证

**Files:**
- Modify: `crates/zerobot-core/src/lib.rs` (确保所有模块正确导出)

- [ ] **Step 1: 完整编译检查**

Run: `cargo check -p zerobot-core`
Expected: PASS

Run: `cargo check -p zerobot-cli`
Expected: PASS

Run: `cargo check -p zerobot-sdk`
Expected: PASS

- [ ] **Step 2: 运行现有测试**

Run: `cargo test -p zerobot-core`
Expected: 现有测试全部通过

- [ ] **Step 3: 运行 clippy**

Run: `cargo clippy -p zerobot-core -- -D warnings`
Expected: 无 warning

- [ ] **Step 4: 修复任何编译或测试问题**

如有问题，逐一修复。

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "chore: 多智能体重构集成验证，修复编译问题"
```

---

## 文件变更总结

### 新增文件（9 个）
| 文件 | 描述 |
|------|------|
| `crates/zerobot-core/src/task.rs` | Task 状态机和 TaskManager |
| `crates/zerobot-core/src/notification.rs` | NotificationBus |
| `crates/zerobot-core/src/agent_dispatch.rs` | AgentDispatcher |
| `crates/zerobot-core/src/toolset.rs` | ToolsetDefinition + ToolsetRegistry |
| `crates/zerobot-core/src/kanban.rs` | KanbanManager + 5 个工具 |
| `crates/zerobot-core/src/swarm/mod.rs` | SwarmManager + TeammateBackend |
| `crates/zerobot-core/src/swarm/in_process.rs` | InProcessBackend |
| `crates/zerobot-core/src/swarm/mailbox.rs` | 文件邮箱 IPC |
| `crates/zerobot-core/src/swarm/tools.rs` | Swarm 工具集 |
| `crates/zerobot-core/prompts/modes/coordinator.md` | Coordinator 系统提示 |

### 修改文件（8 个）
| 文件 | 变更 |
|------|------|
| `Cargo.toml` | 添加 tokio-util 依赖 |
| `crates/zerobot-core/Cargo.toml` | 添加 tokio-util 依赖 |
| `crates/zerobot-core/src/lib.rs` | 新模块声明和 re-exports |
| `crates/zerobot-core/src/error.rs` | 新错误变体 |
| `crates/zerobot-core/src/agent.rs` | Agent 新字段 + run_background |
| `crates/zerobot-core/src/agents.rs` | AgentDefinition 扩展 |
| `crates/zerobot-core/src/tool.rs` | AgentDispatcherTool + SendMessageTool |
| `crates/zerobot-core/src/config.rs` | Kanban/Swarm/Orchestration 配置 |
| `crates/zerobot-core/src/gateway.rs` | Kanban 调度集成 |
