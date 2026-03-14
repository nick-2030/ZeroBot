use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AgentKind {
    Supervisor,
    Specialist,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub id: Uuid,
    pub name: String,
    pub kind: AgentKind,
    pub system_prompt: String,
}
