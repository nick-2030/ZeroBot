use zerobot_core::{summarize_messages, ContextSummary};

pub fn compress_messages(messages: &[String]) -> ContextSummary {
    summarize_messages(messages)
}
