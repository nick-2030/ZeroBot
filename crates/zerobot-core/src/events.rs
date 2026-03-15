#[derive(Debug, Clone)]
pub enum AgentEvent {
    SessionStarted { session_id: String },
    SessionResumed { session_id: String },
    UserMessage { content: String },
    AssistantMessage { content: String },
    ToolCallStarted { name: String },
    ToolCallFinished { name: String, output: String },
    Error { message: String },
    Done,
}
