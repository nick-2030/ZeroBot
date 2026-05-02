use crate::message::UsageInfo;

/// The structured result of a single `query()` call.
#[derive(Debug, Clone)]
pub struct QueryResult {
    /// The final text response from the assistant.
    pub response: String,
    /// The session ID this query ran in.
    pub session_id: String,
    /// Number of agentic turns executed.
    pub turns: u32,
    /// Total wall-clock duration in milliseconds.
    pub duration_ms: u64,
    /// Token usage breakdown.
    pub usage: UsageInfo,
    /// Estimated cost in USD (if provider pricing is known).
    pub cost_usd: Option<f64>,
    /// Whether the query completed with an error.
    pub is_error: bool,
    /// Error message (if is_error is true).
    pub error: Option<String>,
}
