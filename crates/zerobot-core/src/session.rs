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
            "INSERT INTO messages (id, session_id, role, content, tool_call_id, tool_calls_json, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&message.id)
        .bind(&message.session_id)
        .bind(message.role.to_string())
        .bind(&message.content)
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
        let rows = sqlx::query_as::<_, (String, String, String, String, Option<String>, Option<String>, i64)>(
            "SELECT id, session_id, role, content, tool_call_id, tool_calls_json, created_at FROM messages WHERE session_id = ? ORDER BY created_at ASC",
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
                tool_call_id: row.4,
                tool_calls: row.5.and_then(|raw| serde_json::from_str(&raw).ok()),
                created_at: row.6,
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
}
