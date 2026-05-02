use std::sync::Arc;
use async_trait::async_trait;
use crate::error::{ZeroBotError, ZeroBotResult};
use crate::tool::{Tool, ToolContext, ToolOutput};
use super::{SwarmManager, TeammateConfig};

/// 生成 teammate
pub struct SpawnTeammateTool {
    manager: Arc<SwarmManager>,
}

impl SpawnTeammateTool {
    pub fn new(manager: Arc<SwarmManager>) -> Self { Self { manager } }
}

#[async_trait]
impl Tool for SpawnTeammateTool {
    fn name(&self) -> &str { "spawn_teammate" }
    fn description(&self) -> &str { "生成一个新的 teammate agent" }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "agent_name": { "type": "string", "description": "teammate 名称" },
                "team_name": { "type": "string", "description": "团队名称" },
                "agent_type": { "type": "string", "description": "agent 定义名称" },
                "prompt": { "type": "string", "description": "初始任务描述" }
            },
            "required": ["agent_name", "team_name", "agent_type", "prompt"]
        })
    }
    async fn run(&self, _ctx: &ToolContext, args: serde_json::Value) -> ZeroBotResult<ToolOutput> {
        let config = TeammateConfig {
            agent_name: args["agent_name"].as_str().ok_or_else(|| ZeroBotError::Tool("缺少 agent_name".into()))?.to_string(),
            team_name: args["team_name"].as_str().ok_or_else(|| ZeroBotError::Tool("缺少 team_name".into()))?.to_string(),
            agent_type: args["agent_type"].as_str().ok_or_else(|| ZeroBotError::Tool("缺少 agent_type".into()))?.to_string(),
            prompt: args["prompt"].as_str().ok_or_else(|| ZeroBotError::Tool("缺少 prompt".into()))?.to_string(),
            model: args["model"].as_str().map(|s| s.to_string()),
            cwd: args["cwd"].as_str().map(std::path::PathBuf::from),
        };
        let handle = self.manager.spawn_teammate(config).await?;
        Ok(ToolOutput::new(format!("Teammate {}@{} 已生成 (task: {})", handle.agent_name, handle.team_name, handle.task_id)))
    }
}

/// 向 teammate 发送消息
pub struct SendTeammateMessageTool {
    manager: Arc<SwarmManager>,
}

impl SendTeammateMessageTool {
    pub fn new(manager: Arc<SwarmManager>) -> Self { Self { manager } }
}

#[async_trait]
impl Tool for SendTeammateMessageTool {
    fn name(&self) -> &str { "send_teammate_message" }
    fn description(&self) -> &str { "向 teammate 发送消息" }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "agent_name": { "type": "string", "description": "teammate 名称" },
                "team_name": { "type": "string", "description": "团队名称" },
                "message": { "type": "string", "description": "消息内容" }
            },
            "required": ["agent_name", "team_name", "message"]
        })
    }
    async fn run(&self, _ctx: &ToolContext, args: serde_json::Value) -> ZeroBotResult<ToolOutput> {
        let agent_name = args["agent_name"].as_str().ok_or_else(|| ZeroBotError::Tool("缺少 agent_name".into()))?;
        let team_name = args["team_name"].as_str().ok_or_else(|| ZeroBotError::Tool("缺少 team_name".into()))?;
        let message = args["message"].as_str().ok_or_else(|| ZeroBotError::Tool("缺少 message".into()))?;

        // 查找 handle
        let active = self.manager.list_active().await;
        let handle = active.iter().find(|h| h.agent_name == agent_name && h.team_name == team_name)
            .ok_or_else(|| ZeroBotError::Swarm(format!("Teammate {}@{} 未找到", agent_name, team_name)))?;

        self.manager.send_message(handle, message.to_string()).await?;
        Ok(ToolOutput::new("消息已发送".to_string()))
    }
}

/// 列出活跃的 teammate
pub struct ListTeammatesTool {
    manager: Arc<SwarmManager>,
}

impl ListTeammatesTool {
    pub fn new(manager: Arc<SwarmManager>) -> Self { Self { manager } }
}

#[async_trait]
impl Tool for ListTeammatesTool {
    fn name(&self) -> &str { "list_teammates" }
    fn description(&self) -> &str { "列出所有活跃的 teammate" }
    fn is_read_only(&self) -> bool { true }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {} })
    }
    async fn run(&self, _ctx: &ToolContext, _args: serde_json::Value) -> ZeroBotResult<ToolOutput> {
        let teammates = self.manager.list_active().await;
        if teammates.is_empty() {
            return Ok(ToolOutput::new("没有活跃的 teammate".to_string()));
        }
        let list: Vec<String> = teammates.iter()
            .map(|h| format!("- {}@{} (task: {}, backend: {:?})", h.agent_name, h.team_name, h.task_id, h.backend_type))
            .collect();
        Ok(ToolOutput::new(format!("活跃的 teammate:\n{}", list.join("\n"))))
    }
}
