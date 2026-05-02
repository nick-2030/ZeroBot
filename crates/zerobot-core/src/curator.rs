use crate::config::Settings;
use crate::error::ZeroBotResult;
use crate::provider::{ProviderFactory, ProviderMessage, ProviderMessageRole, ProviderRequest};
use crate::skills::{SkillInfo, SkillManager};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{info, warn};

const CURATOR_PROMPT: &str = r#"你是一个技能整理员。分析以下技能列表，识别需要整理的模式。

执行以下自动转换：
1. 超过 {stale_days} 天未使用的 active 技能 → 标记为 stale
2. 超过 {archive_days} 天未使用的 stale 技能 → 标记为 archived
3. 检查是否有重复或可以合并的技能

输出 JSON（不要包含 markdown 代码块标记）：
{
  "actions": [
    {"action": "set_status", "name": "...", "status": "...", "reason": "..."}
  ],
  "summary": "整理摘要"
}"#;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CuratorResult {
    pub actions: Vec<CuratorAction>,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CuratorAction {
    pub action: String,
    pub name: String,
    pub status: Option<String>,
    pub reason: Option<String>,
}

#[derive(Clone)]
pub struct Curator {
    skill_manager: Arc<SkillManager>,
    provider_factory: ProviderFactory,
    model: String,
    settings: Settings,
    cwd: PathBuf,
    last_run: Arc<std::sync::Mutex<Option<i64>>>,
}

impl Curator {
    pub fn new(
        skill_manager: Arc<SkillManager>,
        provider_factory: ProviderFactory,
        model: String,
        settings: &Settings,
        cwd: PathBuf,
    ) -> Self {
        Self {
            skill_manager,
            provider_factory,
            model,
            settings: settings.clone(),
            cwd,
            last_run: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// Check if enough time has passed since the last run.
    pub fn is_due(&self) -> bool {
        let interval_hours = self.settings.curator.interval_hours;
        if interval_hours == 0 {
            return false;
        }
        let now = chrono::Utc::now().timestamp();
        let last = *self.last_run.lock().unwrap();
        match last {
            Some(last_ts) => {
                let elapsed_hours = (now - last_ts) / 3600;
                elapsed_hours >= interval_hours as i64
            }
            None => true,
        }
    }

    /// Run the curator. Returns the result if any actions were taken.
    pub async fn run(&self) -> ZeroBotResult<Option<CuratorResult>> {
        if !self.is_due() {
            return Ok(None);
        }

        let skills = self.skill_manager.discover()?;
        if skills.is_empty() {
            return Ok(None);
        }

        info!("Curator running with {} skills", skills.len());

        // Auto-transition based on timestamps
        let auto_actions = self.auto_transitions(&skills);
        for action in &auto_actions {
            if let Some(ref status) = action.status {
                if let Err(e) = self.skill_manager.update_skill_status(&action.name, status) {
                    warn!("Curator auto-transition failed for '{}': {e}", action.name);
                }
            }
        }

        // Optionally call LLM for consolidation analysis
        let result = self.consolidate(&skills).await?;

        // Update last run timestamp
        *self.last_run.lock().unwrap() = Some(chrono::Utc::now().timestamp());

        if let Some(ref r) = result {
            info!("Curator completed: {}", r.summary);
        }

        Ok(result)
    }

    fn auto_transitions(&self, _skills: &[SkillInfo]) -> Vec<CuratorAction> {
        // Note: Without actual timestamps in SkillInfo, we return empty here.
        // The LLM-based consolidation handles status transitions.
        // In a full implementation, we'd read each SKILL.md frontmatter
        // and compare last_used_at against stale_after_days / archive_after_days.
        Vec::new()
    }

    async fn consolidate(&self, skills: &[SkillInfo]) -> ZeroBotResult<Option<CuratorResult>> {
        let provider = match (self.provider_factory)() {
            Ok(p) => p,
            Err(e) => {
                warn!("Failed to create provider for curator: {e}");
                return Ok(None);
            }
        };

        let skill_list: Vec<String> = skills
            .iter()
            .map(|s| {
                format!(
                    "- {} ({}): {}",
                    s.name,
                    s.status.as_deref().unwrap_or("active"),
                    s.description
                )
            })
            .collect();

        let stale_days = self.settings.curator.stale_after_days;
        let archive_days = self.settings.curator.archive_after_days;
        let prompt = CURATOR_PROMPT
            .replace("{stale_days}", &stale_days.to_string())
            .replace("{archive_days}", &archive_days.to_string());

        let user_msg = format!("当前技能列表：\n\n{}", skill_list.join("\n"));

        let request = ProviderRequest {
            model: self.model.clone(),
            system: Some(prompt),
            messages: vec![ProviderMessage {
                role: ProviderMessageRole::User,
                content: user_msg,
                tool_call_id: None,
                name: None,
                tool_calls: None,
            }],
            tools: Vec::new(),
            max_tokens: Some(2048),
            temperature: Some(0.3),
            top_p: None,
            top_k: None,
            headers: Default::default(),
            provider_options: Default::default(),
        };

        let response = tokio::time::timeout(
            std::time::Duration::from_secs(120),
            provider.send(request),
        )
        .await
        .map_err(|_| crate::error::ZeroBotError::Tool("Curator LLM 调用超时".to_string()))?
        .map_err(|e| crate::error::ZeroBotError::Tool(format!("Curator LLM 调用失败: {e}")))?;

        let content = response.content.trim();
        let json_str = if let Some(start) = content.find('{') {
            if let Some(end) = content.rfind('}') {
                &content[start..=end]
            } else {
                content
            }
        } else {
            content
        };

        match serde_json::from_str::<CuratorResult>(json_str) {
            Ok(result) => {
                // Execute actions
                for action in &result.actions {
                    if action.action == "set_status" {
                        if let Some(ref status) = action.status {
                            if let Err(e) =
                                self.skill_manager.update_skill_status(&action.name, status)
                            {
                                warn!("Curator action failed for '{}': {e}", action.name);
                            }
                        }
                    }
                }
                Ok(Some(result))
            }
            Err(e) => {
                warn!("Curator response parse failed: {e}");
                Ok(None)
            }
        }
    }
}
