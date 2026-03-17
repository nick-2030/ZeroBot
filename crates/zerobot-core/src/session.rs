use crate::error::{ZeroBotError, ZeroBotResult};
use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Pool, Sqlite};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use uuid::Uuid;
use crate::hooks::HookManager;
use crate::skills::SkillStackEntry;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub parent_id: Option<String>,
    pub kind: SessionKind,
    pub created_at: i64,
    pub updated_at: i64,
    pub archived_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SessionKind {
    Main,
    Sub,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub session_id: String,
    pub role: MessageRole,
    pub content: String,
    #[serde(default)]
    pub summary: bool,
    pub tool_call_id: Option<String>,
    pub tool_calls: Option<Vec<StoredToolCall>>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TodoPriority {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TodoItem {
    pub content: String,
    pub status: TodoStatus,
    pub priority: TodoPriority,
}

#[async_trait]
pub trait SessionStore: Send + Sync {
    async fn init(&self) -> ZeroBotResult<()>;
    async fn create_session_with_parent(
        &self,
        title: String,
        parent_id: Option<String>,
        kind: SessionKind,
    ) -> ZeroBotResult<Session>;
    async fn create_session(&self, title: String) -> ZeroBotResult<Session> {
        self.create_session_with_parent(title, None, SessionKind::Main)
            .await
    }
    async fn get_session(&self, id: &str) -> ZeroBotResult<Option<Session>>;
    async fn list_sessions(&self) -> ZeroBotResult<Vec<Session>>;
    async fn append_message(&self, message: Message) -> ZeroBotResult<()>;
    async fn list_messages(&self, session_id: &str) -> ZeroBotResult<Vec<Message>>;
    async fn record_tool_call(
        &self,
        call_id: &str,
        session_id: &str,
        name: &str,
        arguments: &str,
    ) -> ZeroBotResult<String>;
    async fn record_tool_output(&self, tool_call_id: &str, content: &str) -> ZeroBotResult<()>;
    async fn get_skill_stack(&self, session_id: &str) -> ZeroBotResult<Vec<SkillStackEntry>>;
    async fn push_skill_stack(
        &self,
        session_id: &str,
        entry: SkillStackEntry,
    ) -> ZeroBotResult<()>;
    async fn pop_skill_stack(&self, session_id: &str) -> ZeroBotResult<Option<SkillStackEntry>>;
    async fn clear_skill_stack(&self, session_id: &str) -> ZeroBotResult<()>;
    async fn get_todos(&self, session_id: &str) -> ZeroBotResult<Vec<TodoItem>>;
    async fn set_todos(&self, session_id: &str, todos: &[TodoItem]) -> ZeroBotResult<()>;
}

#[derive(Clone)]
pub struct SqliteSessionStore {
    pool: Pool<Sqlite>,
    path: PathBuf,
}

impl SqliteSessionStore {
    pub async fn new(path: impl AsRef<Path>) -> ZeroBotResult<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let url = format!("sqlite://{}", path.to_string_lossy());
        let opts = SqliteConnectOptions::from_str(&url)
            .map_err(|err| ZeroBotError::SessionStore(err.to_string()))
            .map(|opt| opt.create_if_missing(true))?;
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(opts)
            .await?;
        Ok(Self { pool, path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[async_trait]
impl SessionStore for SqliteSessionStore {
    async fn init(&self) -> ZeroBotResult<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                parent_id TEXT,
                kind TEXT NOT NULL DEFAULT 'main',
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                archived_at INTEGER
            );
            "#,
        )
        .execute(&self.pool)
        .await?;

        let _ = sqlx::query("ALTER TABLE sessions ADD COLUMN parent_id TEXT;")
            .execute(&self.pool)
            .await;
        let _ = sqlx::query("ALTER TABLE sessions ADD COLUMN kind TEXT DEFAULT 'main';")
            .execute(&self.pool)
            .await;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS messages (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                summary INTEGER NOT NULL DEFAULT 0,
                tool_call_id TEXT,
                tool_calls_json TEXT,
                created_at INTEGER NOT NULL,
                FOREIGN KEY(session_id) REFERENCES sessions(id)
            );
            "#,
        )
        .execute(&self.pool)
        .await?;

        let _ = sqlx::query("ALTER TABLE messages ADD COLUMN tool_calls_json TEXT;")
            .execute(&self.pool)
            .await;
        let _ = sqlx::query("ALTER TABLE messages ADD COLUMN summary INTEGER DEFAULT 0;")
            .execute(&self.pool)
            .await;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS tool_calls (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                name TEXT NOT NULL,
                arguments TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                FOREIGN KEY(session_id) REFERENCES sessions(id)
            );
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS tool_outputs (
                id TEXT PRIMARY KEY,
                tool_call_id TEXT NOT NULL,
                content TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                FOREIGN KEY(tool_call_id) REFERENCES tool_calls(id)
            );
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id);")
            .execute(&self.pool)
            .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS todos (
                session_id TEXT NOT NULL,
                position INTEGER NOT NULL,
                content TEXT NOT NULL,
                status TEXT NOT NULL,
                priority TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                PRIMARY KEY (session_id, position),
                FOREIGN KEY(session_id) REFERENCES sessions(id)
            );
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_todos_session ON todos(session_id);")
            .execute(&self.pool)
            .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS skill_stack (
                session_id TEXT PRIMARY KEY,
                stack_json TEXT NOT NULL,
                FOREIGN KEY(session_id) REFERENCES sessions(id)
            );
            "#,
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn create_session_with_parent(
        &self,
        title: String,
        parent_id: Option<String>,
        kind: SessionKind,
    ) -> ZeroBotResult<Session> {
        let now = Utc::now().timestamp();
        let session = Session {
            id: Uuid::new_v4().to_string(),
            title,
            parent_id,
            kind,
            created_at: now,
            updated_at: now,
            archived_at: None,
        };
        sqlx::query(
            "INSERT INTO sessions (id, title, parent_id, kind, created_at, updated_at, archived_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&session.id)
        .bind(&session.title)
        .bind(&session.parent_id)
        .bind(session.kind.to_string())
        .bind(session.created_at)
        .bind(session.updated_at)
        .bind(session.archived_at)
        .execute(&self.pool)
        .await?;
        Ok(session)
    }

    async fn get_session(&self, id: &str) -> ZeroBotResult<Option<Session>> {
        let row = sqlx::query_as::<_, (String, String, Option<String>, String, i64, i64, Option<i64>)>(
            "SELECT id, title, parent_id, kind, created_at, updated_at, archived_at FROM sessions WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|row| Session {
            id: row.0,
            title: row.1,
            parent_id: row.2,
            kind: SessionKind::from_str(&row.3),
            created_at: row.4,
            updated_at: row.5,
            archived_at: row.6,
        }))
    }

    async fn list_sessions(&self) -> ZeroBotResult<Vec<Session>> {
        let rows = sqlx::query_as::<_, (String, String, Option<String>, String, i64, i64, Option<i64>)>(
            "SELECT id, title, parent_id, kind, created_at, updated_at, archived_at FROM sessions ORDER BY updated_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|row| Session {
                id: row.0,
                title: row.1,
                parent_id: row.2,
                kind: SessionKind::from_str(&row.3),
                created_at: row.4,
                updated_at: row.5,
                archived_at: row.6,
            })
            .collect())
    }

    async fn append_message(&self, message: Message) -> ZeroBotResult<()> {
        let tool_calls_json = match &message.tool_calls {
            Some(calls) => Some(serde_json::to_string(calls).map_err(|err| {
                ZeroBotError::SessionStore(err.to_string())
            })?),
            None => None,
        };
        sqlx::query(
            "INSERT INTO messages (id, session_id, role, content, summary, tool_call_id, tool_calls_json, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&message.id)
        .bind(&message.session_id)
        .bind(message.role.to_string())
        .bind(&message.content)
        .bind(if message.summary { 1 } else { 0 })
        .bind(&message.tool_call_id)
        .bind(&tool_calls_json)
        .bind(message.created_at)
        .execute(&self.pool)
        .await?;

        sqlx::query("UPDATE sessions SET updated_at = ? WHERE id = ?")
            .bind(Utc::now().timestamp())
            .bind(&message.session_id)
            .execute(&self.pool)
            .await?;

        Ok(())
    }

    async fn list_messages(&self, session_id: &str) -> ZeroBotResult<Vec<Message>> {
        let rows = sqlx::query_as::<_, (String, String, String, String, i64, Option<String>, Option<String>, i64)>(
            "SELECT id, session_id, role, content, summary, tool_call_id, tool_calls_json, created_at FROM messages WHERE session_id = ? ORDER BY created_at ASC",
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|row| Message {
                id: row.0,
                session_id: row.1,
                role: MessageRole::from_str(&row.2),
                content: row.3,
                summary: row.4 != 0,
                tool_call_id: row.5,
                tool_calls: row.6.and_then(|raw| serde_json::from_str(&raw).ok()),
                created_at: row.7,
            })
            .collect())
    }

    async fn record_tool_call(
        &self,
        call_id: &str,
        session_id: &str,
        name: &str,
        arguments: &str,
    ) -> ZeroBotResult<String> {
        let id = call_id.to_string();
        sqlx::query(
            "INSERT OR REPLACE INTO tool_calls (id, session_id, name, arguments, created_at) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(session_id)
        .bind(name)
        .bind(arguments)
        .bind(Utc::now().timestamp())
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    async fn record_tool_output(&self, tool_call_id: &str, content: &str) -> ZeroBotResult<()> {
        let id = Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO tool_outputs (id, tool_call_id, content, created_at) VALUES (?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(tool_call_id)
        .bind(content)
        .bind(Utc::now().timestamp())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_skill_stack(&self, session_id: &str) -> ZeroBotResult<Vec<SkillStackEntry>> {
        let row = sqlx::query_as::<_, (String,)>(
            "SELECT stack_json FROM skill_stack WHERE session_id = ?",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await?;
        if let Some(row) = row {
            let stack: Vec<SkillStackEntry> =
                serde_json::from_str(&row.0).map_err(|err| ZeroBotError::SessionStore(err.to_string()))?;
            Ok(stack)
        } else {
            Ok(Vec::new())
        }
    }

    async fn push_skill_stack(
        &self,
        session_id: &str,
        entry: SkillStackEntry,
    ) -> ZeroBotResult<()> {
        let mut stack = self.get_skill_stack(session_id).await?;
        stack.push(entry);
        let json = serde_json::to_string(&stack)
            .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?;
        sqlx::query(
            "INSERT OR REPLACE INTO skill_stack (session_id, stack_json) VALUES (?, ?)",
        )
        .bind(session_id)
        .bind(json)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn pop_skill_stack(&self, session_id: &str) -> ZeroBotResult<Option<SkillStackEntry>> {
        let mut stack = self.get_skill_stack(session_id).await?;
        let popped = stack.pop();
        let json = serde_json::to_string(&stack)
            .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?;
        sqlx::query(
            "INSERT OR REPLACE INTO skill_stack (session_id, stack_json) VALUES (?, ?)",
        )
        .bind(session_id)
        .bind(json)
        .execute(&self.pool)
        .await?;
        Ok(popped)
    }

    async fn clear_skill_stack(&self, session_id: &str) -> ZeroBotResult<()> {
        sqlx::query("DELETE FROM skill_stack WHERE session_id = ?")
            .bind(session_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn get_todos(&self, session_id: &str) -> ZeroBotResult<Vec<TodoItem>> {
        let rows = sqlx::query_as::<_, (String, String, String)>(
            "SELECT content, status, priority FROM todos WHERE session_id = ? ORDER BY position ASC",
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await?;

        let mut items = Vec::new();
        for (content, status_raw, priority_raw) in rows {
            let status = serde_json::from_str::<TodoStatus>(&format!("\"{status_raw}\""))
                .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?;
            let priority = serde_json::from_str::<TodoPriority>(&format!("\"{priority_raw}\""))
                .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?;
            items.push(TodoItem {
                content,
                status,
                priority,
            });
        }
        Ok(items)
    }

    async fn set_todos(&self, session_id: &str, todos: &[TodoItem]) -> ZeroBotResult<()> {
        let now = Utc::now().timestamp();
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM todos WHERE session_id = ?")
            .bind(session_id)
            .execute(&mut *tx)
            .await?;
        for (position, todo) in todos.iter().enumerate() {
            let status = serde_json::to_string(&todo.status)
                .unwrap_or_else(|_| "\"pending\"".to_string())
                .trim_matches('"')
                .to_string();
            let priority = serde_json::to_string(&todo.priority)
                .unwrap_or_else(|_| "\"medium\"".to_string())
                .trim_matches('"')
                .to_string();
            sqlx::query(
                "INSERT INTO todos (session_id, position, content, status, priority, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(session_id)
            .bind(position as i64)
            .bind(&todo.content)
            .bind(status)
            .bind(priority)
            .bind(now)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }
}

impl MessageRole {
    pub fn from_str(raw: &str) -> Self {
        match raw {
            "system" => MessageRole::System,
            "assistant" => MessageRole::Assistant,
            "tool" => MessageRole::Tool,
            _ => MessageRole::User,
        }
    }
}

impl SessionKind {
    pub fn from_str(raw: &str) -> Self {
        match raw {
            "sub" => SessionKind::Sub,
            _ => SessionKind::Main,
        }
    }
}

impl StoredToolCall {
    pub fn from_provider_call(call: crate::provider::ToolCall) -> Self {
        Self {
            id: call.id,
            name: call.name,
            arguments: call.arguments,
        }
    }

    pub fn to_provider_call(&self) -> crate::provider::ToolCall {
        crate::provider::ToolCall {
            id: self.id.clone(),
            name: self.name.clone(),
            arguments: self.arguments.clone(),
        }
    }
}

impl ToString for MessageRole {
    fn to_string(&self) -> String {
        match self {
            MessageRole::System => "system".to_string(),
            MessageRole::User => "user".to_string(),
            MessageRole::Assistant => "assistant".to_string(),
            MessageRole::Tool => "tool".to_string(),
        }
    }
}

impl ToString for SessionKind {
    fn to_string(&self) -> String {
        match self {
            SessionKind::Main => "main".to_string(),
            SessionKind::Sub => "sub".to_string(),
        }
    }
}

pub async fn create_session_with_hooks(
    store: &dyn SessionStore,
    hooks: &HookManager,
    title: String,
    parent_id: Option<String>,
    kind: SessionKind,
) -> ZeroBotResult<Session> {
    let session = store
        .create_session_with_parent(title, parent_id, kind)
        .await?;
    hooks
        .run_session_start(&session.id, session.parent_id.clone(), session.kind.clone())
        .await;
    Ok(session)
}

pub async fn end_session_with_hooks(
    hooks: &HookManager,
    session_id: &str,
) {
    hooks.run_session_end(session_id).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    #[tokio::test]
    async fn sqlite_store_creates_and_reads_session() {
        let dir = TempDir::new().unwrap();
        let store = SqliteSessionStore::new(dir.path().join("test.db")).await.unwrap();
        store.init().await.unwrap();
        let session = store.create_session("test".to_string()).await.unwrap();
        let fetched = store.get_session(&session.id).await.unwrap().unwrap();
        assert_eq!(session.id, fetched.id);
        assert_eq!(fetched.kind, SessionKind::Main);
    }

    #[tokio::test]
    async fn sqlite_store_creates_child_session() {
        let dir = TempDir::new().unwrap();
        let store = SqliteSessionStore::new(dir.path().join("test.db")).await.unwrap();
        store.init().await.unwrap();
        let parent = store.create_session("parent".to_string()).await.unwrap();
        let child = store
            .create_session_with_parent("child".to_string(), Some(parent.id.clone()), SessionKind::Sub)
            .await
            .unwrap();
        assert_eq!(child.parent_id, Some(parent.id));
        assert_eq!(child.kind, SessionKind::Sub);
    }

    #[tokio::test]
    async fn skill_stack_push_pop() {
        let dir = TempDir::new().unwrap();
        let store = SqliteSessionStore::new(dir.path().join("test.db")).await.unwrap();
        store.init().await.unwrap();
        let session = store.create_session("test".to_string()).await.unwrap();
        let entry = SkillStackEntry {
            name: "demo".to_string(),
            description: "示例".to_string(),
            path: dir.path().join("SKILL.md"),
            hooks: Vec::new(),
            started_at: 0,
        };
        store.push_skill_stack(&session.id, entry.clone()).await.unwrap();
        let stack = store.get_skill_stack(&session.id).await.unwrap();
        assert_eq!(stack.len(), 1);
        let popped = store.pop_skill_stack(&session.id).await.unwrap().unwrap();
        assert_eq!(popped.name, "demo");
        let stack = store.get_skill_stack(&session.id).await.unwrap();
        assert!(stack.is_empty());
    }

    #[tokio::test]
    async fn todo_set_and_get() {
        let dir = TempDir::new().unwrap();
        let store = SqliteSessionStore::new(dir.path().join("test.db")).await.unwrap();
        store.init().await.unwrap();
        let session = store.create_session("test".to_string()).await.unwrap();

        let todos = vec![
            TodoItem {
                content: "第一步".to_string(),
                status: TodoStatus::Pending,
                priority: TodoPriority::High,
            },
            TodoItem {
                content: "第二步".to_string(),
                status: TodoStatus::InProgress,
                priority: TodoPriority::Medium,
            },
        ];

        store.set_todos(&session.id, &todos).await.unwrap();
        let read_back = store.get_todos(&session.id).await.unwrap();
        assert_eq!(read_back, todos);

        store.set_todos(&session.id, &[]).await.unwrap();
        let cleared = store.get_todos(&session.id).await.unwrap();
        assert!(cleared.is_empty());
    }
}
