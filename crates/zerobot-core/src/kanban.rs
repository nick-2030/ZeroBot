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
