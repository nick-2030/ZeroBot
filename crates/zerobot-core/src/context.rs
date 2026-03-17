use crate::config::Settings;
use crate::instruction;
use crate::provider::{ProviderMessage, ProviderMessageRole};
use crate::session::{Message, MessageRole, StoredToolCall};
use crate::agents::{format_agent_index, AgentManager};
use crate::skills::{format_skill_index, SkillInfo};
use chrono::Local;
use std::path::{Path, PathBuf};

pub struct ContextBuild {
    pub system: Option<String>,
    pub messages: Vec<ProviderMessage>,
    pub dropped_messages: usize,
    pub dropped_chars: usize,
    pub estimated_tokens: usize,
    pub context_limit: Option<u32>,
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
        self.build_with_skills(model, history, None, None)
    }

    pub fn build_with_skills(
        &self,
        model: &str,
        history: &[Message],
        skills: Option<&[SkillInfo]>,
        extra_instructions: Option<&[String]>,
    ) -> ContextBuild {
        let max_messages = if self.settings.context.max_messages == 0 {
            self.settings.session.max_history
        } else {
            self.settings.context.max_messages
        };
        let max_chars = self.settings.context.max_chars;

        let mut dropped_messages = 0usize;
        let mut dropped_chars = 0usize;

        let mut candidates: Vec<Message> = history.to_vec();
        let summary_anchor = history
            .iter()
            .rposition(|msg| msg.summary && matches!(msg.role, MessageRole::Assistant));
        if let Some(idx) = summary_anchor {
            dropped_messages += idx;
            dropped_chars += history[..idx]
                .iter()
                .map(|msg| msg.content.chars().count())
                .sum::<usize>();
            candidates = history[idx..].to_vec();
        }

        if max_messages > 0 && candidates.len() > max_messages {
            if summary_anchor.is_some() {
                let keep_tail = max_messages.saturating_sub(1);
                let tail_start = candidates.len().saturating_sub(keep_tail);
                let mut next = Vec::new();
                let summary_msg = candidates.first().cloned();
                if let Some(summary_msg) = summary_msg {
                    next.push(summary_msg);
                }
                if tail_start > 1 {
                    dropped_messages += tail_start - 1;
                    dropped_chars += candidates[1..tail_start]
                        .iter()
                        .map(|msg| msg.content.chars().count())
                        .sum::<usize>();
                }
                next.extend_from_slice(&candidates[tail_start..]);
                candidates = next;
            } else {
                let start = candidates.len() - max_messages;
                dropped_messages += start;
                dropped_chars += candidates[..start]
                    .iter()
                    .map(|msg| msg.content.chars().count())
                    .sum::<usize>();
                candidates = candidates[start..].to_vec();
            }
        }

        if max_chars > 0 {
            if summary_anchor.is_some() && !candidates.is_empty() && candidates[0].summary {
                let summary_msg = candidates[0].clone();
                let mut kept_rev: Vec<Message> = Vec::new();
                let mut total_chars = summary_msg.content.chars().count();
                for msg in candidates.iter().skip(1).rev() {
                    let msg_chars = msg.content.chars().count();
                    if total_chars + msg_chars > max_chars {
                        dropped_messages += 1;
                        dropped_chars += msg_chars;
                        continue;
                    }
                    kept_rev.push(msg.clone());
                    total_chars += msg_chars;
                }
                kept_rev.reverse();
                let mut kept = Vec::new();
                kept.push(summary_msg);
                kept.extend(kept_rev);
                candidates = kept;
            } else {
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
        }

        let mut extra_system = Vec::new();
        let mut normal_messages = Vec::new();
        for message in candidates {
            if matches!(message.role, MessageRole::System) {
                if !message.content.trim().is_empty() {
                    extra_system.push(message.content);
                }
            } else {
                normal_messages.push(message);
            }
        }

        let messages = normal_messages
            .into_iter()
            .map(message_to_provider)
            .collect::<Vec<_>>();

        let mut system = self.build_system_prompt(model, dropped_messages, skills);
        let sources = instruction::system_sources(&self.settings, &self.cwd);
        let file_instructions = instruction::load_file_instructions(&sources.files);
        let mut instruction_parts: Vec<String> = file_instructions
            .into_iter()
            .map(|item| item.content)
            .collect();
        if let Some(extra) = extra_instructions {
            instruction_parts.extend(extra.iter().cloned());
        }
        if !instruction_parts.is_empty() {
            let extra = instruction_parts.join("\n\n");
            system = Some(match system {
                Some(base) if !base.trim().is_empty() => format!("{base}\n\n{extra}"),
                _ => extra,
            });
        }
        if !extra_system.is_empty() {
            let extra = extra_system.join("\n\n");
            system = Some(match system {
                Some(base) if !base.trim().is_empty() => format!("{base}\n\n{extra}"),
                _ => extra,
            });
        }

        let estimated_tokens = estimate_tokens(
            system.as_deref().unwrap_or_default().chars().count()
                + messages
                    .iter()
                    .map(|msg| message_chars(msg))
                    .sum::<usize>(),
        );
        let context_limit = resolve_context_limit(&self.settings, model);

        ContextBuild {
            system,
            messages,
            dropped_messages,
            dropped_chars,
            estimated_tokens,
            context_limit,
        }
    }

    fn build_system_prompt(
        &self,
        model: &str,
        dropped_messages: usize,
        skills: Option<&[SkillInfo]>,
    ) -> Option<String> {
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

        if self.settings.skills.enabled {
            if let Some(list) = skills {
                parts.push(format_skill_index(list));
            }
        }

        if self
            .settings
            .tools
            .enabled
            .iter()
            .any(|t| t == "subagent")
        {
            let manager = AgentManager::new(&self.cwd);
            if let Ok(list) = manager.discover() {
                if !list.is_empty() {
                    parts.push(format_agent_index(&list));
                }
            }
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

fn resolve_context_limit(settings: &Settings, model: &str) -> Option<u32> {
    if let Some(limit) = settings.context.model_limits.get(model) {
        return Some(*limit);
    }
    settings.context.max_tokens
}

fn estimate_tokens(chars: usize) -> usize {
    (chars + 3) / 4
}

fn message_chars(message: &ProviderMessage) -> usize {
    let mut total = message.content.chars().count();
    if let Some(calls) = &message.tool_calls {
        if let Ok(raw) = serde_json::to_string(calls) {
            total += raw.chars().count();
        }
    }
    if let Some(name) = &message.name {
        total += name.chars().count();
    }
    if let Some(call_id) = &message.tool_call_id {
        total += call_id.chars().count();
    }
    total
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
            summary: false,
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

    #[test]
    fn keeps_summary_anchor() {
        let mut settings = Settings::default();
        settings.context.max_messages = 1;
        settings.context.max_chars = 0;
        let manager = ContextManager::new(&settings, PathBuf::from("."));
        let mut summary = msg(MessageRole::Assistant, "summary");
        summary.summary = true;
        let history = vec![
            msg(MessageRole::User, "a"),
            summary,
            msg(MessageRole::User, "b"),
            msg(MessageRole::Assistant, "c"),
        ];
        let result = manager.build("test-model", &history);
        assert_eq!(result.messages.first().unwrap().content, "summary");
        assert!(result.messages.len() >= 1);
    }

    #[test]
    fn summary_survives_char_trim() {
        let mut settings = Settings::default();
        settings.context.max_messages = 0;
        settings.context.max_chars = 2;
        let manager = ContextManager::new(&settings, PathBuf::from("."));
        let mut summary = msg(MessageRole::Assistant, "summary");
        summary.summary = true;
        let history = vec![
            msg(MessageRole::User, "a"),
            summary,
            msg(MessageRole::User, "bbbb"),
        ];
        let result = manager.build("test-model", &history);
        assert_eq!(result.messages.first().unwrap().content, "summary");
    }
}
