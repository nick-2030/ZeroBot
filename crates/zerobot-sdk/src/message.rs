use serde::{Deserialize, Serialize};
use zerobot_core::events::AgentEvent;

/// A message emitted by the SDK during or after a query.
///
/// Each variant carries `session_id` and `uuid` for tracing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SDKMessage {
    /// An assistant text message (full, not streaming delta).
    Assistant(AssistantMessage),
    /// A user message that was recorded.
    User(UserMessage),
    /// The final result of a query.
    Result(ResultMessage),
    /// System-level message (errors, warnings).
    System(SystemMessage),
    /// Streaming text delta (only during streaming mode).
    StreamEvent(StreamEvent),
    /// A tool call was started.
    ToolCallStarted(ToolCallStartedMessage),
    /// A tool call finished.
    ToolCallFinished(ToolCallFinishedMessage),
    /// Permission was denied for a tool call.
    PermissionDenied(PermissionDeniedMessage),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantMessage {
    pub session_id: String,
    pub uuid: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserMessage {
    pub session_id: String,
    pub uuid: String,
    pub content: String,
}

/// The result of a completed query.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "subtype", rename_all = "snake_case")]
pub enum ResultMessage {
    Success(SuccessResult),
    Error(ErrorResult),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuccessResult {
    pub session_id: String,
    pub uuid: String,
    pub result: String,
    pub duration_ms: u64,
    pub total_cost_usd: Option<f64>,
    pub usage: UsageInfo,
    pub turns: u32,
    pub is_error: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResult {
    pub session_id: String,
    pub uuid: String,
    pub error: String,
    pub duration_ms: u64,
    pub turns: u32,
    pub is_error: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageInfo {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
}

impl Default for UsageInfo {
    fn default() -> Self {
        Self {
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemMessage {
    pub session_id: String,
    pub uuid: String,
    pub content: String,
    pub level: SystemMessageLevel,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SystemMessageLevel {
    Info,
    Warning,
    Error,
}

/// Streaming events -- only emitted by `query_stream`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event_type", rename_all = "snake_case")]
pub enum StreamEvent {
    TextDelta {
        session_id: String,
        uuid: String,
        delta: String,
    },
    Usage {
        session_id: String,
        uuid: String,
        usage: UsageInfo,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallStartedMessage {
    pub session_id: String,
    pub uuid: String,
    pub tool_call_id: String,
    pub name: String,
    pub input: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallFinishedMessage {
    pub session_id: String,
    pub uuid: String,
    pub tool_call_id: String,
    pub name: String,
    pub output: String,
    pub ok: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionDeniedMessage {
    pub session_id: String,
    pub uuid: String,
    pub tool_name: String,
    pub reason: String,
}

impl SDKMessage {
    /// Convert from core `AgentEvent` to SDK message.
    /// Returns `None` for internal-only events that SDK consumers should not see.
    pub(crate) fn from_agent_event(event: &AgentEvent, session_id: &str) -> Option<Self> {
        let uuid = uuid::Uuid::new_v4().to_string();
        match event {
            AgentEvent::AssistantMessage { content } => {
                Some(SDKMessage::Assistant(AssistantMessage {
                    session_id: session_id.to_string(),
                    uuid,
                    content: content.clone(),
                }))
            }
            AgentEvent::UserMessage { content } => Some(SDKMessage::User(UserMessage {
                session_id: session_id.to_string(),
                uuid,
                content: content.clone(),
            })),
            AgentEvent::Error { message } => Some(SDKMessage::System(SystemMessage {
                session_id: session_id.to_string(),
                uuid,
                content: message.clone(),
                level: SystemMessageLevel::Error,
            })),
            AgentEvent::AssistantDelta { content } => {
                Some(SDKMessage::StreamEvent(StreamEvent::TextDelta {
                    session_id: session_id.to_string(),
                    uuid,
                    delta: content.clone(),
                }))
            }
            AgentEvent::ToolCallStarted {
                tool_call_id,
                name,
                input,
            } => Some(SDKMessage::ToolCallStarted(ToolCallStartedMessage {
                session_id: session_id.to_string(),
                uuid,
                tool_call_id: tool_call_id.clone(),
                name: name.clone(),
                input: input.clone(),
            })),
            AgentEvent::ToolCallFinished {
                tool_call_id,
                name,
                output,
                ok,
            } => Some(SDKMessage::ToolCallFinished(ToolCallFinishedMessage {
                session_id: session_id.to_string(),
                uuid,
                tool_call_id: tool_call_id.clone(),
                name: name.clone(),
                output: output.clone(),
                ok: *ok,
            })),
            AgentEvent::PermissionDenied {
                tool_name, reason, ..
            } => Some(SDKMessage::PermissionDenied(PermissionDeniedMessage {
                session_id: session_id.to_string(),
                uuid,
                tool_name: tool_name.clone(),
                reason: reason.clone(),
            })),
            AgentEvent::SessionCost {
                input_tokens,
                output_tokens,
                cache_creation_tokens,
                cache_read_tokens,
                ..
            } => Some(SDKMessage::StreamEvent(StreamEvent::Usage {
                session_id: session_id.to_string(),
                uuid,
                usage: UsageInfo {
                    input_tokens: *input_tokens,
                    output_tokens: *output_tokens,
                    cache_creation_tokens: *cache_creation_tokens,
                    cache_read_tokens: *cache_read_tokens,
                },
            })),
            // Internal events -- not exposed to SDK consumers
            AgentEvent::SessionStarted { .. }
            | AgentEvent::SessionResumed { .. }
            | AgentEvent::ToolBatchStarted { .. }
            | AgentEvent::ContextUsage { .. }
            | AgentEvent::Usage { .. }
            | AgentEvent::PluginWarning { .. }
            | AgentEvent::HookStarted { .. }
            | AgentEvent::HookFinished { .. }
            | AgentEvent::Done
            | AgentEvent::CwdChanged { .. }
            | AgentEvent::FileChanged { .. }
            | AgentEvent::HookSessionAdded { .. }
            | AgentEvent::HookSessionRemoved { .. }
            | AgentEvent::SelfReviewCompleted { .. } => None,
        }
    }
}
