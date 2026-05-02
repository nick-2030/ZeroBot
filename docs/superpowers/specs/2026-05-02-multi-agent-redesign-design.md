# ZeroBot 多智能体系统重设计

日期: 2026-05-02
状态: 待审核
参考: hermes-agent (Python), Claude-Code (TypeScript)

## 新增依赖

- `tokio-util` — 提供 `CancellationToken`（用于 agent 中断机制）
- `chrono` — Kanban 时间戳（如果尚未引入）

## 概述

对 ZeroBot 的多智能体系统进行全面重构，从当前的简单同步 subagent 模式升级为支持后台执行、agent 间协调、编排模式和团队协作的完整多智能体框架。同时重构工具系统，引入 toolset 分组机制。

## 当前架构问题

1. `SubagentTool` 只支持同步执行，父 agent 阻塞等待
2. 无 agent 间通信机制，只有父子返回
3. 无任务状态管理，后台工作无法追踪
4. 无编排模式，复杂任务无法自动化分解
5. 工具系统无分组，agent 定义工具列表冗长

## 设计方案：核心重构（方案 B）

保持 Provider、Session、Hook 等子系统不变，重点重构 Agent 编排层和工具系统。

---

## 1. Task 状态机

### 目标

所有后台工作（agent、shell、cron、workflow）统一为 Task，有完整的生命周期管理。

### 核心类型

```rust
// crates/zerobot-core/src/task.rs

pub struct TaskId(String);
// 前缀约定: "a_" agent, "b_" shell, "c_" cron, "w_" workflow

pub enum TaskType {
    Agent,
    Shell,
    Cron,
    Workflow,
}

pub enum TaskStatus {
    Pending,
    Running,
    Completed,
    Failed(String),
    Killed,
}

pub struct TaskState {
    pub id: TaskId,
    pub task_type: TaskType,
    pub status: TaskStatus,
    pub description: String,
    pub parent_task_id: Option<TaskId>,
    pub agent_type: Option<String>,
    pub started_at: Option<Instant>,
    pub output_file: Option<PathBuf>,  // 详细输出写文件，不占内存
    pub notified: bool,                // 是否已通知父 agent
}

pub struct TaskUsage {
    pub total_tokens: u64,
    pub tool_uses: u32,
    pub duration_ms: u64,
}
```

### TaskManager

```rust
pub struct TaskManager {
    tasks: Arc<RwLock<HashMap<TaskId, TaskState>>>,
    db: Arc<dyn SessionStore>,
    abort_tokens: HashMap<TaskId, CancellationToken>,
}

impl TaskManager {
    pub fn register(&self, task_type: TaskType, description: String, parent: Option<TaskId>) -> TaskId;
    pub fn update_status(&self, id: &TaskId, status: TaskStatus);
    pub fn get_state(&self, id: &TaskId) -> Option<TaskState>;
    pub fn list_by_status(&self, status: TaskStatus) -> Vec<TaskState>;
    pub fn cancel(&self, id: &TaskId);
    pub fn cancel_all(&self);
}
```

### 设计决策

- Task 输出写文件（参考 Claude-Code 的 `outputFile`），不存内存，避免大输出 OOM
- 使用 `tokio_util::sync::CancellationToken` 进行取消和中断
- TaskManager 持有所有活跃 task 的引用，支持全局 cancel_all
- Task 状态持久化到 SQLite，支持跨重启恢复

---

## 2. Agent 重构

### Agent struct 变更

```rust
pub struct Agent {
    // --- 现有字段保持不变 ---
    provider: Box<dyn Provider>,
    model: String,
    settings: Settings,
    store: Arc<dyn SessionStore>,
    tools: ToolRegistry,
    cwd: PathBuf,
    hooks: HookManager,
    interaction: Option<Arc<dyn InteractionHandler>>,
    plugins: Option<Arc<PluginManager>>,
    tool_approvals: Arc<RwLock<HashSet<String>>>,
    outbound: Option<mpsc::UnboundedSender<OutboundMessage>>,
    denial_counts: DenialCounts,

    // --- 新增字段 ---
    task_id: Option<TaskId>,
    parent_task_id: Option<TaskId>,
    abort_token: CancellationToken,
    notification_tx: Option<mpsc::UnboundedSender<Notification>>,
    agent_type: String,
    iteration_budget: Option<u32>,
}

pub type NotificationSender = mpsc::UnboundedSender<Notification>;
```

### run_turn 变更

现有 `run_turn()` 循环核心逻辑不变，增加：

1. **中断检查**：每轮循环开始检查 `abort_token.is_cancelled()`
2. **迭代预算**：每轮递减计数器，耗尽则停止
3. **进度回调**：每轮 tool 执行后通过 `notification_tx` 发送进度
4. **异步返回**：后台 agent 的最终结果通过 NotificationBus 回报

```rust
impl Agent {
    // 现有同步接口（兼容）
    pub async fn run_turn(&mut self, ...) -> ZeroBotResult<AgentResult>;

    // 新增：后台运行，返回 TaskId
    pub async fn run_background(
        self,
        task_manager: Arc<TaskManager>,
        notification_tx: NotificationSender,
    ) -> ZeroBotResult<TaskId>;

    // 新增：通过 CancellationToken 中断
    pub fn abort(&self) {
        self.abort_token.cancel();
    }
}

pub enum AgentResult {
    Done { message: String, usage: TaskUsage },
    Interrupted { reason: String },
    BudgetExhausted { steps_used: u32 },
    Failed(ZeroBotError),
}
```

### 设计决策

- `run_turn()` 保持 async，不改签名，现有调用方无感
- `run_background()` 是新方法，内部 spawn tokio task，返回 TaskId
- 迭代预算参考 Hermes，防止 agent 无限循环
- abort_token 贯穿整个 agent 生命周期，支持优雅取消

---

## 3. AgentDispatcher（统一 Agent 分发）

替代现有 `SubagentTool` 的单一同步模式，提供统一的 agent 分发入口。

### 分发模式

```rust
pub enum DispatchMode {
    Sync,                                    // 同步阻塞
    Background { name: Option<String> },     // 后台执行
    Fork,                                    // 继承父上下文，后台执行
    Teammate { team_name: String },          // 团队模式
}
```

### 辅助类型

```rust
pub enum IsolationMode {
    Worktree,  // git worktree 隔离
    Remote,    // 远程隔离
}

pub struct ToolRestrictions {
    pub allowlist: Option<Vec<String>>,  // 只允许这些工具
    pub blocklist: Option<Vec<String>>,  // 禁止这些工具
}
```

### 分发请求和结果

```rust
pub struct DispatchRequest {
    pub agent_type: String,
    pub prompt: String,
    pub mode: DispatchMode,
    pub model_override: Option<String>,
    pub tool_overrides: Option<ToolRestrictions>,
    pub cwd: Option<PathBuf>,
    pub max_turns: Option<u32>,
    pub isolation: Option<IsolationMode>,
}

pub enum DispatchResult {
    Sync(AgentResult),
    Background(TaskId),
    Fork(TaskId),
    Teammate { agent_name: String, team_name: String },
}
```

### AgentDispatcher

```rust
pub struct AgentDispatcher {
    task_manager: Arc<TaskManager>,
    agent_manager: Arc<AgentManager>,
    notification_bus: Arc<NotificationBus>,
    session_store: Arc<dyn SessionStore>,
    settings: Settings,
}

impl AgentDispatcher {
    pub async fn dispatch(&self, request: DispatchRequest) -> ZeroBotResult<DispatchResult>;
    pub async fn send_message(&self, task_id: &TaskId, message: String) -> ZeroBotResult<()>;
    pub async fn terminate(&self, task_id: &TaskId) -> ZeroBotResult<()>;

    fn resolve_tools(&self, def: &AgentDefinition, overrides: &Option<ToolRestrictions>) -> ZeroBotResult<ToolRegistry>;
    fn build_agent(&self, def: AgentDefinition, tools: ToolRegistry, ...) -> ZeroBotResult<Agent>;
}
```

### SubagentTool 升级

现有 `SubagentTool` 改为 `AgentDispatcherTool`，内部委托给 `AgentDispatcher`：

```rust
pub struct AgentDispatcherTool {
    dispatcher: Arc<AgentDispatcher>,
}

impl Tool for AgentDispatcherTool {
    fn name(&self) -> &str { "agent" }
    // 参数: description, prompt, subagent_type, model, run_in_background,
    //       name, team_name, isolation, cwd
}
```

### SendMessageTool

独立工具，用于向已运行的后台 agent 发送后续消息：

```rust
pub struct SendMessageTool {
    dispatcher: Arc<AgentDispatcher>,
}

impl Tool for SendMessageTool {
    fn name(&self) -> &str { "send_message" }
    // 参数: task_id (str), message (str)
    // 通过 AgentDispatcher::send_message() 实现
}
```

### 设计决策

- AgentDispatcher 是独立 struct，不耦合 Tool trait
- SubagentTool 改名为 AgentDispatcherTool，tool name 改为 "agent"
- SendMessageTool 独立注册，target 是 TaskId 或 agent name
- 四种分发模式覆盖所有场景

---

## 4. 通知系统

后台 agent 通过结构化通知将结果回报给父 agent。

### NotificationBus

```rust
// crates/zerobot-core/src/notification.rs

pub struct Notification {
    pub task_id: TaskId,
    pub agent_type: String,
    pub description: String,
    pub status: NotificationStatus,
    pub result: Option<String>,
    pub usage: Option<TaskUsage>,
    pub timestamp: Instant,
}

pub enum NotificationStatus {
    Completed,
    Failed(String),
    Killed,
    Progress { summary: String },
}

pub struct NotificationBus {
    channels: Arc<RwLock<HashMap<TaskId, mpsc::UnboundedSender<Notification>>>>,
}

impl NotificationBus {
    pub fn subscribe(&self, parent_task_id: TaskId) -> mpsc::UnboundedReceiver<Notification>;
    pub fn notify(&self, parent_task_id: &TaskId, notification: Notification);
    pub fn unsubscribe(&self, parent_task_id: &TaskId);
}
```

### 通知格式

通知注入为结构化消息到父 agent 的消息流中：

```
<task-notification>
<task-id>a_001</task-id>
<agent-type>Explore</agent-type>
<status>completed</status>
<summary>找到了 3 个相关文件</summary>
<result>详细的探索结果...</result>
<usage>
  <total_tokens>15000</total_tokens>
  <tool_uses>12</tool_uses>
  <duration_ms>8500</duration_ms>
</usage>
</task-notification>
```

### 设计决策

- 通知是进程内 mpsc::unbounded_channel，不走文件
- 通知格式与 Claude-Code 的 task-notification 对齐
- 支持进度通知（不只是完成/失败）
- 通知在 run_turn 循环中通过 try_recv 非阻塞检查

---

## 5. Coordinator 编排模式

### AgentRole 扩展

```rust
// crates/zerobot-core/src/agents.rs

pub enum AgentRole {
    Worker,                      // 普通 worker
    Coordinator,                 // 编排多个 worker
    Orchestrator { max_depth: u32 },  // 可递归派发（Hermes 模式）
}

pub struct AgentDefinition {
    // 现有字段不变...

    // 新增
    pub role: AgentRole,
    pub toolsets: Option<Vec<String>>,
    pub max_turns: Option<u32>,
    pub background: bool,
    pub omit_context: bool,
    pub isolation: Option<IsolationMode>,
}
```

### 内置 Coordinator 系统提示

文件: `crates/zerobot-core/prompts/modes/coordinator.md`

Coordinator 执行 4 阶段工作流：
1. **研究阶段**：并行派发只读 worker 进行探索
2. **综合阶段**：等待所有 worker 完成，综合分析结果
3. **实现阶段**：分发实现任务给 worker
4. **验证阶段**：分发验证任务给 worker

### 递归深度限制

```rust
impl AgentDispatcher {
    async fn dispatch_coordinator(&self, request: DispatchRequest, depth: u32) -> ... {
        if depth >= self.settings.agent.max_orchestration_depth {  // 默认 3
            return Err(ZeroBotError::OrchestrationDepthExceeded);
        }
        // ...
    }
}
```

### 设计决策

- Coordinator 是 AgentRole 变体，不是独立 struct
- Orchestrator 递归有深度限制（默认 3）
- Coordinator 的工具限制为 agent + 只读工具
- 用户可以自定义 coordinator 系统提示

---

## 6. Kanban 共享状态协调

### 数据模型

```rust
// crates/zerobot-core/src/kanban.rs

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

pub enum KanbanStatus {
    Todo,
    InProgress,
    Blocked { reason: String },
    Completed,
    Cancelled,
}
```

### 辅助类型

```rust
pub struct NewKanbanTask {
    pub title: String,
    pub description: String,
    pub assignee: Option<String>,
    pub parent_id: Option<String>,
    pub metadata: Option<JsonValue>,
}

pub struct KanbanFilter {
    pub status: Option<KanbanStatus>,
    pub assignee: Option<String>,
    pub parent_id: Option<String>,
}
```

### KanbanManager

```rust
pub struct KanbanManager {
    db: Arc<dyn SessionStore>,
}

impl KanbanManager {
    pub async fn create(&self, task: NewKanbanTask) -> ZeroBotResult<KanbanTask>;
    pub async fn get(&self, id: &str) -> ZeroBotResult<Option<KanbanTask>>;
    pub async fn list(&self, filter: KanbanFilter) -> ZeroBotResult<Vec<KanbanTask>>;
    pub async fn update_status(&self, id: &str, status: KanbanStatus) -> ZeroBotResult<()>;
    pub async fn assign(&self, id: &str, agent: &str) -> ZeroBotResult<()>;
    pub async fn comment(&self, id: &str, comment: &str) -> ZeroBotResult<()>;
    pub async fn complete(&self, id: &str, summary: &str, metadata: JsonValue) -> ZeroBotResult<()>;
}
```

### Kanban 工具集

注册为独立 toolset：`kanban_create`, `kanban_show`, `kanban_complete`, `kanban_block`, `kanban_comment`

### Gateway 调度器

在 GatewayRuntime 中增加 kanban 调度 tick，自动为 Todo 状态的任务 spawn 后台 agent。

### 设计决策

- Kanban 是可选模块，通过配置启用
- 数据库复用现有 SQLite，增加 kanban_tasks 和 kanban_comments 表
- 调度器集成到 Gateway，支持自动化任务分发

---

## 7. Swarm/Teammate 模式

### 后端抽象

```rust
// crates/zerobot-core/src/swarm/mod.rs

#[async_trait]
pub trait TeammateBackend: Send + Sync {
    async fn spawn(&self, config: TeammateConfig) -> ZeroBotResult<TeammateHandle>;
    async fn send_message(&self, handle: &TeammateHandle, message: String) -> ZeroBotResult<()>;
    async fn terminate(&self, handle: &TeammateHandle) -> ZeroBotResult<()>;
    async fn is_active(&self, handle: &TeammateHandle) -> ZeroBotResult<bool>;
}

pub enum BackendType {
    InProcess,   // 同进程 tokio task
    Tmux,        // tmux pane
    External,    // 外部进程（ACP/stdio）
}
```

### 后端实现

- `InProcessBackend`：同进程 tokio task，通过文件邮箱通信
- `TmuxBackend`：tmux pane，通过 tmux send-keys 通信
- `ExternalBackend`：外部进程，通过 ACP stdio 通信

### 邮箱 IPC

```rust
pub struct Mailbox { dir: PathBuf }

impl Mailbox {
    pub fn send(&self, agent_name: &str, team_name: &str, message: &str) -> ZeroBotResult<()>;
    pub fn drain(&self, agent_name: &str, team_name: &str) -> ZeroBotResult<Vec<String>>;
}
```

### SwarmManager

```rust
pub struct SwarmManager {
    backends: HashMap<BackendType, Box<dyn TeammateBackend>>,
    default_backend: BackendType,
}
```

### 后端选择策略

tmux 可用时优先使用 tmux（可见性好），否则回退到 in-process。

### 设计决策

- Swarm 是可选模块
- 后端自动选择
- 邮箱 IPC 用文件系统，跨后端兼容
- Teammate 复用 AgentDispatcher

---

## 8. 工具系统重构

### Toolset 定义

```rust
// crates/zerobot-core/src/toolset.rs

pub struct ToolsetDefinition {
    pub name: String,
    pub description: String,
    pub tools: Vec<String>,
    pub includes: Vec<String>,  // 组合其他 toolset
}

pub struct ToolsetRegistry {
    toolsets: HashMap<String, ToolsetDefinition>,
}
```

### 内置 Toolset

| 名称 | 描述 | 工具 |
|------|------|------|
| filesystem | 文件读写 | read, write, edit, apply_patch, patch, glob, grep |
| shell | Shell 执行 | bash, shell |
| code | 代码分析 | includes: filesystem, tools: bash |
| web | 网络搜索 | web_search, web_fetch |
| task | 任务管理 | todo_read, todo_write |
| agent | 多智能体调度 | agent, send_message |
| kanban | 看板协调 | kanban_create, kanban_show, kanban_complete, kanban_block, kanban_comment |
| swarm | 团队协作 | spawn_teammate, send_teammate_message, list_teammates |

### ToolRegistry 增强

```rust
impl ToolRegistry {
    pub fn with_toolsets(&self, toolsets: &ToolsetRegistry, names: &[String]) -> ZeroBotResult<ToolRegistry>;
    pub fn with_allowlist(&self, allowed: &[String]) -> ToolRegistry;
    pub fn with_blocklist(&self, blocked: &[String]) -> ToolRegistry;
}
```

### AgentDefinition 使用 toolset

```markdown
---
name: explore
description: 只读代码探索
role: worker
toolsets: [filesystem, web]
disallowed_tools: [write, edit, apply_patch]
model: haiku
omit_context: true
---
```

### 设计决策

- Tool trait 签名不变，toolset 是 ToolRegistry 层面的过滤
- Toolset 支持组合（includes）
- Toolset 定义可从配置文件加载

---

## 实现分步计划

### Phase 1: 基础设施
1. Task 状态机（task.rs）— TaskId, TaskState, TaskManager
2. NotificationBus（notification.rs）
3. Agent struct 新增字段（abort_token, task_id 等）
4. Agent::run_background() 方法

### Phase 2: Agent 分发
1. AgentDispatcher（agent_dispatch.rs）
2. AgentDispatcherTool 替换 SubagentTool
3. 四种分发模式实现
4. SendMessage 语义

### Phase 3: 工具系统
1. ToolsetDefinition + ToolsetRegistry（toolset.rs）
2. 内置 toolset 定义
3. ToolRegistry 增强（with_toolsets 等）
4. AgentDefinition 扩展（role, toolsets 等）

### Phase 4: Coordinator 模式
1. coordinator.md 系统提示
2. AgentRole 实现
3. 递归深度限制
4. 内置 coordinator agent 注册

### Phase 5: Kanban 模块
1. KanbanManager + SQLite 表
2. Kanban 工具集（5 个工具）
3. Gateway kanban 调度 tick
4. kanban worker agent 定义

### Phase 6: Swarm 模块
1. TeammateBackend trait
2. InProcessBackend 实现
3. Mailbox IPC
4. SwarmManager
5. Swarm 工具集

### Phase 7: 集成测试
1. 端到端 coordinator 流程测试
2. 后台 agent + 通知测试
3. Kanban 调度测试
4. Swarm teammate 测试

---

## 文件变更清单

### 新增文件

| 文件 | 描述 |
|------|------|
| `crates/zerobot-core/src/task.rs` | Task 状态机和 TaskManager |
| `crates/zerobot-core/src/notification.rs` | NotificationBus |
| `crates/zerobot-core/src/agent_dispatch.rs` | AgentDispatcher |
| `crates/zerobot-core/src/toolset.rs` | ToolsetDefinition + ToolsetRegistry |
| `crates/zerobot-core/src/kanban.rs` | KanbanManager + 工具 |
| `crates/zerobot-core/src/swarm/mod.rs` | SwarmManager + TeammateBackend trait |
| `crates/zerobot-core/src/swarm/in_process.rs` | InProcessBackend |
| `crates/zerobot-core/src/swarm/mailbox.rs` | 文件邮箱 IPC |
| `crates/zerobot-core/prompts/modes/coordinator.md` | Coordinator 系统提示 |

### 修改文件

| 文件 | 变更 |
|------|------|
| `crates/zerobot-core/src/agent.rs` | Agent struct 新增字段，run_turn 增加中断检查 |
| `crates/zerobot-core/src/agents.rs` | AgentDefinition 新增 role, toolsets 等字段 |
| `crates/zerobot-core/src/tool.rs` | SubagentTool → AgentDispatcherTool |
| `crates/zerobot-core/src/lib.rs` | 新模块声明 |
| `crates/zerobot-core/src/config.rs` | Settings 新增 kanban, swarm, orchestration 配置 |
| `crates/zerobot-core/src/gateway.rs` | kanban 调度 tick |
