use crate::error::{ZeroBotError, ZeroBotResult};
use crate::memory::MemoryManager;
use crate::tool::{Tool, ToolContext, ToolOutput};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value as JsonValue};
use std::sync::Arc;
use tokio::sync::Mutex;

const MEMORY_TOOL_DESCRIPTION: &str = "\
将持久信息保存到跨会话的记忆中。记忆会注入未来的对话轮次，因此保持简洁，聚焦于将来仍然有用的事实。

何时保存（主动保存，不要等用户要求）：
- 用户纠正你或说\"记住这个\"/\"别再这样做\"
- 用户分享偏好、习惯或个人细节
- 你发现关于环境的信息
- 你学到特定于用户设置的约定、API 特性或工作流
- 你识别出在未来会话中有用的稳定事实

优先级：用户偏好和纠正 > 环境事实 > 程序性知识。

不要保存：任务进度、会话结果、完成的工作日志或临时 TODO 状态。
如果发现了做某事的新方法，用 skill_manage 工具保存为 Skill。

两个目标：
- 'user'：用户是谁 — 姓名、角色、偏好、沟通风格、忌讳
- 'memory'：你的笔记 — 环境事实、项目约定、工具特性、经验教训

操作：add（添加）、replace（替换）、remove（删除）。

跳过：琐碎/显而易见的信息、容易重新发现的东西、原始数据转储、临时任务状态。";

pub struct MemoryTool {
    manager: Arc<Mutex<MemoryManager>>,
}

impl MemoryTool {
    pub fn new(manager: Arc<Mutex<MemoryManager>>) -> Self {
        Self { manager }
    }
}

#[derive(Debug, Deserialize)]
struct MemoryToolArgs {
    action: MemoryAction,
    target: MemoryTarget,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    old_text: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MemoryAction {
    Add,
    Replace,
    Remove,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MemoryTarget {
    Memory,
    User,
}

#[async_trait]
impl Tool for MemoryTool {
    fn name(&self) -> &str {
        "memory"
    }

    fn description(&self) -> &str {
        MEMORY_TOOL_DESCRIPTION
    }

    fn is_read_only(&self) -> bool {
        false
    }

    fn is_destructive(&self) -> bool {
        false
    }

    fn parameters(&self) -> JsonValue {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["add", "replace", "remove"],
                    "description": "操作类型"
                },
                "target": {
                    "type": "string",
                    "enum": ["memory", "user"],
                    "description": "'user' = 用户画像，'memory' = Agent 知识"
                },
                "content": {
                    "type": "string",
                    "description": "add/replace 时的新内容"
                },
                "old_text": {
                    "type": "string",
                    "description": "replace/remove 时匹配现有条目的子字符串"
                }
            },
            "required": ["action", "target"]
        })
    }

    async fn run(&self, _ctx: &ToolContext, args: JsonValue) -> ZeroBotResult<ToolOutput> {
        let args: MemoryToolArgs =
            serde_json::from_value(args).map_err(|e| ZeroBotError::Tool(e.to_string()))?;

        let target = match args.target {
            MemoryTarget::Memory => "memory",
            MemoryTarget::User => "user",
        };

        let mut mgr = self.manager.lock().await;

        let response = match args.action {
            MemoryAction::Add => {
                let content = args
                    .content
                    .ok_or_else(|| ZeroBotError::Tool("'add' 操作需要 'content' 参数".to_string()))?;
                mgr.store_mut().add(target, &content)?
            }
            MemoryAction::Replace => {
                let content = args
                    .content
                    .ok_or_else(|| ZeroBotError::Tool("'replace' 操作需要 'content' 参数".to_string()))?;
                let old_text = args
                    .old_text
                    .ok_or_else(|| ZeroBotError::Tool("'replace' 操作需要 'old_text' 参数".to_string()))?;
                mgr.store_mut().replace(target, &old_text, &content)?
            }
            MemoryAction::Remove => {
                let old_text = args
                    .old_text
                    .ok_or_else(|| ZeroBotError::Tool("'remove' 操作需要 'old_text' 参数".to_string()))?;
                mgr.store_mut().remove(target, &old_text)?
            }
        };

        let content = serde_json::to_string_pretty(&response)
            .map_err(|e| ZeroBotError::Tool(e.to_string()))?;
        Ok(ToolOutput::new(content))
    }
}
