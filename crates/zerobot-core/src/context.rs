use crate::config::Settings;
use crate::provider::{ProviderMessage, ProviderMessageRole};
use crate::session::{Message, MessageRole, StoredToolCall};
use chrono::Local;
use std::path::{Path, PathBuf};

pub struct ContextBuild {
    pub system: Option<String>,
    pub messages: Vec<ProviderMessage>,
    pub dropped_messages: usize,
    pub dropped_chars: usize,
}

pub struct ContextManager {
    settings: Settings,
    cwd: PathBuf,
}

impl ContextManager {
    pub fn new(settings: &Settings, cwd: PathBuf) -> Self {
        Self {
            settings: settings.clone(),
            cwd,
        }
    }

    pub fn build(&self, model: &str, history: &[Message]) -> ContextBuild {
        let max_messages = if self.settings.context.max_messages == 0 {
            self.settings.session.max_history
        } else {
            self.settings.context.max_messages
        };
        let max_chars = self.settings.context.max_chars;

        let mut dropped_messages = 0usize;
        let mut dropped_chars = 0usize;

        let mut candidates: Vec<Message> = if max_messages > 0 && history.len() > max_messages {
            let start = history.len() - max_messages;
            dropped_messages += start;
            dropped_chars += history[..start]
                .iter()
                .map(|msg| msg.content.chars().count())
                .sum::<usize>();
            history[start..].to_vec()
        } else {
            history.to_vec()
        };

        if max_chars > 0 {
            let mut kept: Vec<Message> = Vec::new();
            let mut total_chars = 0usize;
            for msg in candidates.iter().rev() {
                let msg_chars = msg.content.chars().count();
                if !kept.is_empty() && total_chars + msg_chars > max_chars {
                    dropped_messages += 1;
                    dropped_chars += msg_chars;
                    continue;
                }
                kept.push(msg.clone());
                total_chars += msg_chars;
            }
            kept.reverse();
            candidates = kept;
        }

        let messages = candidates
            .into_iter()
            .map(message_to_provider)
            .collect::<Vec<_>>();

        let system = self.build_system_prompt(model, dropped_messages);

        ContextBuild {
            system,
            messages,
            dropped_messages,
            dropped_chars,
        }
    }

    fn build_system_prompt(&self, model: &str, dropped_messages: usize) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(prompt) = self.settings.agent.system_prompt.as_deref() {
            let trimmed = prompt.trim();
            if !trimmed.is_empty() {
                parts.push(trimmed.to_string());
            }
        }

        if self.settings.context.include_environment {
            parts.push(build_environment_block(model, &self.cwd));
        }

        if dropped_messages > 0 {
            parts.push(format!(
                "注意：对话过长，已裁剪 {dropped_messages} 条历史消息，仅保留最近内容。若需要更早信息请提醒我。"
            ));
        }

        if parts.is_empty() {
            None
        } else {
            Some(parts.join("\n\n"))
        }
    }
}

fn message_to_provider(message: Message) -> ProviderMessage {
    let role = match message.role {
        MessageRole::System => ProviderMessageRole::System,
        MessageRole::User => ProviderMessageRole::User,
        MessageRole::Assistant => ProviderMessageRole::Assistant,
        MessageRole::Tool => ProviderMessageRole::Tool,
    };
    ProviderMessage {
        role,
        content: message.content,
        tool_call_id: message.tool_call_id,
        name: None,
        tool_calls: message
            .tool_calls
            .as_ref()
            .map(|calls| calls.iter().map(StoredToolCall::to_provider_call).collect()),
    }
}

fn build_environment_block(model: &str, cwd: &Path) -> String {
    let workspace = find_workspace_root(cwd).unwrap_or_else(|| cwd.to_path_buf());
    let git_repo = workspace.join(".git").exists();
    let date = Local::now().format("%Y-%m-%d").to_string();
    let platform = std::env::consts::OS;

    [
        "以下是运行环境信息：",
        "<env>",
        &format!("  模型: {model}"),
        &format!("  工作目录: {}", cwd.display()),
        &format!("  工作区根目录: {}", workspace.display()),
        &format!("  是否为 Git 仓库: {}", if git_repo { "是" } else { "否" }),
        &format!("  平台: {platform}"),
        &format!("  日期: {date}"),
        "</env>",
    ]
    .join("\n")
}

fn find_workspace_root(start: &Path) -> Option<PathBuf> {
    let mut current = start.to_path_buf();
    loop {
        if current.join(".git").exists() {
            return Some(current);
        }
        let parent = current.parent()?.to_path_buf();
        if parent == current {
            return None;
        }
        current = parent;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Settings;
    use crate::session::MessageRole;

    fn msg(role: MessageRole, content: &str) -> Message {
        Message {
            id: "m1".to_string(),
            session_id: "s1".to_string(),
            role,
            content: content.to_string(),
            tool_call_id: None,
            tool_calls: None,
            created_at: 0,
        }
    }

    #[test]
    fn trims_by_max_messages() {
        let mut settings = Settings::default();
        settings.context.max_messages = 2;
        let manager = ContextManager::new(&settings, PathBuf::from("."));
        let history = vec![
            msg(MessageRole::User, "a"),
            msg(MessageRole::Assistant, "b"),
            msg(MessageRole::User, "c"),
        ];
        let result = manager.build("test-model", &history);
        assert_eq!(result.messages.len(), 2);
        assert_eq!(result.dropped_messages, 1);
    }

    #[test]
    fn trims_by_max_chars() {
        let mut settings = Settings::default();
        settings.context.max_messages = 0;
        settings.context.max_chars = 3;
        let manager = ContextManager::new(&settings, PathBuf::from("."));
        let history = vec![
            msg(MessageRole::User, "abc"),
            msg(MessageRole::Assistant, "def"),
        ];
        let result = manager.build("test-model", &history);
        assert_eq!(result.messages.len(), 1);
        assert!(result.dropped_messages >= 1);
    }
}
