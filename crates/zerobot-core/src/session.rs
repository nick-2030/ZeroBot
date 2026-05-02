use crate::error::{ZeroBotError, ZeroBotResult};
use crate::hooks::HookManager;
use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Pool, Sqlite};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub parent_id: Option<String>,
    pub kind: SessionKind,
    pub created_at: i64,
    pub updated_at: i64,
    pub archived_at: Option<i64>,
    #[serde(default)]
    pub first_ai_message: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileReadRecord {
    pub session_id: String,
    pub path: String,
    pub mtime: i64,
    pub read_at: i64,
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
    /// Present continuous form shown during execution, e.g. "Running tests" (optional)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_form: Option<String>,
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
    async fn record_file_read(&self, session_id: &str, path: &str, mtime: i64)
        -> ZeroBotResult<()>;
    async fn get_file_read(
        &self,
        session_id: &str,
        path: &str,
    ) -> ZeroBotResult<Option<FileReadRecord>>;
    async fn get_todos(&self, session_id: &str) -> ZeroBotResult<Vec<TodoItem>>;
    async fn set_todos(&self, session_id: &str, todos: &[TodoItem]) -> ZeroBotResult<()>;
    async fn list_tool_approvals(&self) -> ZeroBotResult<Vec<String>>;
    async fn insert_tool_approval(&self, key: &str) -> ZeroBotResult<()>;
    async fn update_session_brief(
        &self,
        session_id: &str,
        first_ai_message: Option<&str>,
        summary: Option<&str>,
    ) -> ZeroBotResult<()>;
    async fn rewind_to_before_message(
        &self,
        _session_id: &str,
        _message_id: &str,
    ) -> ZeroBotResult<()> {
        Err(ZeroBotError::SessionStore(
            "当前会话存储不支持回退消息".to_string(),
        ))
    }
    async fn delete_session(&self, session_id: &str) -> ZeroBotResult<()>;
    async fn search_messages(
        &self,
        session_id: &str,
        query: &str,
        limit: usize,
    ) -> ZeroBotResult<Vec<Message>> {
        let messages = self.list_messages(session_id).await?;
        let query_lower = query.to_lowercase();
        Ok(messages
            .into_iter()
            .filter(|m| m.content.to_lowercase().contains(&query_lower))
            .take(limit)
            .collect())
    }
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
                archived_at INTEGER,
                first_ai_message TEXT,
                summary TEXT
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
        let _ = sqlx::query("ALTER TABLE sessions ADD COLUMN first_ai_message TEXT;")
            .execute(&self.pool)
            .await;
        let _ = sqlx::query("ALTER TABLE sessions ADD COLUMN summary TEXT;")
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

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS file_reads (
                session_id TEXT NOT NULL,
                path TEXT NOT NULL,
                mtime INTEGER NOT NULL,
                read_at INTEGER NOT NULL,
                PRIMARY KEY (session_id, path),
                FOREIGN KEY(session_id) REFERENCES sessions(id)
            );
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id);")
            .execute(&self.pool)
            .await?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_file_reads_session ON file_reads(session_id);")
            .execute(&self.pool)
            .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS todos (
                session_id TEXT NOT NULL,
                position INTEGER NOT NULL,
                content TEXT NOT NULL,
                active_form TEXT,
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
        // Migration: add active_form column to existing tables
        let _ = sqlx::query("ALTER TABLE todos ADD COLUMN active_form TEXT")
            .execute(&self.pool)
            .await;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_todos_session ON todos(session_id);")
            .execute(&self.pool)
            .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS tool_approvals (
                approval_key TEXT PRIMARY KEY,
                created_at INTEGER NOT NULL
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
            first_ai_message: None,
            summary: None,
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
        let row = sqlx::query_as::<_, (String, String, Option<String>, String, i64, i64, Option<i64>, Option<String>, Option<String>)>(
            "SELECT id, title, parent_id, kind, created_at, updated_at, archived_at, first_ai_message, summary FROM sessions WHERE id = ?",
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
            first_ai_message: row.7,
            summary: row.8,
        }))
    }

    async fn list_sessions(&self) -> ZeroBotResult<Vec<Session>> {
        let rows = sqlx::query_as::<_, (String, String, Option<String>, String, i64, i64, Option<i64>, Option<String>, Option<String>)>(
            "SELECT id, title, parent_id, kind, created_at, updated_at, archived_at, first_ai_message, summary FROM sessions ORDER BY updated_at DESC",
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
                first_ai_message: row.7,
                summary: row.8,
            })
            .collect())
    }

    async fn append_message(&self, message: Message) -> ZeroBotResult<()> {
        let tool_calls_json = match &message.tool_calls {
            Some(calls) => Some(
                serde_json::to_string(calls)
                    .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?,
            ),
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

    async fn record_file_read(
        &self,
        session_id: &str,
        path: &str,
        mtime: i64,
    ) -> ZeroBotResult<()> {
        let read_at = Utc::now().timestamp();
        sqlx::query(
            r#"
            INSERT INTO file_reads (session_id, path, mtime, read_at)
            VALUES (?, ?, ?, ?)
            ON CONFLICT(session_id, path) DO UPDATE SET
              mtime = excluded.mtime,
              read_at = excluded.read_at
            "#,
        )
        .bind(session_id)
        .bind(path)
        .bind(mtime)
        .bind(read_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_file_read(
        &self,
        session_id: &str,
        path: &str,
    ) -> ZeroBotResult<Option<FileReadRecord>> {
        let record = sqlx::query_as::<_, (String, String, i64, i64)>(
            "SELECT session_id, path, mtime, read_at FROM file_reads WHERE session_id = ? AND path = ?",
        )
        .bind(session_id)
        .bind(path)
        .fetch_optional(&self.pool)
        .await?;
        Ok(
            record.map(|(session_id, path, mtime, read_at)| FileReadRecord {
                session_id,
                path,
                mtime,
                read_at,
            }),
        )
    }

    async fn get_todos(&self, session_id: &str) -> ZeroBotResult<Vec<TodoItem>> {
        let rows = sqlx::query_as::<_, (String, String, String, Option<String>)>(
            "SELECT content, status, priority, active_form FROM todos WHERE session_id = ? ORDER BY position ASC",
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await?;

        let mut items = Vec::new();
        for (content, status_raw, priority_raw, active_form) in rows {
            let status = serde_json::from_str::<TodoStatus>(&format!("\"{status_raw}\""))
                .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?;
            let priority = serde_json::from_str::<TodoPriority>(&format!("\"{priority_raw}\""))
                .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?;
            items.push(TodoItem {
                content,
                status,
                priority,
                active_form,
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
                "INSERT INTO todos (session_id, position, content, active_form, status, priority, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(session_id)
            .bind(position as i64)
            .bind(&todo.content)
            .bind(&todo.active_form)
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

    async fn list_tool_approvals(&self) -> ZeroBotResult<Vec<String>> {
        let rows = sqlx::query_as::<_, (String,)>("SELECT approval_key FROM tool_approvals")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.into_iter().map(|row| row.0).collect())
    }

    async fn insert_tool_approval(&self, key: &str) -> ZeroBotResult<()> {
        sqlx::query(
            "INSERT OR IGNORE INTO tool_approvals (approval_key, created_at) VALUES (?, ?)",
        )
        .bind(key)
        .bind(Utc::now().timestamp())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn update_session_brief(
        &self,
        session_id: &str,
        first_ai_message: Option<&str>,
        summary: Option<&str>,
    ) -> ZeroBotResult<()> {
        sqlx::query(
            r#"
            UPDATE sessions SET
              first_ai_message = CASE
                WHEN first_ai_message IS NULL AND ?1 IS NOT NULL THEN ?1
                ELSE first_ai_message
              END,
              summary = CASE
                WHEN summary IS NULL AND ?2 IS NOT NULL THEN ?2
                ELSE summary
              END
            WHERE id = ?3
            "#,
        )
        .bind(first_ai_message)
        .bind(summary)
        .bind(session_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn rewind_to_before_message(
        &self,
        session_id: &str,
        message_id: &str,
    ) -> ZeroBotResult<()> {
        let mut tx = self.pool.begin().await?;
        let rows = sqlx::query_as::<_, (String, Option<String>)>(
            r#"
            SELECT id, tool_call_id
            FROM messages
            WHERE session_id = ?
            ORDER BY created_at ASC, rowid ASC
            "#,
        )
        .bind(session_id)
        .fetch_all(&mut *tx)
        .await?;

        let Some(idx) = rows.iter().position(|(id, _)| id == message_id) else {
            return Err(ZeroBotError::SessionStore(format!(
                "未找到可回退的消息: {message_id}"
            )));
        };

        let mut tool_call_ids = std::collections::HashSet::new();
        for (id, tool_call_id) in rows.iter().skip(idx) {
            if let Some(call_id) = tool_call_id {
                tool_call_ids.insert(call_id.clone());
            }
            sqlx::query("DELETE FROM messages WHERE id = ?")
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }

        for call_id in tool_call_ids {
            sqlx::query("DELETE FROM tool_outputs WHERE tool_call_id = ?")
                .bind(&call_id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM tool_calls WHERE id = ?")
                .bind(&call_id)
                .execute(&mut *tx)
                .await?;
        }

        sqlx::query("UPDATE sessions SET updated_at = ? WHERE id = ?")
            .bind(Utc::now().timestamp())
            .bind(session_id)
            .execute(&mut *tx)
            .await?;

        tx.commit().await?;
        Ok(())
    }

    async fn delete_session(&self, session_id: &str) -> ZeroBotResult<()> {
        sqlx::query("DELETE FROM sessions WHERE id = ?")
            .bind(session_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn search_messages(
        &self,
        session_id: &str,
        query: &str,
        limit: usize,
    ) -> ZeroBotResult<Vec<Message>> {
        let pattern = format!("%{}%", query);
        let rows = sqlx::query_as::<_, (String, String, String, String, i64, Option<String>, Option<String>, i64)>(
            "SELECT id, session_id, role, content, summary, tool_call_id, tool_calls_json, created_at FROM messages WHERE session_id = ? AND content LIKE ? ORDER BY created_at DESC LIMIT ?",
        )
        .bind(session_id)
        .bind(&pattern)
        .bind(limit as i64)
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

pub async fn end_session_with_hooks(hooks: &HookManager, session_id: &str) {
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
        let store = SqliteSessionStore::new(dir.path().join("test.db"))
            .await
            .unwrap();
        store.init().await.unwrap();
        let session = store.create_session("test".to_string()).await.unwrap();
        let fetched = store.get_session(&session.id).await.unwrap().unwrap();
        assert_eq!(session.id, fetched.id);
        assert_eq!(fetched.kind, SessionKind::Main);
    }

    #[tokio::test]
    async fn sqlite_store_creates_child_session() {
        let dir = TempDir::new().unwrap();
        let store = SqliteSessionStore::new(dir.path().join("test.db"))
            .await
            .unwrap();
        store.init().await.unwrap();
        let parent = store.create_session("parent".to_string()).await.unwrap();
        let child = store
            .create_session_with_parent(
                "child".to_string(),
                Some(parent.id.clone()),
                SessionKind::Sub,
            )
            .await
            .unwrap();
        assert_eq!(child.parent_id, Some(parent.id));
        assert_eq!(child.kind, SessionKind::Sub);
    }

    #[tokio::test]
    async fn todo_set_and_get() {
        let dir = TempDir::new().unwrap();
        let store = SqliteSessionStore::new(dir.path().join("test.db"))
            .await
            .unwrap();
        store.init().await.unwrap();
        let session = store.create_session("test".to_string()).await.unwrap();

        let todos = vec![
            TodoItem {
                content: "第一步".to_string(),
                status: TodoStatus::Pending,
                priority: TodoPriority::High,
                active_form: None,
            },
            TodoItem {
                content: "第二步".to_string(),
                status: TodoStatus::InProgress,
                priority: TodoPriority::Medium,
                active_form: Some("正在执行第二步".to_string()),
            },
        ];

        store.set_todos(&session.id, &todos).await.unwrap();
        let read_back = store.get_todos(&session.id).await.unwrap();
        assert_eq!(read_back, todos);

        store.set_todos(&session.id, &[]).await.unwrap();
        let cleared = store.get_todos(&session.id).await.unwrap();
        assert!(cleared.is_empty());
    }

    #[tokio::test]
    async fn rewind_to_before_message_removes_selected_and_after() {
        let dir = TempDir::new().unwrap();
        let store = SqliteSessionStore::new(dir.path().join("test.db"))
            .await
            .unwrap();
        store.init().await.unwrap();
        let session = store.create_session("test".to_string()).await.unwrap();

        let user1 = Message {
            id: Uuid::new_v4().to_string(),
            session_id: session.id.clone(),
            role: MessageRole::User,
            content: "第一条".to_string(),
            summary: false,
            tool_call_id: None,
            tool_calls: None,
            created_at: Utc::now().timestamp(),
        };
        store.append_message(user1.clone()).await.unwrap();

        let assistant1 = Message {
            id: Uuid::new_v4().to_string(),
            session_id: session.id.clone(),
            role: MessageRole::Assistant,
            content: "回复一".to_string(),
            summary: false,
            tool_call_id: None,
            tool_calls: None,
            created_at: Utc::now().timestamp(),
        };
        store.append_message(assistant1.clone()).await.unwrap();

        let user2 = Message {
            id: Uuid::new_v4().to_string(),
            session_id: session.id.clone(),
            role: MessageRole::User,
            content: "第二条".to_string(),
            summary: false,
            tool_call_id: None,
            tool_calls: None,
            created_at: Utc::now().timestamp(),
        };
        store.append_message(user2.clone()).await.unwrap();

        let assistant2 = Message {
            id: Uuid::new_v4().to_string(),
            session_id: session.id.clone(),
            role: MessageRole::Assistant,
            content: "回复二".to_string(),
            summary: false,
            tool_call_id: None,
            tool_calls: None,
            created_at: Utc::now().timestamp(),
        };
        store.append_message(assistant2.clone()).await.unwrap();

        store
            .rewind_to_before_message(&session.id, &user2.id)
            .await
            .unwrap();

        let remaining = store.list_messages(&session.id).await.unwrap();
        assert_eq!(remaining.len(), 2);
        assert_eq!(remaining[0].id, user1.id);
        assert_eq!(remaining[1].id, assistant1.id);
    }
}
