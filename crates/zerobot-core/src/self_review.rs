use crate::config::Settings;
use crate::error::{ZeroBotError, ZeroBotResult};
use crate::memory::MemoryManager;
use crate::provider::{
    ProviderFactory, ProviderMessage, ProviderMessageRole, ProviderRequest,
};
use crate::session::{Message, MessageRole, SessionStore};
use crate::skills::SkillManager;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, warn};

const COMBINED_REVIEW_PROMPT: &str = r#"你是一个自我改进审查员。分析以下对话，提取需要长期记忆的信息和可以提炼为技能的模式。

## 记忆审查
提取以下类别的条目：
1. 用户偏好和习惯（写入 USER.md）
2. 技术知识和最佳实践（写入 MEMORY.md）
3. 用户纠正和反馈
4. 过时或错误的记忆，标记为需要删除

## 技能审查
识别可以提炼为 Skill 的模式：
1. 重复出现的任务流程
2. 新发现的技术方法
3. 用户偏好导致的特定工作方式
4. 需要更新或归档的现有 Skill

以 JSON 格式输出（不要包含 markdown 代码块标记）：
{
  "memory_entries": [{"content": "...", "source": "observation"}],
  "user_entries": [{"content": "...", "source": "user_correction"}],
  "skill_actions": [
    {"action": "create", "name": "...", "description": "...", "content": "..."},
    {"action": "set_status", "name": "...", "status": "stale"}
  ],
  "summary": "本次审查摘要"
}"#;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewResult {
    pub memory_entries: Vec<ReviewEntry>,
    pub user_entries: Vec<ReviewEntry>,
    pub skill_actions: Vec<JsonValue>,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewEntry {
    pub content: String,
    pub source: String,
}

#[derive(Clone)]
pub struct SelfReviewer {
    provider_factory: ProviderFactory,
    model: String,
    review_model: Option<String>,
    settings: Settings,
    store: Arc<dyn SessionStore>,
    memory_manager: Arc<Mutex<MemoryManager>>,
    skill_manager: Arc<SkillManager>,
    #[allow(dead_code)]
    cwd: PathBuf,
    turn_counter: Arc<AtomicU32>,
}

impl SelfReviewer {
    pub fn new(
        provider_factory: ProviderFactory,
        model: String,
        settings: &Settings,
        store: Arc<dyn SessionStore>,
        memory_manager: Arc<Mutex<MemoryManager>>,
        skill_manager: Arc<SkillManager>,
        cwd: PathBuf,
    ) -> Self {
        Self {
            provider_factory,
            model,
            review_model: settings.self_review.model.clone(),
            settings: settings.clone(),
            store,
            memory_manager,
            skill_manager,
            cwd,
            turn_counter: Arc::new(AtomicU32::new(0)),
        }
    }

    /// Increment turn counter and run review if interval is reached.
    /// Returns Some(summary) if a review was performed.
    pub async fn maybe_review(&self, session_id: &str) -> ZeroBotResult<Option<String>> {
        let count = self.turn_counter.fetch_add(1, Ordering::SeqCst) + 1;
        let interval = self.settings.self_review.interval;
        if interval == 0 || !count.is_multiple_of(interval) {
            return Ok(None);
        }

        info!("Self-review triggered at turn {count} (interval={interval})");

        let messages = self.store.list_messages(session_id).await?;
        if messages.is_empty() {
            return Ok(None);
        }

        // Build conversation text for review
        let conversation = messages_to_text(&messages);

        let model = self
            .review_model
            .as_deref()
            .unwrap_or(&self.model);

        let result = self.call_review_llm(model, &conversation).await?;
        let summary = result.summary.clone();

        // Persist results
        self.persist_review(&result).await?;

        info!("Self-review completed: {}", summary);
        Ok(Some(summary))
    }

    async fn call_review_llm(
        &self,
        model: &str,
        conversation: &str,
    ) -> ZeroBotResult<ReviewResult> {
        let provider = (self.provider_factory)()?;
        let user_msg = format!(
            "以下是最近的对话内容，请审查并提取值得记忆的信息：\n\n{}",
            conversation
        );

        let request = ProviderRequest {
            model: model.to_string(),
            system: Some(COMBINED_REVIEW_PROMPT.to_string()),
            messages: vec![ProviderMessage {
                role: ProviderMessageRole::User,
                content: user_msg,
                tool_call_id: None,
                name: None,
                tool_calls: None,
            }],
            tools: Vec::new(),
            max_tokens: Some(self.settings.self_review.max_tokens),
            temperature: Some(0.3),
            top_p: None,
            top_k: None,
            headers: Default::default(),
            provider_options: Default::default(),
        };

        let response = tokio::time::timeout(
            std::time::Duration::from_secs(self.settings.self_review.timeout_secs),
            provider.send(request),
        )
        .await
        .map_err(|_| ZeroBotError::Tool("Self-review LLM 调用超时".to_string()))?
        .map_err(|e| ZeroBotError::Tool(format!("Self-review LLM 调用失败: {e}")))?;

        let content = response.content.trim();
        // Try to extract JSON from response (handle markdown code blocks)
        let json_str = if let Some(start) = content.find('{') {
            if let Some(end) = content.rfind('}') {
                &content[start..=end]
            } else {
                content
            }
        } else {
            content
        };

        serde_json::from_str::<ReviewResult>(json_str)
            .map_err(|e| ZeroBotError::Tool(format!("Self-review 响应解析失败: {e}")))
    }

    async fn persist_review(&self, result: &ReviewResult) -> ZeroBotResult<()> {
        let mut mgr = self.memory_manager.lock().await;

        // Persist memory entries
        for entry in &result.memory_entries {
            if let Err(e) = mgr.store_mut().add("memory", &entry.content) {
                warn!("Failed to add memory entry: {e}");
            }
        }

        // Persist user entries
        for entry in &result.user_entries {
            if let Err(e) = mgr.store_mut().add("user", &entry.content) {
                warn!("Failed to add user entry: {e}");
            }
        }

        // Persist skill actions
        for action in &result.skill_actions {
            if let Some(action_type) = action.get("action").and_then(|v| v.as_str()) {
                match action_type {
                    "create" => {
                        let name = action.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        let desc = action.get("description").and_then(|v| v.as_str()).unwrap_or("");
                        let content = action.get("content").and_then(|v| v.as_str()).unwrap_or("");
                        if let Err(e) = self.skill_manager.write_skill(name, desc, content, None) {
                            warn!("Failed to create skill '{}': {e}", name);
                        }
                    }
                    "set_status" => {
                        let name = action.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        let status = action.get("status").and_then(|v| v.as_str()).unwrap_or("");
                        if let Err(e) =
                            self.skill_manager.update_skill_status(name, status)
                        {
                            warn!("Failed to update skill status '{}': {e}", name);
                        }
                    }
                    _ => {
                        warn!("Unknown skill action: {}", action_type);
                    }
                }
            }
        }

        Ok(())
    }
}

fn messages_to_text(messages: &[Message]) -> String {
    let mut out = String::new();
    for msg in messages {
        if msg.summary {
            continue; // Skip summary messages
        }
        let role = match msg.role {
            MessageRole::User => "User",
            MessageRole::Assistant => "Assistant",
            MessageRole::System => "System",
            MessageRole::Tool => "Tool",
        };
        out.push_str(&format!("{}: {}\n\n", role, msg.content));
    }
    out
}
