use zerobot_core::{Message, Role, ToolDefinition, ToolResult, ZeroSettings};

use crate::llm::{self, ChatRequest, LlmMessage, ToolCall as LlmToolCall, ToolSpec};
use crate::tools::ToolRegistry;

pub struct Supervisor {
    tools: std::sync::Arc<ToolRegistry>,
}

impl Supervisor {
    pub fn new(tools: std::sync::Arc<ToolRegistry>) -> Self {
        Self { tools }
    }

    pub async fn handle_user_message_with_tools<F>(
        &self,
        settings: &ZeroSettings,
        messages: &[Message],
        tools: &[ToolDefinition],
        mut on_chunk: F,
        mut on_tool_call: impl FnMut(&LlmToolCall) + Send,
    ) -> anyhow::Result<AgentOutcome>
    where
        F: FnMut(&str) + Send,
    {
        let mut llm_messages = build_llm_messages(messages);
        inject_tool_system_prompt(&mut llm_messages, tools);
        let tool_specs = build_tool_specs(tools);
        let mut tool_executions = Vec::new();

        for _ in 0..3 {
            let req = ChatRequest {
                provider: None,
                model: None,
                messages: llm_messages.clone(),
                temperature: Some(0.2),
                max_tokens: Some(512),
                tools: tool_specs.clone(),
                tool_choice: Some("auto".to_string()),
            };
            let resp = llm::chat_stream_with_tools(settings, req, &mut on_chunk).await?;

            if !resp.tool_calls.is_empty() {
                let calls: Vec<LlmToolCall> = resp.tool_calls.into_iter().map(ensure_call_id).collect();
                let assistant_tool_calls = llm::to_openai_tool_calls(&calls);
                llm_messages.push(LlmMessage {
                    role: "assistant".to_string(),
                    content: resp.output.clone(),
                    name: None,
                    tool_call_id: None,
                    tool_calls: Some(assistant_tool_calls),
                });
                for call in calls {
                    on_tool_call(&call);
                    let result = self.tools.execute(&call.name, &call.arguments);
                    llm_messages.push(LlmMessage {
                        role: "tool".to_string(),
                        content: result.output.to_string(),
                        name: Some(call.name.clone()),
                        tool_call_id: Some(call.id.clone()),
                        tool_calls: None,
                    });
                    tool_executions.push(ToolExecution { call, result });
                }
                continue;
            }

            if resp.output.is_empty() {
                return Ok(AgentOutcome {
                    output: String::new(),
                    tool_executions,
                });
            }
            return Ok(AgentOutcome {
                output: resp.output,
                tool_executions,
            });
        }

        Ok(AgentOutcome {
            output: "工具调用轮次已达上限。".to_string(),
            tool_executions,
        })
    }
}

#[derive(Debug)]
pub struct ToolExecution {
    pub call: LlmToolCall,
    pub result: ToolResult,
}

#[derive(Debug)]
pub struct AgentOutcome {
    pub output: String,
    pub tool_executions: Vec<ToolExecution>,
}

fn build_llm_messages(messages: &[Message]) -> Vec<LlmMessage> {
    messages
        .iter()
        .filter_map(|msg| match msg.role {
            Role::System => Some(LlmMessage {
                role: "system".to_string(),
                content: msg.content.clone(),
                name: None,
                tool_call_id: None,
                tool_calls: None,
            }),
            Role::User => Some(LlmMessage {
                role: "user".to_string(),
                content: msg.content.clone(),
                name: None,
                tool_call_id: None,
                tool_calls: None,
            }),
            Role::Assistant => Some(LlmMessage {
                role: "assistant".to_string(),
                content: msg.content.clone(),
                name: None,
                tool_call_id: None,
                tool_calls: None,
            }),
            Role::Tool => None,
        })
        .collect()
}

fn build_tool_specs(tools: &[ToolDefinition]) -> Vec<ToolSpec> {
    tools
        .iter()
        .map(|tool| ToolSpec {
            name: tool.name.clone(),
            description: tool.description.clone(),
            input_schema: tool.input_schema.clone(),
        })
        .collect()
}

fn inject_tool_system_prompt(messages: &mut Vec<LlmMessage>, tools: &[ToolDefinition]) {
    if tools.is_empty() {
        return;
    }
    let has_system = messages.iter().any(|m| m.role == "system");
    if has_system {
        return;
    }
    let mut descs = Vec::new();
    for tool in tools {
        descs.push(format!("{}: {}", tool.name, tool.description));
    }
    let content = format!(
        "你可以使用以下工具来完成任务，需要文件/命令/检索时请调用工具。\n可用工具：{}。",
        descs.join("，")
    );
    messages.insert(
        0,
        LlmMessage {
            role: "system".to_string(),
            content,
            name: None,
            tool_call_id: None,
            tool_calls: None,
        },
    );
}

fn ensure_call_id(mut call: LlmToolCall) -> LlmToolCall {
    if call.id.trim().is_empty() {
        call.id = uuid::Uuid::new_v4().to_string();
    }
    call
}
