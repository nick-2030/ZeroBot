use serde_json::Value;
use uuid::Uuid;

use zerobot_core::{Message, Role, SessionId, ToolCall, ToolResult};

use crate::tools::ToolRegistry;

pub struct Supervisor {
    tools: std::sync::Arc<ToolRegistry>,
}

impl Supervisor {
    pub fn new(tools: std::sync::Arc<ToolRegistry>) -> Self {
        Self { tools }
    }

    pub fn handle_user_message(&self, session_id: &SessionId, content: &str) -> Vec<AgentOutput> {
        let _ = session_id;
        if let Some(call) = parse_tool_call(content) {
            let result = self.tools.execute(&call.name, &call.arguments);
            return vec![
                AgentOutput::Tool(result),
                AgentOutput::Assistant(format!("Tool `{}` executed", call.name)),
            ];
        }
        vec![AgentOutput::Assistant(format!("Stub response: {}", content))]
    }
}

#[derive(Debug)]
pub enum AgentOutput {
    Assistant(String),
    Tool(ToolResult),
}

fn parse_tool_call(content: &str) -> Option<ToolCall> {
    let trimmed = content.trim();
    if !trimmed.starts_with("/tool") {
        return None;
    }
    let mut parts = trimmed.splitn(3, ' ');
    let _ = parts.next();
    let name = parts.next()?.to_string();
    let args = parts.next().unwrap_or("{}");
    let json: Value = serde_json::from_str(args).ok()?;
    Some(ToolCall { name, arguments: json })
}

pub fn tool_message(session_id: &SessionId, result: &ToolResult) -> Message {
    Message {
        id: Uuid::new_v4(),
        session_id: session_id.clone(),
        role: Role::Tool,
        content: result.output.to_string(),
        created_at: chrono::Utc::now(),
    }
}
