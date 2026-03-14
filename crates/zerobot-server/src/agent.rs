use serde_json::Value;

use zerobot_core::{Message, Role, ToolCall, ToolResult, ZeroSettings};

use crate::llm::{self, ChatRequest, LlmMessage};
use crate::tools::ToolRegistry;

pub struct Supervisor {
    tools: std::sync::Arc<ToolRegistry>,
}

impl Supervisor {
    pub fn new(tools: std::sync::Arc<ToolRegistry>) -> Self {
        Self { tools }
    }

    pub async fn handle_user_message_stream<F>(
        &self,
        settings: &ZeroSettings,
        messages: &[Message],
        content: &str,
        mut on_chunk: F,
    ) -> anyhow::Result<Vec<AgentOutput>>
    where
        F: FnMut(&str) + Send,
    {
        if let Some(call) = parse_tool_call(content) {
            let result = self.tools.execute(&call.name, &call.arguments);
            return Ok(vec![
                AgentOutput::Tool(result),
                AgentOutput::Assistant(format!("Tool `{}` executed", call.name)),
            ]);
        }
        let chat_messages = messages
            .iter()
            .filter_map(|msg| match msg.role {
                Role::System => Some(LlmMessage {
                    role: "system".to_string(),
                    content: msg.content.clone(),
                }),
                Role::User => Some(LlmMessage {
                    role: "user".to_string(),
                    content: msg.content.clone(),
                }),
                Role::Assistant => Some(LlmMessage {
                    role: "assistant".to_string(),
                    content: msg.content.clone(),
                }),
                Role::Tool => None,
            })
            .collect::<Vec<_>>();
        let req = ChatRequest {
            provider: None,
            model: None,
            messages: chat_messages,
            temperature: Some(0.2),
            max_tokens: Some(512),
        };
        let output = llm::chat_stream(settings, req, &mut on_chunk).await?;
        Ok(vec![AgentOutput::Assistant(output)])
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
