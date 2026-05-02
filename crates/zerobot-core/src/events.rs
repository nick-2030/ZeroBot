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
    HookStarted {
        event: String,
        hook_name: String,
        status_message: Option<String>,
    },
    HookFinished {
        event: String,
        hook_name: String,
        ok: bool,
        message: Option<String>,
    },
    Done,
    SessionCost {
        input_tokens: u64,
        output_tokens: u64,
        cache_creation_tokens: u64,
        cache_read_tokens: u64,
        turn_count: u32,
    },
    PermissionDenied {
        tool_name: String,
        reason: String,
        permission_reason: Option<String>,
    },
    CwdChanged {
        old_cwd: String,
        new_cwd: String,
    },
    FileChanged {
        paths: Vec<String>,
    },
    HookSessionAdded {
        hook_name: String,
    },
    HookSessionRemoved {
        hook_name: String,
    },
    SelfReviewCompleted {
        summary: String,
        memory_changes: usize,
        skill_changes: usize,
    },
    Stop,
}
