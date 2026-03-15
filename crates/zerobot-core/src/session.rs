use crate::error::{ZeroBotError, ZeroBotResult};
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
    pub created_at: i64,
    pub updated_at: i64,
    pub archived_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub session_id: String,
    pub role: MessageRole,
    pub content: String,
    pub tool_call_id: Option<String>,
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

#[async_trait]
pub trait SessionStore: Send + Sync {
    async fn init(&self) -> ZeroBotResult<()>;
    async fn create_session(&self, title: String) -> ZeroBotResult<Session>;
    async fn get_session(&self, id: &str) -> ZeroBotResult<Option<Session>>;
    async fn list_sessions(&self) -> ZeroBotResult<Vec<Session>>;
    async fn append_message(&self, message: Message) -> ZeroBotResult<()>;
    async fn list_messages(&self, session_id: &str) -> ZeroBotResult<Vec<Message>>;
    async fn record_tool_call(&self, session_id: &str, name: &str, arguments: &str) -> ZeroBotResult<String>;
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
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                archived_at INTEGER
            );
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS messages (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                tool_call_id TEXT,
                created_at INTEGER NOT NULL,
                FOREIGN KEY(session_id) REFERENCES sessions(id)
            );
            "#,
        )
        .execute(&self.pool)
        .await?;

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

    async fn create_session(&self, title: String) -> ZeroBotResult<Session> {
        let now = Utc::now().timestamp();
        let session = Session {
            id: Uuid::new_v4().to_string(),
            title,
            created_at: now,
            updated_at: now,
            archived_at: None,
        };
        sqlx::query(
            "INSERT INTO sessions (id, title, created_at, updated_at, archived_at) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&session.id)
        .bind(&session.title)
        .bind(session.created_at)
        .bind(session.updated_at)
        .bind(session.archived_at)
        .execute(&self.pool)
        .await?;
        Ok(session)
    }

    async fn get_session(&self, id: &str) -> ZeroBotResult<Option<Session>> {
        let row = sqlx::query_as::<_, (String, String, i64, i64, Option<i64>)>(
            "SELECT id, title, created_at, updated_at, archived_at FROM sessions WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|row| Session {
            id: row.0,
            title: row.1,
            created_at: row.2,
            updated_at: row.3,
            archived_at: row.4,
        }))
    }

    async fn list_sessions(&self) -> ZeroBotResult<Vec<Session>> {
        let rows = sqlx::query_as::<_, (String, String, i64, i64, Option<i64>)>(
            "SELECT id, title, created_at, updated_at, archived_at FROM sessions ORDER BY updated_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|row| Session {
                id: row.0,
                title: row.1,
                created_at: row.2,
                updated_at: row.3,
                archived_at: row.4,
            })
            .collect())
    }

    async fn append_message(&self, message: Message) -> ZeroBotResult<()> {
        sqlx::query(
            "INSERT INTO messages (id, session_id, role, content, tool_call_id, created_at) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&message.id)
        .bind(&message.session_id)
        .bind(message.role.to_string())
        .bind(&message.content)
        .bind(&message.tool_call_id)
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
        let rows = sqlx::query_as::<_, (String, String, String, String, Option<String>, i64)>(
            "SELECT id, session_id, role, content, tool_call_id, created_at FROM messages WHERE session_id = ? ORDER BY created_at ASC",
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
                created_at: row.5,
            })
            .collect())
    }

    async fn record_tool_call(&self, session_id: &str, name: &str, arguments: &str) -> ZeroBotResult<String> {
        let id = Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO tool_calls (id, session_id, name, arguments, created_at) VALUES (?, ?, ?, ?, ?)",
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
    fn from_str(raw: &str) -> Self {
        match raw {
            "system" => MessageRole::System,
            "assistant" => MessageRole::Assistant,
            "tool" => MessageRole::Tool,
            _ => MessageRole::User,
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
    }
}
