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
                reason: String::new(),
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

// ============ Kanban Tools ============

use async_trait::async_trait;
use crate::tool::{Tool, ToolContext, ToolOutput};

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
    fn parameters(&self) -> JsonValue {
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
    async fn run(&self, _ctx: &ToolContext, args: JsonValue) -> ZeroBotResult<ToolOutput> {
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
    fn parameters(&self) -> JsonValue {
        serde_json::json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "任务 ID（查看单个）" },
                "status": { "type": "string", "description": "按状态过滤" },
                "assignee": { "type": "string", "description": "按分配者过滤" }
            }
        })
    }
    async fn run(&self, _ctx: &ToolContext, args: JsonValue) -> ZeroBotResult<ToolOutput> {
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
    fn parameters(&self) -> JsonValue {
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
    async fn run(&self, _ctx: &ToolContext, args: JsonValue) -> ZeroBotResult<ToolOutput> {
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
    fn parameters(&self) -> JsonValue {
        serde_json::json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "任务 ID" },
                "reason": { "type": "string", "description": "阻塞原因" }
            },
            "required": ["id", "reason"]
        })
    }
    async fn run(&self, _ctx: &ToolContext, args: JsonValue) -> ZeroBotResult<ToolOutput> {
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
    fn parameters(&self) -> JsonValue {
        serde_json::json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "任务 ID" },
                "comment": { "type": "string", "description": "评论内容" }
            },
            "required": ["id", "comment"]
        })
    }
    async fn run(&self, _ctx: &ToolContext, args: JsonValue) -> ZeroBotResult<ToolOutput> {
        let id = args["id"].as_str().ok_or_else(|| ZeroBotError::Tool("缺少 id".into()))?;
        let comment = args["comment"].as_str().ok_or_else(|| ZeroBotError::Tool("缺少 comment".into()))?;

        self.manager.comment(id, comment).await?;
        Ok(ToolOutput::new(format!("已为任务 {} 添加评论", id)))
    }
}
