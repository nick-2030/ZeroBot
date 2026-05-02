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
        tool_call_id: String,
        name: String,
        input: String,
    },
    ToolCallFinished {
        tool_call_id: String,
        name: String,
        output: String,
        ok: bool,
    },
    ToolBatchStarted {
        tool_call_ids: Vec<String>,
        parallel: bool,
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
    PluginWarning {
        plugin: String,
        hook: String,
        message: String,
        degraded: bool,
    },
    Done,
    SessionCost {
        input_tokens: u64,
        output_tokens: u64,
        cache_creation_tokens: u64,
        cache_read_tokens: u64,
        turn_count: u32,
    },
}
