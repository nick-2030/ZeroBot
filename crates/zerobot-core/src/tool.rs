use crate::agent::Agent;
use crate::agents::AgentManager;
use crate::config::{Settings, ToolOutputDirection, ToolOutputSettings};
use crate::error::{ZeroBotError, ZeroBotResult};
use crate::hooks::HookManager;
use crate::mcp::{format_tool_output, McpManager, McpToolInfo};
use crate::provider::ProviderFactory;
use crate::session::{SessionKind, SessionStore, TodoItem};
use crate::skills::{SkillAction, SkillManager, SkillContent, SkillStackEntry};
use async_trait::async_trait;
use diffy::Patch;
use regex::Regex;
use serde::Deserialize;
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::process::Command;
use walkdir::WalkDir;

#[derive(Clone)]
pub struct ToolContext {
    pub cwd: PathBuf,
    pub allow_paths: Vec<PathBuf>,
    pub session_id: String,
    pub store: Option<Arc<dyn SessionStore>>,
}

impl ToolContext {
    pub fn new(
        cwd: PathBuf,
        allow_paths: Vec<PathBuf>,
        session_id: impl Into<String>,
        store: Option<Arc<dyn SessionStore>>,
    ) -> Self {
        Self {
            cwd,
            allow_paths,
            session_id: session_id.into(),
            store,
        }
    }

    pub fn resolve_path(&self, input: &str) -> ZeroBotResult<PathBuf> {
        let path = PathBuf::from(input);
        let full = if path.is_absolute() {
            path
        } else {
            self.cwd.join(path)
        };
        let full = full
            .canonicalize()
            .unwrap_or_else(|_| full.clone());

        if self.allow_paths.is_empty() {
            return Ok(full);
        }

        for allowed in &self.allow_paths {
            if full.starts_with(allowed) {
                return Ok(full);
            }
        }

        Err(ZeroBotError::Tool("路径不在允许范围内".to_string()))
    }

    pub fn store(&self) -> Option<Arc<dyn SessionStore>> {
        self.store.clone()
    }
}

#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub content: String,
    pub truncated: bool,
}

impl ToolOutput {
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            truncated: false,
        }
    }

    pub fn truncated(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            truncated: true,
        }
    }
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> JsonValue;
    async fn run(&self, ctx: &ToolContext, args: JsonValue) -> ZeroBotResult<ToolOutput>;
}

#[derive(Default, Clone)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self { tools: HashMap::new() }
    }

    pub fn register<T: Tool + 'static>(&mut self, tool: T) {
        self.tools.insert(tool.name().to_string(), Arc::new(tool));
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    pub fn names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }

    pub fn specs(&self, enabled: &[String]) -> Vec<crate::provider::ToolSpec> {
        let mut specs = Vec::new();
        for name in enabled {
            if let Some(tool) = self.tools.get(name) {
                specs.push(crate::provider::ToolSpec {
                    name: tool.name().to_string(),
                    description: tool.description().to_string(),
                    parameters: tool.parameters(),
                });
            }
        }
        specs
    }

    pub async fn run(
        &self,
        ctx: &ToolContext,
        name: &str,
        args: JsonValue,
    ) -> ZeroBotResult<ToolOutput> {
        self.run_with_settings(ctx, name, args, &ToolOutputSettings::default())
            .await
    }

    pub async fn run_with_settings(
        &self,
        ctx: &ToolContext,
        name: &str,
        args: JsonValue,
        output_settings: &ToolOutputSettings,
    ) -> ZeroBotResult<ToolOutput> {
        let tool = self
            .get(name)
            .ok_or_else(|| ZeroBotError::Tool(format!("未知工具: {name}")))?;
        let output = tool.run(ctx, args).await?;
        if output.truncated {
            return Ok(output);
        }
        truncate_tool_output(&output.content, output_settings).await
    }

    pub fn with_builtin() -> Self {
        let mut registry = Self::new();
        registry.register(ReadTool);
        registry.register(WriteTool);
        registry.register(EditTool);
        registry.register(PatchTool);
        registry.register(GlobTool);
        registry.register(GrepTool);
        registry.register(ShellTool);
        registry.register(TodoReadTool);
        registry.register(TodoWriteTool);
        registry
    }

    pub async fn with_builtin_async(
        settings: &crate::config::Settings,
        cwd: &std::path::Path,
        store: Option<Arc<dyn SessionStore>>,
    ) -> ZeroBotResult<Self> {
        let mut registry = Self::with_builtin();

        if settings.skills.enabled {
            let store = store.ok_or_else(|| ZeroBotError::Tool("Skill 需要 SessionStore".to_string()))?;
            let manager = Arc::new(SkillManager::new(settings, cwd));
            registry.register(SkillTool { manager, store });
        }

        if let Some(mcp) = McpManager::new(settings, cwd).await? {
            let mcp = Arc::new(mcp);
            for tool in mcp.tools() {
                let name = format!("mcp__{}__{}", tool.server, tool.name);
                registry.tools.insert(
                    name.clone(),
                    Arc::new(McpToolAdapter {
                        manager: Arc::clone(&mcp),
                        full_name: name,
                        tool,
                    }),
                );
            }
        }

        Ok(registry)
    }
}

pub struct SubagentTool {
    settings: Settings,
    store: Arc<dyn SessionStore>,
    tools: ToolRegistry,
    cwd: PathBuf,
    provider_factory: ProviderFactory,
    fallback_model: String,
    parent_hooks: HookManager,
}

impl SubagentTool {
    pub fn new(
        settings: Settings,
        store: Arc<dyn SessionStore>,
        tools: ToolRegistry,
        cwd: PathBuf,
        provider_factory: ProviderFactory,
        fallback_model: String,
        parent_hooks: HookManager,
    ) -> Self {
        Self {
            settings,
            store,
            tools,
            cwd,
            provider_factory,
            fallback_model,
            parent_hooks,
        }
    }
}

#[derive(Deserialize)]
struct SubagentArgs {
    name: String,
    prompt: String,
}

#[async_trait]
impl Tool for SubagentTool {
    fn name(&self) -> &str {
        "subagent"
    }

    fn description(&self) -> &str {
        "调用指定子代理处理任务"
    }

    fn parameters(&self) -> JsonValue {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {"type": "string", "description": "子代理名称"},
                "prompt": {"type": "string", "description": "子代理输入内容"}
            },
            "required": ["name", "prompt"]
        })
    }

    async fn run(&self, ctx: &ToolContext, args: JsonValue) -> ZeroBotResult<ToolOutput> {
        let args: SubagentArgs = serde_json::from_value(args)
            .map_err(|err| ZeroBotError::Tool(err.to_string()))?;

        let manager = AgentManager::new(&self.cwd);
        let def = manager.load(&args.name)?;

        let name = def.name.clone();
        let description = def.description.clone();
        let model = def
            .model
            .clone()
            .unwrap_or_else(|| self.fallback_model.clone());

        let mut settings = self.settings.clone();
        let mut system_prompt = String::new();
        system_prompt.push_str(&format!("子代理描述：{}", def.description));
        if !def.body.trim().is_empty() {
            if !system_prompt.trim().is_empty() {
                system_prompt.push_str("\n\n");
            }
            system_prompt.push_str(def.body.trim());
        }
        if !system_prompt.trim().is_empty() {
            settings.agent.system_prompt = Some(system_prompt);
        }
        if let Some(tools) = def.tools.clone() {
            settings.tools.enabled = tools;
        }

        let hooks = HookManager::load(&settings, &self.cwd, Some(def.hooks.clone()))?;
        let session = crate::session::create_session_with_hooks(
            self.store.as_ref(),
            &hooks,
            format!("子代理:{}", name),
            Some(ctx.session_id.clone()),
            SessionKind::Sub,
        )
        .await?;

        let parent_skill_hooks = self
            .store
            .get_skill_stack(&ctx.session_id)
            .await?
            .into_iter()
            .flat_map(|entry| entry.hooks)
            .collect::<Vec<_>>();
        let start_decision = self
            .parent_hooks
            .apply_event(
                crate::hooks::HookEvent::SubagentStart,
                &ctx.session_id,
                serde_json::json!({
                    "subagent_name": name,
                    "subagent_description": description,
                    "subagent_session_id": session.id,
                    "prompt": args.prompt.clone(),
                }),
                &parent_skill_hooks,
            )
            .await?;
        if matches!(start_decision.action, crate::hooks::HookAction::Deny) {
            let message = start_decision
                .message
                .unwrap_or_else(|| "子代理调用被 Hook 拒绝".to_string());
            crate::session::end_session_with_hooks(&hooks, &session.id).await;
            return Err(ZeroBotError::Tool(message));
        }

        let provider = (self.provider_factory)()?;
        let agent = Agent::new(
            provider,
            model,
            settings,
            self.store.clone(),
            self.tools.clone(),
            self.cwd.clone(),
            hooks.clone(),
        );

        let result = agent.run_turn(&session.id, &args.prompt, None).await;
        crate::session::end_session_with_hooks(&hooks, &session.id).await;
        let output = result?;
        let output_for_hook = output.clone();

        let _ = self
            .parent_hooks
            .apply_event(
                crate::hooks::HookEvent::SubagentStop,
                &ctx.session_id,
                serde_json::json!({
                    "subagent_name": name,
                    "subagent_session_id": session.id,
                    "output": output_for_hook,
                }),
                &parent_skill_hooks,
            )
            .await;
        Ok(ToolOutput::new(output))
    }
}

struct SkillTool {
    manager: Arc<SkillManager>,
    store: Arc<dyn SessionStore>,
}

#[derive(Deserialize)]
struct SkillArgs {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    action: Option<SkillAction>,
}

#[async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &str {
        "skill"
    }

    fn description(&self) -> &str {
        "加载指定 Skill 的内容"
    }

    fn parameters(&self) -> JsonValue {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {"type": "string", "description": "Skill 名称"},
                "action": {"type": "string", "enum": ["start", "end"], "description": "start 或 end"}
            },
            "required": []
        })
    }

    async fn run(&self, _ctx: &ToolContext, args: JsonValue) -> ZeroBotResult<ToolOutput> {
        let args: SkillArgs = serde_json::from_value(args)
            .map_err(|err| ZeroBotError::Tool(err.to_string()))?;
        let action = args.action.unwrap_or(SkillAction::Start);
        match action {
            SkillAction::Start => {
                let name = args
                    .name
                    .ok_or_else(|| ZeroBotError::Tool("skill 缺少 name".to_string()))?;
                let SkillContent { info, body } = self.manager.load(&name)?;
                self.store
                    .push_skill_stack(
                        &_ctx.session_id,
                        SkillStackEntry {
                            name: info.name.clone(),
                            description: info.description.clone(),
                            path: info.path.clone(),
                            hooks: info.hooks.clone(),
                            started_at: chrono::Utc::now().timestamp(),
                        },
                    )
                    .await?;
                let output = format!(
                    "<skill>\n<name>{}</name>\n<path>{}</path>\n{}\n</skill>",
                    info.name,
                    info.path.display(),
                    body
                );
                Ok(ToolOutput::new(output))
            }
            SkillAction::End => {
                let current = self.store.get_skill_stack(&_ctx.session_id).await?;
                if current.is_empty() {
                    return Ok(ToolOutput::new("skill 栈为空，无需结束"));
                }
                let top = current.last().cloned();
                if let Some(name) = args.name {
                    if let Some(top) = top.as_ref() {
                        if top.name != name {
                            return Err(ZeroBotError::Tool(format!(
                                "skill 栈顶为 {}，与 end 名称不一致",
                                top.name
                            )));
                        }
                    }
                }
                let popped = self.store.pop_skill_stack(&_ctx.session_id).await?;
                let name = popped.map(|s| s.name).unwrap_or_else(|| "未知".to_string());
                Ok(ToolOutput::new(format!("skill 已结束: {name}")))
            }
        }
    }
}

struct McpToolAdapter {
    manager: Arc<McpManager>,
    full_name: String,
    tool: McpToolInfo,
}

#[async_trait]
impl Tool for McpToolAdapter {
    fn name(&self) -> &str {
        &self.full_name
    }

    fn description(&self) -> &str {
        &self.tool.description
    }

    fn parameters(&self) -> JsonValue {
        self.tool.parameters.clone()
    }

    async fn run(&self, _ctx: &ToolContext, args: JsonValue) -> ZeroBotResult<ToolOutput> {
        let result = self
            .manager
            .call_tool(&self.tool.server, &self.tool.name, args)
            .await?;
        Ok(ToolOutput::new(format_tool_output(result)))
    }
}

async fn truncate_tool_output(
    content: &str,
    settings: &ToolOutputSettings,
) -> ZeroBotResult<ToolOutput> {
    let max_lines = settings.max_lines;
    let max_bytes = settings.max_bytes;
    let direction = settings.direction;

    let lines: Vec<&str> = content.split('\n').collect();
    let total_bytes = content.as_bytes().len();

    if lines.len() <= max_lines && total_bytes <= max_bytes {
        return Ok(ToolOutput::new(content));
    }

    let mut out: Vec<&str> = Vec::new();
    let mut bytes = 0usize;
    let mut hit_bytes = false;

    if direction == ToolOutputDirection::Head {
        for line in lines.iter().take(max_lines) {
            let size = line.as_bytes().len() + if out.is_empty() { 0 } else { 1 };
            if bytes + size > max_bytes {
                hit_bytes = true;
                break;
            }
            out.push(*line);
            bytes += size;
        }
    } else {
        for line in lines.iter().rev().take(max_lines) {
            let size = line.as_bytes().len() + if out.is_empty() { 0 } else { 1 };
            if bytes + size > max_bytes {
                hit_bytes = true;
                break;
            }
            out.push(*line);
            bytes += size;
        }
        out.reverse();
    }

    let removed = if hit_bytes {
        total_bytes.saturating_sub(bytes)
    } else {
        lines.len().saturating_sub(out.len())
    };
    let unit = if hit_bytes { "字节" } else { "行" };
    let preview = out.join("\n");

    let summary = format!("...已截断 {removed} {unit}...");
    let hint = "输出过长已截断，建议使用 grep 搜索，或使用 read 搭配 offset/limit 查看指定范围。"
        .to_string();
    let message = if direction == ToolOutputDirection::Head {
        format!("{preview}\n\n{summary}\n\n{hint}")
    } else {
        format!("{summary}\n\n{hint}\n\n{preview}")
    };

    Ok(ToolOutput::truncated(message))
}

struct ReadTool;

#[derive(Deserialize)]
struct ReadArgs {
    path: String,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }

    fn description(&self) -> &str {
        "读取文件内容"
    }

    fn parameters(&self) -> JsonValue {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "文件路径"},
                "offset": {"type": "integer", "description": "起始行号（从 0 开始）"},
                "limit": {"type": "integer", "description": "返回的最大行数"}
            },
            "required": ["path"]
        })
    }

    async fn run(&self, ctx: &ToolContext, args: JsonValue) -> ZeroBotResult<ToolOutput> {
        let args: ReadArgs = serde_json::from_value(args)
            .map_err(|err| ZeroBotError::Tool(err.to_string()))?;
        let path = ctx.resolve_path(&args.path)?;
        let content = tokio::fs::read_to_string(path).await?;
        let offset = args.offset.unwrap_or(0);
        let limit = args.limit.unwrap_or(usize::MAX);
        if offset == 0 && limit == usize::MAX {
            return Ok(ToolOutput::new(content));
        }
        let lines: Vec<&str> = content.lines().collect();
        if offset >= lines.len() {
            return Ok(ToolOutput::new(""));
        }
        let end = offset.saturating_add(limit).min(lines.len());
        Ok(ToolOutput::new(lines[offset..end].join("\n")))
    }
}

struct WriteTool;

#[derive(Deserialize)]
struct WriteArgs {
    path: String,
    content: String,
    #[serde(default)]
    append: bool,
}

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &str {
        "write"
    }

    fn description(&self) -> &str {
        "写入文件"
    }

    fn parameters(&self) -> JsonValue {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "文件路径"},
                "content": {"type": "string", "description": "写入内容"},
                "append": {"type": "boolean", "description": "是否追加"}
            },
            "required": ["path", "content"]
        })
    }

    async fn run(&self, ctx: &ToolContext, args: JsonValue) -> ZeroBotResult<ToolOutput> {
        let args: WriteArgs = serde_json::from_value(args)
            .map_err(|err| ZeroBotError::Tool(err.to_string()))?;
        let path = ctx.resolve_path(&args.path)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        if args.append {
            use tokio::io::AsyncWriteExt;
            let mut file = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .await?;
            file.write_all(args.content.as_bytes()).await?;
        } else {
            tokio::fs::write(path, args.content).await?;
        }
        Ok(ToolOutput::new("写入完成"))
    }
}

struct EditTool;

#[derive(Deserialize)]
struct EditArgs {
    path: String,
    find: String,
    replace: String,
    #[serde(default)]
    replace_all: bool,
}

#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }

    fn description(&self) -> &str {
        "替换文件内容"
    }

    fn parameters(&self) -> JsonValue {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "find": {"type": "string"},
                "replace": {"type": "string"},
                "replace_all": {"type": "boolean"}
            },
            "required": ["path", "find", "replace"]
        })
    }

    async fn run(&self, ctx: &ToolContext, args: JsonValue) -> ZeroBotResult<ToolOutput> {
        let args: EditArgs = serde_json::from_value(args)
            .map_err(|err| ZeroBotError::Tool(err.to_string()))?;
        let path = ctx.resolve_path(&args.path)?;
        let mut content = tokio::fs::read_to_string(&path).await?;
        let count = if args.replace_all {
            let replaced = content.replace(&args.find, &args.replace);
            let count = content.matches(&args.find).count();
            content = replaced;
            count
        } else if let Some(pos) = content.find(&args.find) {
            content.replace_range(pos..pos + args.find.len(), &args.replace);
            1
        } else {
            0
        };
        tokio::fs::write(&path, content).await?;
        Ok(ToolOutput::new(format!("已替换 {count} 处")))
    }
}

struct PatchTool;

#[derive(Deserialize)]
struct PatchArgs {
    path: String,
    patch: String,
}

#[async_trait]
impl Tool for PatchTool {
    fn name(&self) -> &str {
        "patch"
    }

    fn description(&self) -> &str {
        "应用补丁"
    }

    fn parameters(&self) -> JsonValue {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "patch": {"type": "string"}
            },
            "required": ["path", "patch"]
        })
    }

    async fn run(&self, ctx: &ToolContext, args: JsonValue) -> ZeroBotResult<ToolOutput> {
        let args: PatchArgs = serde_json::from_value(args)
            .map_err(|err| ZeroBotError::Tool(err.to_string()))?;
        let path = ctx.resolve_path(&args.path)?;
        let content = tokio::fs::read_to_string(&path).await?;
        let patch = Patch::from_str(&args.patch)
            .map_err(|err| ZeroBotError::Tool(err.to_string()))?;
        let updated = diffy::apply(&content, &patch)
            .map_err(|err| ZeroBotError::Tool(err.to_string()))?;
        tokio::fs::write(&path, updated).await?;
        Ok(ToolOutput::new("补丁已应用"))
    }
}

struct GlobTool;

#[derive(Deserialize)]
struct GlobArgs {
    pattern: String,
}

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }

    fn description(&self) -> &str {
        "查找匹配文件"
    }

    fn parameters(&self) -> JsonValue {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string"}
            },
            "required": ["pattern"]
        })
    }

    async fn run(&self, ctx: &ToolContext, args: JsonValue) -> ZeroBotResult<ToolOutput> {
        let args: GlobArgs = serde_json::from_value(args)
            .map_err(|err| ZeroBotError::Tool(err.to_string()))?;
        let pattern = ctx.cwd.join(&args.pattern);
        let mut results = Vec::new();
        for entry in glob::glob(pattern.to_string_lossy().as_ref())
            .map_err(|err| ZeroBotError::Tool(err.to_string()))?
        {
            if let Ok(path) = entry {
                results.push(path.to_string_lossy().to_string());
            }
        }
        Ok(ToolOutput::new(results.join("\n")))
    }
}

struct GrepTool;

#[derive(Deserialize)]
struct GrepArgs {
    pattern: String,
    path: String,
}

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn description(&self) -> &str {
        "搜索文件内容"
    }

    fn parameters(&self) -> JsonValue {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string"},
                "path": {"type": "string"}
            },
            "required": ["pattern", "path"]
        })
    }

    async fn run(&self, ctx: &ToolContext, args: JsonValue) -> ZeroBotResult<ToolOutput> {
        let args: GrepArgs = serde_json::from_value(args)
            .map_err(|err| ZeroBotError::Tool(err.to_string()))?;
        let root = ctx.resolve_path(&args.path)?;
        let regex = Regex::new(&args.pattern)
            .map_err(|err| ZeroBotError::Tool(err.to_string()))?;

        let mut matches = Vec::new();
        for entry in WalkDir::new(root).into_iter().filter_map(Result::ok) {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            let content = std::fs::read_to_string(path);
            if let Ok(content) = content {
                for (idx, line) in content.lines().enumerate() {
                    if regex.is_match(line) {
                        matches.push(format!("{}:{}:{}", path.display(), idx + 1, line));
                    }
                }
            }
        }

        Ok(ToolOutput::new(matches.join("\n")))
    }
}

struct ShellTool;

#[derive(Deserialize)]
struct ShellArgs {
    command: String,
    #[serde(default)]
    dir: Option<String>,
}

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &str {
        "shell"
    }

    fn description(&self) -> &str {
        "执行 shell 命令"
    }

    fn parameters(&self) -> JsonValue {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {"type": "string"},
                "dir": {"type": "string"}
            },
            "required": ["command"]
        })
    }

    async fn run(&self, ctx: &ToolContext, args: JsonValue) -> ZeroBotResult<ToolOutput> {
        let args: ShellArgs = serde_json::from_value(args)
            .map_err(|err| ZeroBotError::Tool(err.to_string()))?;
        let dir = if let Some(dir) = args.dir {
            ctx.resolve_path(&dir)?
        } else {
            ctx.cwd.clone()
        };
        let output = Command::new("/bin/sh")
            .arg("-lc")
            .arg(args.command)
            .current_dir(dir)
            .output()
            .await?;
        let mut text = String::new();
        text.push_str("[stdout]\n");
        text.push_str(&String::from_utf8_lossy(&output.stdout));
        text.push_str("\n[stderr]\n");
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        text.push_str(&format!("\n[exit_code]\n{}", output.status.code().unwrap_or(-1)));
        Ok(ToolOutput::new(text))
    }
}

struct TodoReadTool;

#[async_trait]
impl Tool for TodoReadTool {
    fn name(&self) -> &str {
        "todoread"
    }

    fn description(&self) -> &str {
        "读取当前会话的待办列表，返回 JSON 数组"
    }

    fn parameters(&self) -> JsonValue {
        serde_json::json!({
            "type": "object",
            "properties": {},
            "required": []
        })
    }

    async fn run(&self, ctx: &ToolContext, _args: JsonValue) -> ZeroBotResult<ToolOutput> {
        let store = ctx
            .store()
            .ok_or_else(|| ZeroBotError::Tool("Todo 工具需要 SessionStore".to_string()))?;
        let todos = store.get_todos(&ctx.session_id).await?;
        let content = serde_json::to_string_pretty(&todos)
            .map_err(|err| ZeroBotError::Tool(err.to_string()))?;
        Ok(ToolOutput::new(content))
    }
}

struct TodoWriteTool;

#[derive(Deserialize)]
struct TodoWriteArgs {
    todos: Vec<TodoItem>,
}

#[async_trait]
impl Tool for TodoWriteTool {
    fn name(&self) -> &str {
        "todowrite"
    }

    fn description(&self) -> &str {
        "创建或更新当前会话的待办列表（适合多步骤任务）。请提供完整列表并保持有且只有一项 in_progress，其余为 pending/completed/cancelled。"
    }

    fn parameters(&self) -> JsonValue {
        serde_json::json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "description": "更新后的待办列表（完整覆盖）",
                    "items": {
                        "type": "object",
                        "properties": {
                            "content": { "type": "string", "description": "待办内容" },
                            "status": { "type": "string", "enum": ["pending", "in_progress", "completed", "cancelled"] },
                            "priority": { "type": "string", "enum": ["high", "medium", "low"] }
                        },
                        "required": ["content", "status", "priority"]
                    }
                }
            },
            "required": ["todos"]
        })
    }

    async fn run(&self, ctx: &ToolContext, args: JsonValue) -> ZeroBotResult<ToolOutput> {
        let args: TodoWriteArgs = serde_json::from_value(args)
            .map_err(|err| ZeroBotError::Tool(err.to_string()))?;
        let store = ctx
            .store()
            .ok_or_else(|| ZeroBotError::Tool("Todo 工具需要 SessionStore".to_string()))?;
        store.set_todos(&ctx.session_id, &args.todos).await?;
        let content = serde_json::to_string_pretty(&args.todos)
            .map_err(|err| ZeroBotError::Tool(err.to_string()))?;
        Ok(ToolOutput::new(content))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;
    use httpmock::Method::POST;
    use httpmock::MockServer;
    use std::collections::HashMap;
    use std::sync::Arc;
    use crate::provider::{Provider, ProviderFactory, ProviderRequest, ProviderResponse};
    use crate::session::{SessionKind, SqliteSessionStore};
    use async_trait::async_trait;

    #[tokio::test]
    async fn registry_runs_builtin_tool() {
        let dir = TempDir::new().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf(), vec![], "s1", None);
        let registry = ToolRegistry::with_builtin();
        let args = serde_json::json!({"path": "test.txt", "content": "hi"});
        let output = registry.run(&ctx, "write", args).await.unwrap();
        assert_eq!(output.content, "写入完成");
    }

    #[tokio::test]
    async fn truncates_tool_output_without_persisting() {
        let settings = ToolOutputSettings {
            max_lines: 2,
            max_bytes: 16,
            direction: ToolOutputDirection::Head,
        };
        let content = "line1\nline2\nline3\nline4";
        let output = truncate_tool_output(content, &settings).await.unwrap();
        assert!(output.truncated);
        assert!(output.content.contains("已截断"));
    }

    #[tokio::test]
    async fn registry_registers_mcp_tool() {
        let server = MockServer::start();
        let _init_mock = server.mock(|when, then| {
            when.method(POST).body_contains("initialize");
            then.json_body(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {"ok": true}
            }));
        });
        let _list_mock = server.mock(|when, then| {
            when.method(POST).body_contains("tools/list");
            then.json_body(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "result": {
                    "tools": [
                        {"name":"echo","description":"echo","inputSchema":{"type":"object"}}
                    ]
                }
            }));
        });
        let _call_mock = server.mock(|when, then| {
            when.method(POST).body_contains("tools/call");
            then.json_body(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 3,
                "result": {
                    "content": [{"type":"text","text":"ok"}]
                }
            }));
        });

        let mut settings = crate::config::Settings::default();
        settings.mcp.enabled = true;
        settings.mcp.servers = vec![crate::config::McpServerConfig::Remote {
            name: "remote-one".to_string(),
            url: server.url("/mcp"),
            headers: HashMap::new(),
            timeout_ms: Some(5000),
            enabled: Some(true),
        }];
        let registry = ToolRegistry::with_builtin_async(&settings, &std::path::PathBuf::from("."), None)
            .await
            .unwrap();
        let tool_name = "mcp__remote-one__echo";
        assert!(registry.get(tool_name).is_some());

        let ctx = ToolContext::new(std::path::PathBuf::from("."), vec![], "s1", None);
        let output = registry
            .run(&ctx, tool_name, serde_json::json!({"text":"hi"}))
            .await
            .unwrap();
        assert_eq!(output.content, "ok");
    }

    #[tokio::test]
    async fn todo_tools_read_write() {
        let dir = TempDir::new().unwrap();
        let store = SqliteSessionStore::new(dir.path().join("test.db")).await.unwrap();
        store.init().await.unwrap();
        let session = store.create_session("test".to_string()).await.unwrap();
        let store = Arc::new(store);

        let ctx = ToolContext::new(dir.path().to_path_buf(), vec![], session.id.clone(), Some(store));
        let registry = ToolRegistry::with_builtin();

        let write_args = serde_json::json!({
            "todos": [
                {"content": "第一步", "status": "pending", "priority": "high"},
                {"content": "第二步", "status": "in_progress", "priority": "medium"}
            ]
        });
        let output = registry.run(&ctx, "todowrite", write_args).await.unwrap();
        assert!(output.content.contains("第一步"));

        let output = registry.run(&ctx, "todoread", serde_json::json!({})).await.unwrap();
        assert!(output.content.contains("第二步"));
    }

    #[tokio::test]
    async fn todo_tools_reject_invalid_status() {
        let dir = TempDir::new().unwrap();
        let store = SqliteSessionStore::new(dir.path().join("test.db")).await.unwrap();
        store.init().await.unwrap();
        let session = store.create_session("test".to_string()).await.unwrap();
        let store = Arc::new(store);

        let ctx = ToolContext::new(dir.path().to_path_buf(), vec![], session.id.clone(), Some(store));
        let registry = ToolRegistry::with_builtin();
        let write_args = serde_json::json!({
            "todos": [
                {"content": "无效状态", "status": "doing", "priority": "low"}
            ]
        });
        let result = registry.run(&ctx, "todowrite", write_args).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn subagent_tool_creates_child_session() {
        struct DummyProvider;

        #[async_trait]
        impl Provider for DummyProvider {
            fn id(&self) -> &str {
                "dummy"
            }

            async fn send(&self, _request: ProviderRequest) -> ZeroBotResult<ProviderResponse> {
                Ok(ProviderResponse {
                    content: "子代理输出".to_string(),
                    tool_calls: Vec::new(),
                    raw: serde_json::json!({}),
                })
            }
        }

        let dir = TempDir::new().unwrap();
        let agents_dir = dir.path().join(".zerobot/agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(
            agents_dir.join("demo.md"),
            r#"---
name: demo
description: 示例
---

子代理内容
"#,
        )
        .unwrap();

        let store = SqliteSessionStore::new(dir.path().join("test.db")).await.unwrap();
        store.init().await.unwrap();
        let parent = store
            .create_session_with_parent("主会话".to_string(), None, SessionKind::Main)
            .await
            .unwrap();

        let provider_factory: ProviderFactory = std::sync::Arc::new(|| Ok(Box::new(DummyProvider)));
        let settings = Settings::default();
        let mut registry = ToolRegistry::with_builtin();
        let subagent_tools = registry.clone();
        let hooks = crate::hooks::HookManager::empty();
        registry.register(SubagentTool::new(
            settings.clone(),
            std::sync::Arc::new(store),
            subagent_tools,
            dir.path().to_path_buf(),
            provider_factory,
            "dummy-model".to_string(),
            hooks,
        ));

        let ctx = ToolContext::new(dir.path().to_path_buf(), vec![], parent.id.clone(), None);
        let output = registry
            .run(&ctx, "subagent", serde_json::json!({"name":"demo","prompt":"hi"}))
            .await
            .unwrap();
        assert_eq!(output.content, "子代理输出");
    }
}
