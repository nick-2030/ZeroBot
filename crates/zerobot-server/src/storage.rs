use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use uuid::Uuid;

use zerobot_core::{MemoryItem, Message, Role, Session, SessionId, SessionState, Task, TaskStatus};

pub struct SqliteStore {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteStore {
    pub fn new(path: PathBuf) -> anyhow::Result<Arc<Self>> {
        let conn = Connection::open(path)?;
        Ok(Arc::new(Self {
            conn: Arc::new(Mutex::new(conn)),
        }))
    }

    pub fn init(&self) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                forked_from TEXT
            );
            CREATE TABLE IF NOT EXISTS messages (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS tasks (
                id TEXT PRIMARY KEY,
                session_id TEXT,
                name TEXT NOT NULL,
                cron TEXT,
                status TEXT NOT NULL,
                payload TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS memory (
                id TEXT PRIMARY KEY,
                session_id TEXT,
                kind TEXT NOT NULL,
                content TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS context_summary (
                session_id TEXT PRIMARY KEY,
                summary TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            ",
        )?;
        Ok(())
    }

    pub fn create_session(&self, title: String, forked_from: Option<SessionId>) -> anyhow::Result<Session> {
        let session = Session {
            id: SessionId::new(),
            title,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            forked_from,
        };
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO sessions (id, title, created_at, updated_at, forked_from) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                session.id.0.to_string(),
                session.title,
                session.created_at.to_rfc3339(),
                session.updated_at.to_rfc3339(),
                session.forked_from.map(|s| s.0.to_string()),
            ],
        )?;
        Ok(session)
    }

    pub fn list_sessions(&self) -> anyhow::Result<Vec<Session>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT id, title, created_at, updated_at, forked_from FROM sessions ORDER BY updated_at DESC")?;
        let rows = stmt.query_map([], |row| {
            let forked: Option<String> = row.get(4)?;
            Ok(Session {
                id: SessionId(Uuid::parse_str(&row.get::<_, String>(0)?)?),
                title: row.get(1)?,
                created_at: parse_dt(row.get(2)?),
                updated_at: parse_dt(row.get(3)?),
                forked_from: forked.and_then(|f| Uuid::parse_str(&f).ok()).map(SessionId),
            })
        })?;
        Ok(rows.filter_map(Result::ok).collect())
    }

    pub fn get_session(&self, session_id: &SessionId) -> anyhow::Result<Option<Session>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT id, title, created_at, updated_at, forked_from FROM sessions WHERE id = ?1")?;
        let mut rows = stmt.query(params![session_id.0.to_string()])?;
        if let Some(row) = rows.next()? {
            let forked: Option<String> = row.get(4)?;
            Ok(Some(Session {
                id: SessionId(Uuid::parse_str(&row.get::<_, String>(0)?)?),
                title: row.get(1)?,
                created_at: parse_dt(row.get(2)?),
                updated_at: parse_dt(row.get(3)?),
                forked_from: forked.and_then(|f| Uuid::parse_str(&f).ok()).map(SessionId),
            }))
        } else {
            Ok(None)
        }
    }

    pub fn add_message(&self, session_id: &SessionId, role: Role, content: String) -> anyhow::Result<Message> {
        let msg = Message {
            id: Uuid::new_v4(),
            session_id: session_id.clone(),
            role,
            content,
            created_at: Utc::now(),
        };
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO messages (id, session_id, role, content, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                msg.id.to_string(),
                msg.session_id.0.to_string(),
                role_to_str(&msg.role),
                msg.content,
                msg.created_at.to_rfc3339(),
            ],
        )?;
        conn.execute(
            "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
            params![Utc::now().to_rfc3339(), session_id.0.to_string()],
        )?;
        Ok(msg)
    }

    pub fn list_messages(&self, session_id: &SessionId) -> anyhow::Result<Vec<Message>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, session_id, role, content, created_at FROM messages WHERE session_id = ?1 ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map(params![session_id.0.to_string()], |row| {
            Ok(Message {
                id: Uuid::parse_str(&row.get::<_, String>(0)?)?,
                session_id: SessionId(Uuid::parse_str(&row.get::<_, String>(1)?)?),
                role: role_from_str(row.get::<_, String>(2)?.as_str()),
                content: row.get(3)?,
                created_at: parse_dt(row.get(4)?),
            })
        })?;
        Ok(rows.filter_map(Result::ok).collect())
    }

    pub fn get_state(&self, session_id: &SessionId) -> anyhow::Result<Option<SessionState>> {
        if let Some(session) = self.get_session(session_id)? {
            let messages = self.list_messages(session_id)?;
            Ok(Some(SessionState { session, messages }))
        } else {
            Ok(None)
        }
    }

    pub fn fork_session(&self, session_id: &SessionId, title: String) -> anyhow::Result<Session> {
        let new_session = self.create_session(title, Some(session_id.clone()))?;
        let messages = self.list_messages(session_id)?;
        for msg in messages {
            let _ = self.add_message(&new_session.id, msg.role, msg.content);
        }
        Ok(new_session)
    }

    pub fn rollback_to_message(&self, session_id: &SessionId, message_id: Uuid) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        let created_at: String = conn.query_row(
            "SELECT created_at FROM messages WHERE id = ?1 AND session_id = ?2",
            params![message_id.to_string(), session_id.0.to_string()],
            |row| row.get(0),
        )?;
        conn.execute(
            "DELETE FROM messages WHERE session_id = ?1 AND created_at > ?2",
            params![session_id.0.to_string(), created_at],
        )?;
        Ok(())
    }

    pub fn create_task(&self, task: Task) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO tasks (id, session_id, name, cron, status, payload, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                task.id.to_string(),
                task.session_id.map(|id| id.to_string()),
                task.name,
                task.cron,
                task_status_to_str(&task.status),
                task.payload.to_string(),
                task.created_at.to_rfc3339(),
                task.updated_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn get_task(&self, task_id: Uuid) -> anyhow::Result<Option<Task>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, session_id, name, cron, status, payload, created_at, updated_at FROM tasks WHERE id = ?1",
        )?;
        let mut rows = stmt.query(params![task_id.to_string()])?;
        if let Some(row) = rows.next()? {
            let status: String = row.get(4)?;
            let session_id: Option<String> = row.get(1)?;
            Ok(Some(Task {
                id: Uuid::parse_str(&row.get::<_, String>(0)?)?,
                session_id: session_id.and_then(|id| Uuid::parse_str(&id).ok()),
                name: row.get(2)?,
                cron: row.get(3)?,
                status: task_status_from_str(&status),
                payload: serde_json::from_str(&row.get::<_, String>(5)?)?,
                created_at: parse_dt(row.get(6)?),
                updated_at: parse_dt(row.get(7)?),
            }))
        } else {
            Ok(None)
        }
    }

    pub fn add_memory(&self, memory: MemoryItem) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO memory (id, session_id, kind, content, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                memory.id.to_string(),
                memory.session_id.map(|id| id.to_string()),
                memory.kind,
                memory.content,
                memory.created_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn list_memory(&self, session_id: Option<Uuid>) -> anyhow::Result<Vec<MemoryItem>> {
        let conn = self.conn.lock().unwrap();
        if let Some(id) = session_id {
            let mut stmt = conn.prepare(
                "SELECT id, session_id, kind, content, created_at FROM memory WHERE session_id = ?1 ORDER BY created_at DESC",
            )?;
            let rows = stmt.query_map(params![id.to_string()], |row| {
                let session_id: Option<String> = row.get(1)?;
                Ok(MemoryItem {
                    id: Uuid::parse_str(&row.get::<_, String>(0)?)?,
                    session_id: session_id.and_then(|id| Uuid::parse_str(&id).ok()),
                    kind: row.get(2)?,
                    content: row.get(3)?,
                    created_at: parse_dt(row.get(4)?),
                })
            })?;
            Ok(rows.filter_map(Result::ok).collect())
        } else {
            let mut stmt = conn.prepare(
                "SELECT id, session_id, kind, content, created_at FROM memory ORDER BY created_at DESC",
            )?;
            let rows = stmt.query_map([], |row| {
                let session_id: Option<String> = row.get(1)?;
                Ok(MemoryItem {
                    id: Uuid::parse_str(&row.get::<_, String>(0)?)?,
                    session_id: session_id.and_then(|id| Uuid::parse_str(&id).ok()),
                    kind: row.get(2)?,
                    content: row.get(3)?,
                    created_at: parse_dt(row.get(4)?),
                })
            })?;
            Ok(rows.filter_map(Result::ok).collect())
        }
    }

    pub fn upsert_summary(&self, session_id: &SessionId, summary: &serde_json::Value) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO context_summary (session_id, summary, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(session_id) DO UPDATE SET summary = excluded.summary, updated_at = excluded.updated_at",
            params![session_id.0.to_string(), summary.to_string(), Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn get_summary(&self, session_id: &SessionId) -> anyhow::Result<Option<serde_json::Value>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT summary FROM context_summary WHERE session_id = ?1")?;
        let mut rows = stmt.query(params![session_id.0.to_string()])?;
        if let Some(row) = rows.next()? {
            let summary: String = row.get(0)?;
            Ok(Some(serde_json::from_str(&summary)?))
        } else {
            Ok(None)
        }
    }
}

fn parse_dt(value: String) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(&value)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}

fn role_to_str(role: &Role) -> &str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn role_from_str(value: &str) -> Role {
    match value {
        "system" => Role::System,
        "assistant" => Role::Assistant,
        "tool" => Role::Tool,
        _ => Role::User,
    }
}

fn task_status_to_str(status: &TaskStatus) -> &str {
    match status {
        TaskStatus::Pending => "pending",
        TaskStatus::Running => "running",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "failed",
        TaskStatus::Cancelled => "cancelled",
    }
}

fn task_status_from_str(value: &str) -> TaskStatus {
    match value {
        "running" => TaskStatus::Running,
        "completed" => TaskStatus::Completed,
        "failed" => TaskStatus::Failed,
        "cancelled" => TaskStatus::Cancelled,
        _ => TaskStatus::Pending,
    }
}
