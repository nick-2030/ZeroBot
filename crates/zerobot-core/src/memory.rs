use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryItem {
    pub id: Uuid,
    pub session_id: Option<Uuid>,
    pub kind: String,
    pub content: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryQuery {
    pub session_id: Option<Uuid>,
    pub kind: Option<String>,
    pub text: Option<String>,
    pub limit: usize,
}
