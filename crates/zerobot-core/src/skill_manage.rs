use crate::error::{ZeroBotError, ZeroBotResult};
use crate::skills::SkillManager;
use crate::tool::{Tool, ToolContext, ToolOutput};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value as JsonValue};
use std::path::PathBuf;
use std::sync::Arc;

const SKILL_MANAGE_DESCRIPTION: &str = "\
创建、修改、删除或管理 Skill 的生命周期状态。\
Skill 是 Agent 的程序性记忆：基于经验证明的、完成特定类型任务的方法。

何时创建/更新 Skill：
- 用户纠正你的方法（风格、语气、工作流、方法）
- 你发现新技术、修复、变通方案
- 你发现某个 Skill 过时或错误
- 某个任务模式在多个会话中重复出现

优先顺序：
1. 更新当前加载的 Skill
2. 更新现有的伞形 Skill
3. 在现有伞形下添加支持文件
4. 创建新的类级伞形 Skill

创建 Skill 时，目标是类级指令和经验知识的库，而不是数百个狭窄的 Skill。";

pub struct SkillManageTool {
    manager: Arc<SkillManager>,
    cwd: PathBuf,
}

impl SkillManageTool {
    pub fn new(manager: Arc<SkillManager>, cwd: PathBuf) -> Self {
        Self { manager, cwd }
    }
}

#[derive(Debug, Deserialize)]
struct SkillManageArgs {
    action: SkillAction,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SkillAction {
    Create,
    Patch,
    Write,
    Delete,
    List,
    SetStatus,
}

#[async_trait]
impl Tool for SkillManageTool {
    fn name(&self) -> &str {
        "skill_manage"
    }

    fn description(&self) -> &str {
        SKILL_MANAGE_DESCRIPTION
    }

    fn is_read_only(&self) -> bool {
        false
    }

    fn is_destructive(&self) -> bool {
        true
    }

    fn parameters(&self) -> JsonValue {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["create", "patch", "write", "delete", "list", "set_status"],
                    "description": "操作类型"
                },
                "name": {
                    "type": "string",
                    "description": "Skill 名称（小写-连字符格式）"
                },
                "description": {
                    "type": "string",
                    "description": "Skill 描述"
                },
                "content": {
                    "type": "string",
                    "description": "create/write 时的 Skill 内容（markdown 正文）"
                },
                "status": {
                    "type": "string",
                    "enum": ["active", "stale", "archived", "pinned"],
                    "description": "set_status 时的目标状态"
                }
            },
            "required": ["action"]
        })
    }

    async fn run(&self, _ctx: &ToolContext, args: JsonValue) -> ZeroBotResult<ToolOutput> {
        let args: SkillManageArgs =
            serde_json::from_value(args).map_err(|e| ZeroBotError::Tool(e.to_string()))?;

        match args.action {
            SkillAction::Create => {
                let name = args
                    .name
                    .ok_or_else(|| ZeroBotError::Tool("'create' 需要 'name' 参数".to_string()))?;
                let description = args.description.ok_or_else(|| {
                    ZeroBotError::Tool("'create' 需要 'description' 参数".to_string())
                })?;
                let body = args.content.unwrap_or_default();
                let path = self
                    .manager
                    .write_skill(&name, &description, &body, args.status.as_deref())?;
                let response = json!({
                    "success": true,
                    "action": "create",
                    "name": name,
                    "path": path.display().to_string(),
                    "message": format!("Skill '{}' 已创建", name)
                });
                Ok(ToolOutput::new(
                    serde_json::to_string_pretty(&response).unwrap(),
                ))
            }
            SkillAction::Write => {
                let name = args
                    .name
                    .ok_or_else(|| ZeroBotError::Tool("'write' 需要 'name' 参数".to_string()))?;
                let description = args.description.ok_or_else(|| {
                    ZeroBotError::Tool("'write' 需要 'description' 参数".to_string())
                })?;
                let body = args
                    .content
                    .ok_or_else(|| ZeroBotError::Tool("'write' 需要 'content' 参数".to_string()))?;
                let path = self
                    .manager
                    .write_skill(&name, &description, &body, args.status.as_deref())?;
                let response = json!({
                    "success": true,
                    "action": "write",
                    "name": name,
                    "path": path.display().to_string(),
                    "message": format!("Skill '{}' 已写入", name)
                });
                Ok(ToolOutput::new(
                    serde_json::to_string_pretty(&response).unwrap(),
                ))
            }
            SkillAction::Patch => {
                return Err(ZeroBotError::Tool(
                    "'patch' 操作暂未实现，请使用 'write' 完全覆盖".to_string(),
                ));
            }
            SkillAction::Delete => {
                let name = args
                    .name
                    .ok_or_else(|| ZeroBotError::Tool("'delete' 需要 'name' 参数".to_string()))?;
                self.manager.delete_skill(&name)?;
                let response = json!({
                    "success": true,
                    "action": "delete",
                    "name": name,
                    "message": format!("Skill '{}' 已删除", name)
                });
                Ok(ToolOutput::new(
                    serde_json::to_string_pretty(&response).unwrap(),
                ))
            }
            SkillAction::List => {
                let skills = self.manager.discover()?;
                let list: Vec<JsonValue> = skills
                    .iter()
                    .map(|s| {
                        json!({
                            "name": s.name,
                            "description": s.description,
                            "path": s.path.display().to_string(),
                            "status": s.status,
                        })
                    })
                    .collect();
                let response = json!({
                    "success": true,
                    "action": "list",
                    "count": list.len(),
                    "skills": list
                });
                Ok(ToolOutput::new(
                    serde_json::to_string_pretty(&response).unwrap(),
                ))
            }
            SkillAction::SetStatus => {
                let name = args.name.ok_or_else(|| {
                    ZeroBotError::Tool("'set_status' 需要 'name' 参数".to_string())
                })?;
                let status = args.status.ok_or_else(|| {
                    ZeroBotError::Tool("'set_status' 需要 'status' 参数".to_string())
                })?;
                let path = self.manager.update_skill_status(&name, &status)?;
                let response = json!({
                    "success": true,
                    "action": "set_status",
                    "name": name,
                    "status": status,
                    "path": path.display().to_string(),
                    "message": format!("Skill '{}' 状态已更新为 '{}'", name, status)
                });
                Ok(ToolOutput::new(
                    serde_json::to_string_pretty(&response).unwrap(),
                ))
            }
        }
    }
}
