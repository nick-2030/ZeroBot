#[derive(Debug, Clone)]
pub enum AgentEvent {
    SessionStarted {
        session_id: String,
    },
    SessionResumed {
        session_id: String,
    },
    UserMessage {
        content: String,
    },
    AssistantDelta {
        content: String,
    },
    AssistantMessage {
        content: String,
    },
    ToolCallStarted {
        name: String,
        input: String,
    },
    ToolCallFinished {
        name: String,
        output: String,
        ok: bool,
    },
    ContextUsage {
        used: usize,
        limit: Option<u32>,
    },
    Usage {
        usage: crate::provider::TokenUsage,
    },
    Error {
        message: String,
    },
    Done,
}
