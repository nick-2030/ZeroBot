use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextItem {
    pub kind: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ContextSummary {
    pub session_intent: String,
    pub files_modified: Vec<String>,
    pub decisions: Vec<String>,
    pub current_state: String,
    pub next_steps: Vec<String>,
}

impl ContextSummary {
    pub fn merge(&mut self, other: ContextSummary) {
        if !other.session_intent.is_empty() {
            self.session_intent = other.session_intent;
        }
        self.files_modified.extend(other.files_modified);
        self.decisions.extend(other.decisions);
        if !other.current_state.is_empty() {
            self.current_state = other.current_state;
        }
        self.next_steps.extend(other.next_steps);
    }
}

pub fn summarize_messages(messages: &[String]) -> ContextSummary {
    // Anchored iterative summarization placeholder.
    // In v1 we simply capture the last user goal and list any file-like tokens.
    let mut summary = ContextSummary::default();
    if let Some(last) = messages.iter().rev().find(|m| !m.trim().is_empty()) {
        summary.session_intent = last.trim().to_string();
    }
    for msg in messages {
        for token in msg.split_whitespace() {
            if token.contains('/') && token.contains('.') {
                summary.files_modified.push(token.trim_matches(|c: char| c == ',' || c == '.').to_string());
            }
        }
    }
    summary
}
