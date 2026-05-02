use serde::{Deserialize, Serialize};

/// SDK-level session info with a stable public surface.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub title: String,
    pub parent_id: Option<String>,
    pub kind: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub summary: Option<String>,
}

impl From<zerobot_core::session::Session> for SessionInfo {
    fn from(s: zerobot_core::session::Session) -> Self {
        Self {
            id: s.id,
            title: s.title,
            parent_id: s.parent_id,
            kind: s.kind.to_string(),
            created_at: s.created_at,
            updated_at: s.updated_at,
            summary: s.summary,
        }
    }
}
