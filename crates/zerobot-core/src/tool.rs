use crate::agent::Agent;
use crate::agents::AgentManager;
use crate::bus::OutboundMessage;
use crate::config::{Settings, ToolOutputDirection, ToolOutputSettings};
use crate::cron::{CronPayload, CronSchedule, CronScheduleKind, CronService};
use crate::error::{ZeroBotError, ZeroBotResult};
use crate::hooks::HookManager;
use crate::instruction;
use crate::interaction::{InteractionHandler, UserInputRequest, UserInputResponse};
use crate::mcp::{format_tool_output, McpManager, McpToolInfo};
use crate::plugin::PluginManager;
use crate::provider::ProviderFactory;
use crate::session::{FileReadRecord, SessionKind, SessionStore, TodoItem};
use crate::skills::{format_skill_summary, SkillContent, SkillManager};
use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use diffy::{create_patch, Patch};
use regex::Regex;
use serde::Deserialize;
use serde_json::{json, Value as JsonValue};
use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::process::Command;
use tokio::sync::{mpsc, RwLock};
use tokio::time::timeout;
use url::Url;
use uuid::Uuid;
use walkdir::WalkDir;

#[derive(Clone)]
pub struct ToolContext {
    pub cwd: PathBuf,
    pub allow_paths: Vec<PathBuf>,
    pub session_id: String,
    pub store: Option<Arc<dyn SessionStore>>,
    pub interaction: Option<Arc<dyn InteractionHandler>>,
    pub plugins: Option<Arc<PluginManager>>,
    pub route: Option<ToolRouteContext>,
    pub outbound: Option<mpsc::UnboundedSender<OutboundMessage>>,
}

#[derive(Debug, Clone)]
pub struct ToolRouteContext {
    pub channel: String,
    pub chat_id: String,
    pub message_id: Option<String>,
}

impl ToolContext {
    pub fn new(
        cwd: PathBuf,
        allow_paths: Vec<PathBuf>,
        session_id: impl Into<String>,
        store: Option<Arc<dyn SessionStore>>,
        interaction: Option<Arc<dyn InteractionHandler>>,
    ) -> Self {
        Self {
            cwd,
            allow_paths,
            session_id: session_id.into(),
            store,
            interaction,
            plugins: None,
            route: None,
            outbound: None,
        }
    }

    pub fn with_plugins(mut self, plugins: Option<Arc<PluginManager>>) -> Self {
        self.plugins = plugins;
        self
    }

    pub fn with_route(mut self, route: Option<ToolRouteContext>) -> Self {
        self.route = route;
        self
    }

    pub fn with_outbound(
        mut self,
        outbound: Option<mpsc::UnboundedSender<OutboundMessage>>,
    ) -> Self {
        self.outbound = outbound;
        self
    }

    pub fn resolve_path(&self, input: &str) -> ZeroBotResult<PathBuf> {
        let path = PathBuf::from(input);
        let full = if path.is_absolute() {
            path
        } else {
            self.cwd.join(path)
        };
        let full = full.canonicalize().unwrap_or_else(|_| full.clone());

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

    pub fn interaction(&self) -> Option<Arc<dyn InteractionHandler>> {
        self.interaction.clone()
    }

    pub fn plugins(&self) -> Option<Arc<PluginManager>> {
        self.plugins.clone()
    }
}

fn param_error(err: serde_json::Error) -> ZeroBotError {
    ZeroBotError::Tool(format!("参数解析失败: {err}"))
}

fn io_error(op: &str, path: &std::path::Path, err: &io::Error) -> ZeroBotError {
    let detail = match err.kind() {
        io::ErrorKind::NotFound => "路径不存在",
        io::ErrorKind::PermissionDenied => "权限不足",
        io::ErrorKind::AlreadyExists => "目标已存在",
        io::ErrorKind::IsADirectory => "目标是目录",
        io::ErrorKind::NotADirectory => "目标不是目录",
        io::ErrorKind::InvalidInput => "输入非法",
        _ => "IO 错误",
    };
    ZeroBotError::Tool(format!("{op}失败: {detail}: {}", path.display()))
}

fn system_time_to_ts(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_secs() as i64
}

async fn record_file_read(ctx: &ToolContext, path: &Path) -> ZeroBotResult<()> {
    let Some(store) = ctx.store() else {
        return Ok(());
    };
    let metadata = tokio::fs::metadata(path)
        .await
        .map_err(|err| io_error("读取文件元信息", path, &err))?;
    let mtime = metadata
        .modified()
        .map(system_time_to_ts)
        .unwrap_or_default();
    store
        .record_file_read(&ctx.session_id, &path.to_string_lossy(), mtime)
        .await?;
    Ok(())
}

async fn ensure_read_before_write(
    ctx: &ToolContext,
    path: &Path,
) -> ZeroBotResult<Option<FileReadRecord>> {
    let metadata = match tokio::fs::metadata(path).await {
        Ok(meta) => meta,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(io_error("读取文件元信息", path, &err)),
    };
    if metadata.is_dir() {
        return Err(ZeroBotError::Tool("目标是目录，无法写入".to_string()));
    }
    let store = ctx
        .store()
        .ok_or_else(|| ZeroBotError::Tool("写入前需要 SessionStore 用于 read 校验".to_string()))?;
    let current_mtime = metadata
        .modified()
        .map(system_time_to_ts)
        .unwrap_or_default();
    let record = store
        .get_file_read(&ctx.session_id, &path.to_string_lossy())
        .await?;
    let Some(record) = record else {
        return Err(ZeroBotError::Tool(
            "写入前请先使用 read 读取目标文件".to_string(),
        ));
    };
    if record.mtime != current_mtime {
        return Err(ZeroBotError::Tool(
            "文件已发生变化，请重新使用 read 读取后再写入".to_string(),
        ));
    }
    Ok(Some(record))
}
#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub title: Option<String>,
    pub content: String,
    pub metadata: JsonValue,
    pub truncated: bool,
    pub output_path: Option<String>,
}

impl ToolOutput {
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            title: None,
            content: content.into(),
            metadata: json!({}),
            truncated: false,
            output_path: None,
        }
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn with_metadata(mut self, metadata: JsonValue) -> Self {
        self.metadata = metadata;
        self
    }

    pub fn truncated(mut self, output_path: Option<String>) -> Self {
        self.truncated = true;
        self.output_path = output_path;
        self
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
    plugins: Option<Arc<PluginManager>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
            plugins: None,
        }
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
        render_tool_output(ctx, output, output_settings).await
    }

    pub fn with_builtin() -> Self {
        let mut registry = Self::new();
        registry.register(ReadTool);
        registry.register(WriteTool);
        registry.register(EditTool);
        registry.register(ApplyPatchTool);
        registry.register(PatchTool);
        registry.register(GlobTool);
        registry.register(GrepTool);
        registry.register(BashTool);
        registry.register(ShellTool);
        registry.register(TodoReadTool);
        registry.register(TodoWriteTool);
        registry.register(RequestUserInputTool);
        registry.register(MessageTool);
        registry
    }

    pub async fn with_builtin_async(
        settings: &crate::config::Settings,
        cwd: &std::path::Path,
        _store: Option<Arc<dyn SessionStore>>,
        plugins: Option<Arc<PluginManager>>,
    ) -> ZeroBotResult<Self> {
        let mut registry = Self::with_builtin();
        registry.plugins = plugins.clone();

        if settings.skills.enabled {
            let manager = Arc::new(SkillManager::new(settings, cwd));
            let description = match manager.discover() {
                Ok(list) => {
                    if list.is_empty() {
                        "加载指定 Skill 的内容。当前没有可用 Skill。".to_string()
                    } else {
                        format!("加载指定 Skill 的内容。\n\n{}", format_skill_summary(&list))
                    }
                }
                Err(_) => "加载指定 Skill 的内容。".to_string(),
            };
            registry.register(SkillTool {
                manager,
                description,
            });
        }

        let workspace_root = crate::workspace::resolve_workspace_root(cwd);
        let db_path = crate::workspace::resolve_session_db_path(&workspace_root);
        let cron_export_path = settings
            .gateway
            .cron
            .export_json
            .as_ref()
            .map(PathBuf::from)
            .or_else(|| Some(workspace_root.join(".zerobot").join("cron-jobs.json")));
        let cron_service = Arc::new(
            CronService::new(
                db_path,
                cron_export_path,
                settings.gateway.cron.run_history_limit,
            )
            .await?,
        );
        registry.register(CronTool::new(cron_service));

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

        if let Some(manager) = plugins {
            for tool in manager.tools() {
                registry.tools.insert(
                    tool.name.clone(),
                    Arc::new(PluginToolAdapter {
                        manager: manager.clone(),
                        info: tool,
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
    interaction: Option<Arc<dyn InteractionHandler>>,
    tool_approvals: Arc<RwLock<HashSet<String>>>,
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
        interaction: Option<Arc<dyn InteractionHandler>>,
        tool_approvals: Arc<RwLock<HashSet<String>>>,
    ) -> Self {
        Self {
            settings,
            store,
            tools,
            cwd,
            provider_factory,
            fallback_model,
            parent_hooks,
            interaction,
            tool_approvals,
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
        let args: SubagentArgs =
            serde_json::from_value(args).map_err(|err| ZeroBotError::Tool(err.to_string()))?;

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

        let parent_skill_hooks: Vec<crate::hooks::HookDefinition> = Vec::new();
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
            self.interaction.clone(),
            ctx.plugins(),
            self.tool_approvals.clone(),
            ctx.route.clone(),
            ctx.outbound.clone(),
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
    description: String,
}

#[derive(Deserialize)]
struct SkillArgs {
    name: String,
}

#[async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &str {
        "skill"
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> JsonValue {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {"type": "string", "description": "可用 Skill 名称"}
            },
            "required": ["name"]
        })
    }

    async fn run(&self, _ctx: &ToolContext, args: JsonValue) -> ZeroBotResult<ToolOutput> {
        let args: SkillArgs =
            serde_json::from_value(args).map_err(|err| ZeroBotError::Tool(err.to_string()))?;
        let SkillContent { info, body } = match self.manager.load(&args.name) {
            Ok(content) => content,
            Err(ZeroBotError::Skill(_)) => {
                let available = self
                    .manager
                    .discover()?
                    .into_iter()
                    .map(|skill| skill.name)
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(ZeroBotError::Tool(format!(
                    "Skill \"{}\" 未找到。可用 Skills: {}",
                    args.name,
                    if available.is_empty() {
                        "none".to_string()
                    } else {
                        available
                    }
                )));
            }
            Err(err) => return Err(err),
        };

        let dir = info
            .path
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let base = Url::from_directory_path(&dir)
            .map(|url| url.to_string())
            .unwrap_or_else(|_| format!("file://{}", dir.display()));
        let files = sample_skill_files(&dir, 10)
            .into_iter()
            .map(|file| format!("<file>{}</file>", file.display()))
            .collect::<Vec<_>>()
            .join("\n");

        let output = [
            format!("<skill_content name=\"{}\">", info.name),
            format!("# Skill: {}", info.name),
            String::new(),
            body.trim().to_string(),
            String::new(),
            format!("Base directory for this skill: {base}"),
            "Relative paths in this skill (for example scripts/ and references/) are relative to this base directory.".to_string(),
            "Note: file list is sampled.".to_string(),
            String::new(),
            "<skill_files>".to_string(),
            files,
            "</skill_files>".to_string(),
            "</skill_content>".to_string(),
        ]
        .join("\n");

        Ok(ToolOutput::new(output)
            .with_title(format!("Loaded skill: {}", info.name))
            .with_metadata(json!({
                "name": info.name,
                "dir": dir.to_string_lossy().to_string(),
            })))
    }
}

fn sample_skill_files(dir: &Path, limit: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in WalkDir::new(dir).into_iter().filter_map(Result::ok) {
        if !entry.file_type().is_file() {
            continue;
        }
        if entry.file_name() == "SKILL.md" {
            continue;
        }
        out.push(entry.path().to_path_buf());
        if out.len() >= limit {
            break;
        }
    }
    out
}

struct McpToolAdapter {
    manager: Arc<McpManager>,
    full_name: String,
    tool: McpToolInfo,
}

struct PluginToolAdapter {
    manager: Arc<PluginManager>,
    info: crate::plugin::PluginToolInfo,
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

#[async_trait]
impl Tool for PluginToolAdapter {
    fn name(&self) -> &str {
        &self.info.name
    }

    fn description(&self) -> &str {
        &self.info.description
    }

    fn parameters(&self) -> JsonValue {
        self.info.parameters.clone()
    }

    async fn run(&self, ctx: &ToolContext, args: JsonValue) -> ZeroBotResult<ToolOutput> {
        let result = self
            .manager
            .call_tool(
                &self.info.plugin,
                &self.info.name,
                args,
                json!({
                    "session_id": ctx.session_id,
                    "cwd": ctx.cwd.to_string_lossy(),
                    "allow_paths": ctx
                        .allow_paths
                        .iter()
                        .map(|p| p.to_string_lossy().to_string())
                        .collect::<Vec<_>>(),
                }),
            )
            .await?;
        let content = result
            .get("output")
            .and_then(|v| v.as_str())
            .map(ToString::to_string)
            .unwrap_or_else(|| {
                if result.is_string() {
                    result.as_str().unwrap_or_default().to_string()
                } else {
                    result.to_string()
                }
            });
        let title = result
            .get("title")
            .and_then(|v| v.as_str())
            .map(ToString::to_string);
        let metadata = result.get("metadata").cloned().unwrap_or_else(|| json!({}));
        let mut output = ToolOutput::new(content).with_metadata(metadata);
        if let Some(title) = title {
            output = output.with_title(title);
        }
        Ok(output)
    }
}

struct TruncateResult {
    content: String,
    truncated: bool,
    output_path: Option<String>,
    summary: Option<String>,
}

async fn render_tool_output(
    ctx: &ToolContext,
    output: ToolOutput,
    settings: &ToolOutputSettings,
) -> ZeroBotResult<ToolOutput> {
    let title = output.title.clone();
    let metadata = output.metadata.clone();
    let trunc = truncate_tool_content(ctx, &output.content, settings).await?;
    let wrapped = wrap_tool_output(
        title.as_deref(),
        &trunc.content,
        &metadata,
        trunc.summary.as_deref(),
        trunc.truncated,
        trunc.output_path.as_deref(),
    );
    Ok(ToolOutput {
        title,
        content: wrapped,
        metadata,
        truncated: trunc.truncated,
        output_path: trunc.output_path,
    })
}

async fn truncate_tool_content(
    ctx: &ToolContext,
    content: &str,
    settings: &ToolOutputSettings,
) -> ZeroBotResult<TruncateResult> {
    let max_lines = settings.max_lines;
    let max_bytes = settings.max_bytes;
    let direction = settings.direction;

    let lines: Vec<&str> = content.split('\n').collect();
    let total_bytes = content.as_bytes().len();

    if lines.len() <= max_lines && total_bytes <= max_bytes {
        return Ok(TruncateResult {
            content: content.to_string(),
            truncated: false,
            output_path: None,
            summary: None,
        });
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

    let output_path = persist_tool_output(ctx, content).await?;
    let summary = format!(
        "输出过长已截断（移除 {removed} {unit}）。完整输出已保存至: {output_path}。可使用 read 配合 offset/limit 或 grep 进行检索。"
    );

    let truncated_preview = if direction == ToolOutputDirection::Head {
        preview
    } else {
        preview
    };

    Ok(TruncateResult {
        content: truncated_preview,
        truncated: true,
        output_path: Some(output_path),
        summary: Some(summary),
    })
}

fn wrap_tool_output(
    title: Option<&str>,
    content: &str,
    metadata: &JsonValue,
    summary: Option<&str>,
    truncated: bool,
    output_path: Option<&str>,
) -> String {
    let mut out = String::new();
    out.push_str("<tool_output>\n");
    if let Some(title) = title {
        out.push_str("<title>");
        out.push_str(title);
        out.push_str("</title>\n");
    }
    if let Some(summary) = summary {
        out.push_str("<summary>");
        out.push_str(summary);
        out.push_str("</summary>\n");
    }
    if truncated {
        out.push_str("<truncated>true</truncated>\n");
        if let Some(path) = output_path {
            out.push_str("<output_path>");
            out.push_str(path);
            out.push_str("</output_path>\n");
        }
    }
    out.push_str("<content>\n");
    out.push_str(content);
    out.push_str("\n</content>\n");
    let metadata_json = serde_json::to_string(metadata).unwrap_or_else(|_| "{}".to_string());
    out.push_str("<metadata>");
    out.push_str(&metadata_json);
    out.push_str("</metadata>\n");
    out.push_str("</tool_output>");
    out
}

async fn persist_tool_output(ctx: &ToolContext, content: &str) -> ZeroBotResult<String> {
    let dir = ctx.cwd.join(".zerobot").join("tool-output");
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|err| io_error("创建工具输出目录", &dir, &err))?;
    let filename = format!(
        "tool_output_{}_{}.txt",
        Utc::now().timestamp(),
        Uuid::new_v4()
    );
    let path = dir.join(filename);
    tokio::fs::write(&path, content)
        .await
        .map_err(|err| io_error("写入工具输出", &path, &err))?;
    Ok(path.to_string_lossy().to_string())
}

struct ReadTool;

#[derive(Deserialize)]
struct ReadArgs {
    #[serde(rename = "filePath", alias = "path")]
    file_path: String,
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
        "读取文件或目录内容（1-indexed 行号）"
    }

    fn parameters(&self) -> JsonValue {
        serde_json::json!({
            "type": "object",
            "properties": {
                "filePath": {"type": "string", "description": "文件或目录路径（建议使用绝对路径）"},
                "offset": {"type": "integer", "description": "起始行号（从 1 开始）"},
                "limit": {"type": "integer", "description": "返回的最大行数（默认 2000）"}
            },
            "required": ["filePath"]
        })
    }

    async fn run(&self, ctx: &ToolContext, args: JsonValue) -> ZeroBotResult<ToolOutput> {
        let args: ReadArgs = serde_json::from_value(args).map_err(param_error)?;
        let path = ctx.resolve_path(&args.file_path)?;
        let offset = args.offset.unwrap_or(1);
        let limit = args.limit.unwrap_or(2000);
        if offset == 0 {
            return Err(ZeroBotError::Tool("offset 必须从 1 开始".to_string()));
        }
        if limit == 0 {
            return Err(ZeroBotError::Tool("limit 必须大于 0".to_string()));
        }

        let metadata = tokio::fs::metadata(&path)
            .await
            .map_err(|err| io_error("读取文件", &path, &err))?;

        let title = path
            .strip_prefix(&ctx.cwd)
            .unwrap_or(&path)
            .to_string_lossy()
            .to_string();

        if metadata.is_dir() {
            let mut entries = Vec::new();
            let mut dir = tokio::fs::read_dir(&path)
                .await
                .map_err(|err| io_error("读取目录", &path, &err))?;
            while let Some(entry) = dir
                .next_entry()
                .await
                .map_err(|err| io_error("读取目录项", &path, &err))?
            {
                let name = entry.file_name().to_string_lossy().to_string();
                let file_type = entry
                    .file_type()
                    .await
                    .map_err(|err| io_error("读取目录项", &entry.path(), &err))?;
                if file_type.is_dir() {
                    entries.push(format!("{name}/"));
                } else {
                    entries.push(name);
                }
            }
            entries.sort();

            if entries.is_empty() {
                if offset > 1 {
                    return Err(ZeroBotError::Tool(
                        "offset 超出范围（目录为空）".to_string(),
                    ));
                }
            } else if offset > entries.len() {
                return Err(ZeroBotError::Tool(format!(
                    "offset 超出范围（目录共有 {} 项）",
                    entries.len()
                )));
            }
            let start = offset.saturating_sub(1);
            let end = (start + limit).min(entries.len());
            let slice = entries[start..end].to_vec();
            let truncated = end < entries.len();
            let summary = if truncated {
                format!(
                    "Showing entries {}-{} of {}. Use offset={} to continue.",
                    offset,
                    offset + slice.len().saturating_sub(1),
                    entries.len(),
                    end + 1
                )
            } else {
                format!("Directory entries: {}", entries.len())
            };

            let mut body = String::new();
            body.push_str(&format!(
                "<path>{}</path>\n<type>directory</type>\n<entries>\n",
                path.display()
            ));
            body.push_str(&slice.join("\n"));
            body.push_str("\n</entries>\n");
            body.push_str(&format!("<summary>{summary}</summary>"));

            return Ok(ToolOutput::new(body)
                .with_title(title)
                .with_metadata(json!({
                    "path": path.to_string_lossy(),
                    "type": "directory",
                    "offset": offset,
                    "limit": limit,
                    "total_entries": entries.len(),
                    "truncated": truncated
                })));
        }

        let content = match tokio::fs::read_to_string(&path).await {
            Ok(content) => content,
            Err(err) if err.kind() == io::ErrorKind::InvalidData => {
                return Err(ZeroBotError::Tool("无法读取二进制文件".to_string()))
            }
            Err(err) => return Err(io_error("读取文件", &path, &err)),
        };

        let lines: Vec<&str> = content.lines().collect();
        if lines.is_empty() {
            if offset > 1 {
                return Err(ZeroBotError::Tool(
                    "offset 超出范围（文件为空）".to_string(),
                ));
            }
        } else if offset > lines.len() {
            return Err(ZeroBotError::Tool(format!(
                "offset 超出范围（文件共有 {} 行）",
                lines.len()
            )));
        }
        let start = offset.saturating_sub(1);
        let end = (start + limit).min(lines.len());
        let mut output_lines = Vec::new();
        const MAX_LINE_LENGTH: usize = 2000;
        for (idx, line) in lines[start..end].iter().enumerate() {
            let mut text = line.to_string();
            if text.chars().count() > MAX_LINE_LENGTH {
                text = text.chars().take(MAX_LINE_LENGTH).collect::<String>()
                    + "... (line truncated to 2000 chars)";
            }
            output_lines.push(format!("{}: {}", offset + idx, text));
        }
        let truncated = end < lines.len();
        let summary = if lines.is_empty() {
            "Empty file".to_string()
        } else if truncated {
            format!(
                "Showing lines {}-{} of {}. Use offset={} to continue.",
                offset,
                offset + output_lines.len().saturating_sub(1),
                lines.len(),
                end + 1
            )
        } else {
            format!("End of file - total {} lines", lines.len())
        };

        let mut body = String::new();
        body.push_str(&format!(
            "<path>{}</path>\n<type>file</type>\n<content>\n",
            path.display()
        ));
        body.push_str(&output_lines.join("\n"));
        body.push_str("\n</content>\n");
        body.push_str(&format!("<summary>{summary}</summary>"));

        let reminders = instruction::resolve_nearby_instructions(&ctx.session_id, &path);
        if !reminders.is_empty() {
            let reminder_text = reminders
                .iter()
                .map(|item| item.content.as_str())
                .collect::<Vec<_>>()
                .join("\n\n");
            body.push_str("\n<system-reminder>\n");
            body.push_str(&reminder_text);
            body.push_str("\n</system-reminder>");
        }

        record_file_read(ctx, &path).await?;

        Ok(ToolOutput::new(body)
            .with_title(title)
            .with_metadata(json!({
                "path": path.to_string_lossy(),
                "type": "file",
                "offset": offset,
                "limit": limit,
                "total_lines": lines.len(),
                "truncated": truncated
            })))
    }
}

struct WriteTool;

#[derive(Deserialize)]
struct WriteArgs {
    #[serde(rename = "filePath", alias = "path")]
    file_path: String,
    content: String,
    #[serde(default)]
    append: Option<bool>,
}

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &str {
        "write"
    }

    fn description(&self) -> &str {
        "写入文件（覆盖写入）"
    }

    fn parameters(&self) -> JsonValue {
        serde_json::json!({
            "type": "object",
            "properties": {
                "filePath": {"type": "string", "description": "文件路径（建议使用绝对路径）"},
                "content": {"type": "string", "description": "写入内容"},
            },
            "required": ["filePath", "content"]
        })
    }

    async fn run(&self, ctx: &ToolContext, args: JsonValue) -> ZeroBotResult<ToolOutput> {
        let args: WriteArgs = serde_json::from_value(args).map_err(param_error)?;
        if args.append.unwrap_or(false) {
            return Err(ZeroBotError::Tool(
                "append 已废弃，请使用 edit 或 apply_patch 完成追加".to_string(),
            ));
        }
        let path = ctx.resolve_path(&args.file_path)?;
        let existed = tokio::fs::metadata(&path).await.is_ok();
        if existed {
            let _ = ensure_read_before_write(ctx, &path).await?;
        }
        let before = if existed {
            tokio::fs::read_to_string(&path).await.unwrap_or_default()
        } else {
            String::new()
        };
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|err| io_error("创建目录", parent, &err))?;
        }
        tokio::fs::write(&path, &args.content)
            .await
            .map_err(|err| io_error("写入文件", &path, &err))?;
        let diff = create_patch(&before, &args.content).to_string();
        let summary = if existed {
            "Wrote file successfully."
        } else {
            "Created file successfully."
        };
        let body = format!(
            "<path>{}</path>\n<action>write</action>\n<summary>{}</summary>\n<diff>\n{}\n</diff>",
            path.display(),
            summary,
            diff.trim_end()
        );
        Ok(ToolOutput::new(body)
            .with_title(
                path.strip_prefix(&ctx.cwd)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .to_string(),
            )
            .with_metadata(json!({
                "path": path.to_string_lossy(),
                "existed": existed
            })))
    }
}

struct EditTool;

#[derive(Deserialize)]
struct EditArgs {
    #[serde(rename = "filePath", alias = "path")]
    file_path: String,
    #[serde(rename = "oldString", alias = "find")]
    old_string: String,
    #[serde(rename = "newString", alias = "replace")]
    new_string: String,
    #[serde(default)]
    #[serde(rename = "replaceAll", alias = "replace_all")]
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
                "filePath": {"type": "string", "description": "要修改的文件路径"},
                "oldString": {"type": "string", "description": "要替换的文本"},
                "newString": {"type": "string", "description": "替换后的文本"},
                "replaceAll": {"type": "boolean", "description": "是否替换全部匹配项"}
            },
            "required": ["filePath", "oldString", "newString"]
        })
    }

    async fn run(&self, ctx: &ToolContext, args: JsonValue) -> ZeroBotResult<ToolOutput> {
        let args: EditArgs = serde_json::from_value(args).map_err(param_error)?;
        let path = ctx.resolve_path(&args.file_path)?;
        let _ = ensure_read_before_write(ctx, &path).await?;
        let mut content = tokio::fs::read_to_string(&path)
            .await
            .map_err(|err| io_error("读取文件", &path, &err))?;
        if args.old_string == args.new_string {
            return Err(ZeroBotError::Tool(
                "oldString 与 newString 相同，未产生任何修改".to_string(),
            ));
        }
        if args.old_string.is_empty() {
            return Err(ZeroBotError::Tool("oldString 不能为空".to_string()));
        }

        let original = content.clone();
        let line_ending = if content.contains("\r\n") {
            "\r\n"
        } else {
            "\n"
        };
        let old = args.old_string.replace("\r\n", "\n").replace('\r', "\n");
        let new = args.new_string.replace("\r\n", "\n").replace('\r', "\n");
        let old = if line_ending == "\n" {
            old
        } else {
            old.replace('\n', "\r\n")
        };
        let new = if line_ending == "\n" {
            new
        } else {
            new.replace('\n', "\r\n")
        };

        let count = content.matches(&old).count();
        if count == 0 {
            return Err(ZeroBotError::Tool("oldString 未找到".to_string()));
        }
        if !args.replace_all && count > 1 {
            return Err(ZeroBotError::Tool(
                "找到多个匹配项，请提供更长的 oldString 或设置 replaceAll".to_string(),
            ));
        }

        if args.replace_all {
            content = content.replace(&old, &new);
        } else if let Some(pos) = content.find(&old) {
            content.replace_range(pos..pos + old.len(), &new);
        }

        tokio::fs::write(&path, &content)
            .await
            .map_err(|err| io_error("写入文件", &path, &err))?;
        let diff = create_patch(&original, &content).to_string();
        let body = format!(
            "<path>{}</path>\n<action>edit</action>\n<summary>Replaced {count} occurrence(s).</summary>\n<diff>\n{}\n</diff>",
            path.display(),
            diff.trim_end()
        );
        Ok(ToolOutput::new(body)
            .with_title(
                path.strip_prefix(&ctx.cwd)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .to_string(),
            )
            .with_metadata(json!({
                "path": path.to_string_lossy(),
                "replaced": count
            })))
    }
}

struct ApplyPatchTool;

#[derive(Deserialize)]
struct ApplyPatchArgs {
    #[serde(rename = "patchText", alias = "patch", alias = "patch_text")]
    patch_text: String,
}

#[async_trait]
impl Tool for ApplyPatchTool {
    fn name(&self) -> &str {
        "apply_patch"
    }

    fn description(&self) -> &str {
        "应用补丁（*** Begin Patch 格式）"
    }

    fn parameters(&self) -> JsonValue {
        serde_json::json!({
            "type": "object",
            "properties": {
                "patchText": {"type": "string", "description": "完整补丁内容（*** Begin Patch ... *** End Patch）"}
            },
            "required": ["patchText"]
        })
    }

    async fn run(&self, ctx: &ToolContext, args: JsonValue) -> ZeroBotResult<ToolOutput> {
        let args: ApplyPatchArgs = serde_json::from_value(args).map_err(param_error)?;
        apply_patch_text(ctx, &args.patch_text).await
    }
}

struct PatchTool;

#[derive(Deserialize)]
struct PatchArgs {
    #[serde(rename = "patchText", alias = "patch")]
    patch_text: String,
    #[serde(default)]
    path: Option<String>,
}

#[async_trait]
impl Tool for PatchTool {
    fn name(&self) -> &str {
        "patch"
    }

    fn description(&self) -> &str {
        "补丁兼容工具（推荐使用 apply_patch）"
    }

    fn parameters(&self) -> JsonValue {
        serde_json::json!({
            "type": "object",
            "properties": {
                "patchText": {"type": "string", "description": "补丁内容（*** Begin Patch ...）"},
                "path": {"type": "string", "description": "旧版兼容：与 patch 配合的目标文件路径"}
            },
            "required": ["patchText"]
        })
    }

    async fn run(&self, ctx: &ToolContext, args: JsonValue) -> ZeroBotResult<ToolOutput> {
        let args: PatchArgs = serde_json::from_value(args).map_err(param_error)?;
        let trimmed = args.patch_text.trim_start();
        if trimmed.starts_with("*** Begin Patch") {
            return apply_patch_text(ctx, &args.patch_text).await;
        }
        if let Some(path) = args.path {
            return apply_legacy_patch(ctx, &path, &args.patch_text).await;
        }
        Err(ZeroBotError::Tool(
            "patch 需要 patchText（*** Begin Patch 格式）或旧版 path+patch".to_string(),
        ))
    }
}

#[derive(Debug)]
enum ApplyPatchOp {
    Add {
        path: String,
        content: String,
    },
    Update {
        path: String,
        move_to: Option<String>,
        hunks: Vec<ApplyPatchHunk>,
    },
    Delete {
        path: String,
    },
}

#[derive(Debug)]
struct ApplyPatchHunk {
    lines: Vec<ApplyPatchLine>,
}

#[derive(Debug)]
enum ApplyPatchLine {
    Context(String),
    Add(String),
    Remove(String),
}

async fn apply_patch_text(ctx: &ToolContext, patch_text: &str) -> ZeroBotResult<ToolOutput> {
    let ops = parse_apply_patch(patch_text)?;
    if ops.is_empty() {
        return Err(ZeroBotError::Tool("补丁为空".to_string()));
    }

    let mut diffs = Vec::new();
    let mut files = Vec::new();
    let mut additions = 0usize;
    let mut deletions = 0usize;

    for op in ops {
        match op {
            ApplyPatchOp::Add { path, content } => {
                let full = resolve_relative_path(ctx, &path)?;
                if tokio::fs::metadata(&full).await.is_ok() {
                    return Err(ZeroBotError::Tool(format!(
                        "文件已存在，无法 Add: {}",
                        full.display()
                    )));
                }
                if let Some(parent) = full.parent() {
                    tokio::fs::create_dir_all(parent)
                        .await
                        .map_err(|err| io_error("创建目录", parent, &err))?;
                }
                tokio::fs::write(&full, &content)
                    .await
                    .map_err(|err| io_error("写入文件", &full, &err))?;
                let diff = create_patch("", &content).to_string();
                let (add, del) = count_diff_changes(&diff);
                additions += add;
                deletions += del;
                diffs.push(format!("## {}\n{}", full.display(), diff.trim_end()));
                files.push(format!("{} (add)", full.display()));
            }
            ApplyPatchOp::Update {
                path,
                move_to,
                hunks,
            } => {
                let full = resolve_relative_path(ctx, &path)?;
                let _ = ensure_read_before_write(ctx, &full).await?;
                let original = tokio::fs::read_to_string(&full)
                    .await
                    .map_err(|err| io_error("读取文件", &full, &err))?;
                let updated = apply_hunks(&original, &hunks)?;
                let target = if let Some(move_to) = move_to {
                    resolve_relative_path(ctx, &move_to)?
                } else {
                    full.clone()
                };
                if target != full && tokio::fs::metadata(&target).await.is_ok() {
                    return Err(ZeroBotError::Tool(format!(
                        "目标已存在，无法 Move to: {}",
                        target.display()
                    )));
                }
                if let Some(parent) = target.parent() {
                    tokio::fs::create_dir_all(parent)
                        .await
                        .map_err(|err| io_error("创建目录", parent, &err))?;
                }
                tokio::fs::write(&target, &updated)
                    .await
                    .map_err(|err| io_error("写入文件", &target, &err))?;
                if target != full {
                    tokio::fs::remove_file(&full)
                        .await
                        .map_err(|err| io_error("删除文件", &full, &err))?;
                }
                let diff = create_patch(&original, &updated).to_string();
                let (add, del) = count_diff_changes(&diff);
                additions += add;
                deletions += del;
                diffs.push(format!("## {}\n{}", target.display(), diff.trim_end()));
                let label = if target != full {
                    format!("{} (move to {})", full.display(), target.display())
                } else {
                    format!("{} (update)", full.display())
                };
                files.push(label);
            }
            ApplyPatchOp::Delete { path } => {
                let full = resolve_relative_path(ctx, &path)?;
                let _ = ensure_read_before_write(ctx, &full).await?;
                let original = tokio::fs::read_to_string(&full)
                    .await
                    .map_err(|err| io_error("读取文件", &full, &err))?;
                tokio::fs::remove_file(&full)
                    .await
                    .map_err(|err| io_error("删除文件", &full, &err))?;
                let diff = create_patch(&original, "").to_string();
                let (add, del) = count_diff_changes(&diff);
                additions += add;
                deletions += del;
                diffs.push(format!("## {}\n{}", full.display(), diff.trim_end()));
                files.push(format!("{} (delete)", full.display()));
            }
        }
    }

    let mut body = String::new();
    body.push_str(&format!(
        "<summary>Applied patch to {} file(s).</summary>\n",
        files.len()
    ));
    body.push_str("<files>\n");
    body.push_str(&files.join("\n"));
    body.push_str("\n</files>\n<diff>\n");
    body.push_str(&diffs.join("\n\n"));
    body.push_str("\n</diff>");

    Ok(ToolOutput::new(body)
        .with_title("apply_patch")
        .with_metadata(json!({
            "files_changed": files.len(),
            "additions": additions,
            "deletions": deletions
        })))
}

async fn apply_legacy_patch(
    ctx: &ToolContext,
    path: &str,
    patch_text: &str,
) -> ZeroBotResult<ToolOutput> {
    let full = ctx.resolve_path(path)?;
    let _ = ensure_read_before_write(ctx, &full).await?;
    let content = tokio::fs::read_to_string(&full)
        .await
        .map_err(|err| io_error("读取文件", &full, &err))?;
    let patch = Patch::from_str(patch_text).map_err(|err| ZeroBotError::Tool(err.to_string()))?;
    let updated =
        diffy::apply(&content, &patch).map_err(|err| ZeroBotError::Tool(err.to_string()))?;
    tokio::fs::write(&full, &updated)
        .await
        .map_err(|err| io_error("写入文件", &full, &err))?;
    let diff = create_patch(&content, &updated).to_string();
    let (additions, deletions) = count_diff_changes(&diff);
    let body = format!(
        "<summary>Applied legacy patch.</summary>\n<diff>\n{}\n</diff>",
        diff.trim_end()
    );
    Ok(ToolOutput::new(body)
        .with_title("patch")
        .with_metadata(json!({
            "path": full.to_string_lossy(),
            "additions": additions,
            "deletions": deletions
        })))
}

fn parse_apply_patch(patch: &str) -> ZeroBotResult<Vec<ApplyPatchOp>> {
    let normalized = patch.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = normalized.lines().collect();
    if lines.is_empty() || lines[0].trim() != "*** Begin Patch" {
        return Err(ZeroBotError::Tool(
            "apply_patch 需要以 \"*** Begin Patch\" 开头".to_string(),
        ));
    }
    let mut ops = Vec::new();
    let mut i = 1usize;
    while i < lines.len() {
        let line = lines[i].trim_end();
        if line == "*** End Patch" {
            break;
        }
        if line.is_empty() {
            i += 1;
            continue;
        }
        if let Some(path) = line.strip_prefix("*** Add File: ") {
            let path = path.trim().to_string();
            if path.is_empty() {
                return Err(ZeroBotError::Tool("Add File 缺少路径".to_string()));
            }
            i += 1;
            let mut content_lines = Vec::new();
            while i < lines.len() {
                let line = lines[i];
                if line.starts_with("*** ") {
                    break;
                }
                if !line.starts_with('+') {
                    return Err(ZeroBotError::Tool(
                        "Add File 只能包含 '+' 开头的内容行".to_string(),
                    ));
                }
                content_lines.push(line[1..].to_string());
                i += 1;
            }
            let mut content = content_lines.join("\n");
            if !content.is_empty() && !content.ends_with('\n') {
                content.push('\n');
            }
            ops.push(ApplyPatchOp::Add { path, content });
            continue;
        }
        if let Some(path) = line.strip_prefix("*** Delete File: ") {
            let path = path.trim().to_string();
            if path.is_empty() {
                return Err(ZeroBotError::Tool("Delete File 缺少路径".to_string()));
            }
            i += 1;
            ops.push(ApplyPatchOp::Delete { path });
            continue;
        }
        if let Some(path) = line.strip_prefix("*** Update File: ") {
            let path = path.trim().to_string();
            if path.is_empty() {
                return Err(ZeroBotError::Tool("Update File 缺少路径".to_string()));
            }
            i += 1;
            let mut move_to = None;
            if i < lines.len() {
                if let Some(next) = lines[i].strip_prefix("*** Move to: ") {
                    let target = next.trim().to_string();
                    if target.is_empty() {
                        return Err(ZeroBotError::Tool("Move to 缺少路径".to_string()));
                    }
                    move_to = Some(target);
                    i += 1;
                }
            }
            let mut hunks = Vec::new();
            while i < lines.len() {
                let line = lines[i];
                if line.starts_with("*** ") {
                    break;
                }
                if line.trim().is_empty() {
                    i += 1;
                    continue;
                }
                if !line.starts_with("@@") {
                    return Err(ZeroBotError::Tool(format!(
                        "Update File 需要 @@ 开头的 hunk，遇到: {line}"
                    )));
                }
                i += 1;
                let mut hunk_lines = Vec::new();
                while i < lines.len() {
                    let line = lines[i];
                    if line.starts_with("@@") || line.starts_with("*** ") {
                        break;
                    }
                    if line == "*** End of File" {
                        i += 1;
                        continue;
                    }
                    let mut chars = line.chars();
                    let prefix = chars
                        .next()
                        .ok_or_else(|| ZeroBotError::Tool("空行不符合补丁格式".to_string()))?;
                    let rest = chars.collect::<String>();
                    match prefix {
                        ' ' => hunk_lines.push(ApplyPatchLine::Context(rest)),
                        '+' => hunk_lines.push(ApplyPatchLine::Add(rest)),
                        '-' => hunk_lines.push(ApplyPatchLine::Remove(rest)),
                        _ => return Err(ZeroBotError::Tool(format!("无效的补丁行前缀: {prefix}"))),
                    }
                    i += 1;
                }
                if hunk_lines.is_empty() {
                    return Err(ZeroBotError::Tool("空的 hunk".to_string()));
                }
                hunks.push(ApplyPatchHunk { lines: hunk_lines });
            }
            if hunks.is_empty() {
                return Err(ZeroBotError::Tool("Update File 缺少 hunk".to_string()));
            }
            ops.push(ApplyPatchOp::Update {
                path,
                move_to,
                hunks,
            });
            continue;
        }

        return Err(ZeroBotError::Tool(format!("未知补丁头: {line}")));
    }
    Ok(ops)
}

fn apply_hunks(content: &str, hunks: &[ApplyPatchHunk]) -> ZeroBotResult<String> {
    let line_ending = if content.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let normalized = content.replace("\r\n", "\n");
    let ends_with_newline = normalized.ends_with('\n');
    let mut lines: Vec<String> = if normalized.is_empty() {
        Vec::new()
    } else {
        normalized
            .split_terminator('\n')
            .map(|s| s.to_string())
            .collect()
    };

    for hunk in hunks {
        let mut pattern = Vec::new();
        for line in &hunk.lines {
            match line {
                ApplyPatchLine::Context(text) | ApplyPatchLine::Remove(text) => {
                    pattern.push(text.as_str());
                }
                ApplyPatchLine::Add(_) => {}
            }
        }

        let start = if pattern.is_empty() {
            lines.len()
        } else {
            let mut found = None;
            for idx in 0..=lines.len().saturating_sub(pattern.len()) {
                if pattern
                    .iter()
                    .enumerate()
                    .all(|(off, text)| lines.get(idx + off).map(|s| s.as_str()) == Some(*text))
                {
                    found = Some(idx);
                    break;
                }
            }
            found.ok_or_else(|| ZeroBotError::Tool("hunk 未匹配到目标内容".to_string()))?
        };

        let mut replacement = Vec::new();
        let mut cursor = start;
        for line in &hunk.lines {
            match line {
                ApplyPatchLine::Context(text) => {
                    if lines.get(cursor).map(|s| s.as_str()) != Some(text.as_str()) {
                        return Err(ZeroBotError::Tool("hunk 内容与文件不匹配".to_string()));
                    }
                    replacement.push(text.clone());
                    cursor += 1;
                }
                ApplyPatchLine::Remove(text) => {
                    if lines.get(cursor).map(|s| s.as_str()) != Some(text.as_str()) {
                        return Err(ZeroBotError::Tool("hunk 内容与文件不匹配".to_string()));
                    }
                    cursor += 1;
                }
                ApplyPatchLine::Add(text) => replacement.push(text.clone()),
            }
        }

        let end = start + pattern.len();
        lines.splice(start..end, replacement);
    }

    let mut output = lines.join("\n");
    if ends_with_newline {
        output.push('\n');
    }
    if line_ending == "\r\n" {
        output = output.replace('\n', "\r\n");
    }
    Ok(output)
}

fn resolve_relative_path(ctx: &ToolContext, path: &str) -> ZeroBotResult<PathBuf> {
    let input = Path::new(path);
    if input.is_absolute() {
        return Err(ZeroBotError::Tool("补丁路径必须为相对路径".to_string()));
    }
    ctx.resolve_path(path)
}

fn count_diff_changes(diff: &str) -> (usize, usize) {
    let mut additions = 0;
    let mut deletions = 0;
    for line in diff.lines() {
        if line.starts_with("+++") || line.starts_with("---") || line.starts_with("@@") {
            continue;
        }
        if line.starts_with('+') {
            additions += 1;
        } else if line.starts_with('-') {
            deletions += 1;
        }
    }
    (additions, deletions)
}

struct GlobTool;

#[derive(Deserialize)]
struct GlobArgs {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
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
                "pattern": {"type": "string", "description": "glob 模式"},
                "path": {"type": "string", "description": "搜索目录（默认当前工作目录）"}
            },
            "required": ["pattern"]
        })
    }

    async fn run(&self, ctx: &ToolContext, args: JsonValue) -> ZeroBotResult<ToolOutput> {
        let args: GlobArgs = serde_json::from_value(args).map_err(param_error)?;
        let root = if let Some(path) = args.path.as_deref() {
            ctx.resolve_path(path)?
        } else {
            ctx.cwd.clone()
        };
        let pattern = if Path::new(&args.pattern).is_absolute() {
            PathBuf::from(&args.pattern)
        } else {
            root.join(&args.pattern)
        };
        let mut results = Vec::new();
        for entry in glob::glob(pattern.to_string_lossy().as_ref())
            .map_err(|err| ZeroBotError::Tool(format!("glob 模式无效: {err}")))?
        {
            if let Ok(path) = entry {
                if let Ok(meta) = tokio::fs::metadata(&path).await {
                    let mtime = meta.modified().map(system_time_to_ts).unwrap_or_default();
                    results.push((path, mtime));
                }
            }
        }
        results.sort_by(|a, b| b.1.cmp(&a.1));
        let limit = 100usize;
        let truncated = results.len() > limit;
        let slice = if truncated {
            results[..limit].to_vec()
        } else {
            results.clone()
        };
        let output_lines = slice
            .iter()
            .map(|(path, _)| path.to_string_lossy().to_string())
            .collect::<Vec<_>>();
        let summary = if output_lines.is_empty() {
            "No files found".to_string()
        } else if truncated {
            format!("Found {} files (showing first {}).", results.len(), limit)
        } else {
            format!("Found {} files.", results.len())
        };
        let body = format!(
            "<summary>{summary}</summary>\n<results>\n{}\n</results>",
            output_lines.join("\n")
        );
        Ok(ToolOutput::new(body)
            .with_title(
                root.strip_prefix(&ctx.cwd)
                    .unwrap_or(&root)
                    .to_string_lossy()
                    .to_string(),
            )
            .with_metadata(json!({
                "path": root.to_string_lossy(),
                "count": results.len(),
                "truncated": truncated
            })))
    }
}

struct GrepTool;

#[derive(Deserialize)]
struct GrepArgs {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    include: Option<String>,
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
                "pattern": {"type": "string", "description": "要搜索的正则表达式"},
                "path": {"type": "string", "description": "搜索目录（默认当前工作目录）"},
                "include": {"type": "string", "description": "包含的文件模式（如 *.rs）"}
            },
            "required": ["pattern"]
        })
    }

    async fn run(&self, ctx: &ToolContext, args: JsonValue) -> ZeroBotResult<ToolOutput> {
        let args: GrepArgs = serde_json::from_value(args).map_err(param_error)?;
        let root = if let Some(path) = args.path.as_deref() {
            ctx.resolve_path(path)?
        } else {
            ctx.cwd.clone()
        };
        let regex = Regex::new(&args.pattern).map_err(|err| ZeroBotError::Tool(err.to_string()))?;
        if !root.exists() {
            return Err(ZeroBotError::Tool(format!(
                "搜索路径不存在: {}",
                root.display()
            )));
        }

        let include = if let Some(pattern) = args.include.as_deref() {
            Some(
                glob::Pattern::new(pattern)
                    .map_err(|err| ZeroBotError::Tool(format!("include 模式无效: {err}")))?,
            )
        } else {
            None
        };

        let mut matches: HashMap<PathBuf, Vec<(usize, String)>> = HashMap::new();
        let mut total_matches = 0usize;
        for entry in WalkDir::new(&root).into_iter().filter_map(Result::ok) {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            if let Some(pattern) = include.as_ref() {
                let relative = path.strip_prefix(&root).unwrap_or(path);
                if !pattern.matches_path(relative) {
                    continue;
                }
            }
            let content = std::fs::read_to_string(path);
            if let Ok(content) = content {
                for (idx, line) in content.lines().enumerate() {
                    if regex.is_match(line) {
                        total_matches += 1;
                        let entry = matches.entry(path.to_path_buf()).or_default();
                        let mut text = line.to_string();
                        const MAX_LINE_LENGTH: usize = 2000;
                        if text.chars().count() > MAX_LINE_LENGTH {
                            text = text.chars().take(MAX_LINE_LENGTH).collect::<String>() + "...";
                        }
                        entry.push((idx + 1, text));
                    }
                }
            }
        }

        if matches.is_empty() {
            return Ok(
                ToolOutput::new("<summary>No files found</summary>\n<results>\n</results>")
                    .with_title(
                        root.strip_prefix(&ctx.cwd)
                            .unwrap_or(&root)
                            .to_string_lossy()
                            .to_string(),
                    )
                    .with_metadata(json!({
                        "path": root.to_string_lossy(),
                        "matches": 0,
                        "truncated": false
                    })),
            );
        }

        let mut files: Vec<(PathBuf, i64)> = Vec::new();
        for (path, _) in matches.iter() {
            let mtime = std::fs::metadata(path)
                .and_then(|m| m.modified())
                .map(system_time_to_ts)
                .unwrap_or_default();
            files.push((path.clone(), mtime));
        }
        files.sort_by(|a, b| b.1.cmp(&a.1));

        let mut results = Vec::new();
        let mut shown = 0usize;
        let limit = 100usize;
        for (path, _) in files {
            if shown >= limit {
                break;
            }
            results.push(format!("{}:", path.display()));
            if let Some(lines) = matches.get(&path) {
                for (line_no, text) in lines {
                    if shown >= limit {
                        break;
                    }
                    results.push(format!("  Line {}: {}", line_no, text));
                    shown += 1;
                }
            }
            results.push(String::new());
        }
        let truncated = total_matches > shown;
        let summary = if truncated {
            format!("Found {total_matches} matches (showing first {shown}).")
        } else {
            format!("Found {total_matches} matches.")
        };

        let body = format!(
            "<summary>{summary}</summary>\n<results>\n{}\n</results>",
            results.join("\n")
        );
        Ok(ToolOutput::new(body)
            .with_title(
                root.strip_prefix(&ctx.cwd)
                    .unwrap_or(&root)
                    .to_string_lossy()
                    .to_string(),
            )
            .with_metadata(json!({
                "path": root.to_string_lossy(),
                "matches": total_matches,
                "truncated": truncated
            })))
    }
}

struct BashTool;

#[derive(Deserialize)]
struct BashArgs {
    command: String,
    #[serde(default, alias = "dir")]
    workdir: Option<String>,
    #[serde(default, rename = "timeoutMs", alias = "timeout")]
    timeout_ms: Option<u64>,
    #[serde(default)]
    description: Option<String>,
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "执行 bash 命令（支持 workdir/timeoutMs/description）"
    }

    fn parameters(&self) -> JsonValue {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "要执行的命令"},
                "workdir": {"type": "string", "description": "工作目录（默认当前工作目录）"},
                "timeoutMs": {"type": "integer", "description": "超时时间（毫秒，默认 120000）"},
                "description": {"type": "string", "description": "对命令的简要说明"}
            },
            "required": ["command"]
        })
    }

    async fn run(&self, ctx: &ToolContext, args: JsonValue) -> ZeroBotResult<ToolOutput> {
        let args: BashArgs = serde_json::from_value(args).map_err(param_error)?;
        run_bash_command(
            ctx,
            &args.command,
            args.workdir.as_deref(),
            args.timeout_ms,
            args.description.as_deref(),
        )
        .await
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
        "兼容旧版 shell 工具（推荐使用 bash）"
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
        let args: ShellArgs = serde_json::from_value(args).map_err(param_error)?;
        run_bash_command(ctx, &args.command, args.dir.as_deref(), None, None).await
    }
}

async fn run_bash_command(
    ctx: &ToolContext,
    command: &str,
    workdir: Option<&str>,
    timeout_ms: Option<u64>,
    description: Option<&str>,
) -> ZeroBotResult<ToolOutput> {
    let dir = if let Some(dir) = workdir {
        ctx.resolve_path(dir)?
    } else {
        ctx.cwd.clone()
    };
    let timeout_ms = timeout_ms.unwrap_or(120_000);
    let mut plugin_env = HashMap::new();
    if let Some(plugins) = ctx.plugins() {
        let out = plugins
            .run_hook(
                "shell.env",
                json!({
                    "cwd": dir.to_string_lossy(),
                    "session_id": ctx.session_id,
                }),
                json!({ "env": {} }),
            )
            .await?;
        if let Some(map) = out.get("env").and_then(|v| v.as_object()) {
            for (k, v) in map {
                if let Some(value) = v.as_str() {
                    plugin_env.insert(k.clone(), value.to_string());
                }
            }
        }
    }
    let output = timeout(Duration::from_millis(timeout_ms), async {
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-lc").arg(command).current_dir(dir);
        if !plugin_env.is_empty() {
            cmd.envs(plugin_env);
        }
        cmd.output().await
    })
    .await;
    let (stdout, stderr, exit_code) = match output {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let exit_code = output.status.code().unwrap_or(-1);
            (stdout, stderr, exit_code)
        }
        Ok(Err(err)) => {
            return Err(ZeroBotError::Tool(format!("命令执行失败: {err}")));
        }
        Err(_) => {
            let msg = format!("命令执行超时（{} ms）", timeout_ms);
            let body = format!(
                "<command>{}</command>\n<exit_code>-1</exit_code>\n<stdout></stdout>\n<stderr>{}</stderr>",
                command,
                msg
            );
            return Ok(ToolOutput::new(body)
                .with_title("bash")
                .with_metadata(json!({
                    "command": command,
                    "workdir": workdir,
                    "timeout_ms": timeout_ms,
                    "description": description,
                    "exit_code": -1,
                    "ok": false
                })));
        }
    };

    let body = format!(
        "<command>{}</command>\n<exit_code>{}</exit_code>\n<stdout>\n{}\n</stdout>\n<stderr>\n{}\n</stderr>",
        command,
        exit_code,
        stdout.trim_end(),
        stderr.trim_end()
    );
    Ok(ToolOutput::new(body)
        .with_title("bash")
        .with_metadata(json!({
            "command": command,
            "workdir": workdir,
            "timeout_ms": timeout_ms,
            "description": description,
            "exit_code": exit_code,
            "ok": exit_code == 0
        })))
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
        let args: TodoWriteArgs =
            serde_json::from_value(args).map_err(|err| ZeroBotError::Tool(err.to_string()))?;
        let store = ctx
            .store()
            .ok_or_else(|| ZeroBotError::Tool("Todo 工具需要 SessionStore".to_string()))?;
        store.set_todos(&ctx.session_id, &args.todos).await?;
        let content = serde_json::to_string_pretty(&args.todos)
            .map_err(|err| ZeroBotError::Tool(err.to_string()))?;
        Ok(ToolOutput::new(content))
    }
}

struct MessageTool;

#[derive(Deserialize)]
struct MessageArgs {
    content: String,
    #[serde(default)]
    channel: Option<String>,
    #[serde(default)]
    chat_id: Option<String>,
    #[serde(default)]
    message_id: Option<String>,
    #[serde(default)]
    media: Option<Vec<String>>,
}

#[async_trait]
impl Tool for MessageTool {
    fn name(&self) -> &str {
        "message"
    }

    fn description(&self) -> &str {
        "向指定通道发送消息；未显式指定 channel/chat_id 时默认回复当前通道上下文。"
    }

    fn parameters(&self) -> JsonValue {
        serde_json::json!({
            "type": "object",
            "properties": {
                "content": {"type":"string", "description":"消息正文"},
                "channel": {"type":"string", "description":"目标通道，如 feishu"},
                "chat_id": {"type":"string", "description":"目标会话 ID"},
                "message_id": {"type":"string", "description":"可选，目标消息 ID（用于回复）"},
                "media": {"type":"array", "items":{"type":"string"}, "description":"可选，本地文件路径列表"}
            },
            "required": ["content"]
        })
    }

    async fn run(&self, ctx: &ToolContext, args: JsonValue) -> ZeroBotResult<ToolOutput> {
        let args: MessageArgs = serde_json::from_value(args).map_err(param_error)?;
        let sender = ctx
            .outbound
            .as_ref()
            .ok_or_else(|| ZeroBotError::Tool("message 工具仅在 gateway 运行时可用".to_string()))?;

        let route = ctx.route.clone();
        let channel = args
            .channel
            .or_else(|| route.as_ref().map(|r| r.channel.clone()))
            .ok_or_else(|| ZeroBotError::Tool("message 缺少 channel".to_string()))?;
        let chat_id = args
            .chat_id
            .or_else(|| route.as_ref().map(|r| r.chat_id.clone()))
            .ok_or_else(|| ZeroBotError::Tool("message 缺少 chat_id".to_string()))?;
        let message_id = args.message_id.or_else(|| route.and_then(|r| r.message_id));

        let mut metadata = json!({});
        if let Some(mid) = message_id {
            metadata["message_id"] = JsonValue::String(mid);
        }
        let msg = OutboundMessage {
            channel,
            chat_id,
            content: args.content,
            media: args.media.unwrap_or_default(),
            metadata,
        };
        sender
            .send(msg)
            .map_err(|err| ZeroBotError::Tool(format!("message 发送失败: {err}")))?;
        Ok(ToolOutput::new("message queued"))
    }
}

struct CronTool {
    service: Arc<CronService>,
}

impl CronTool {
    fn new(service: Arc<CronService>) -> Self {
        Self { service }
    }
}

#[derive(Deserialize)]
struct CronToolArgs {
    action: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    every_seconds: Option<i64>,
    #[serde(default)]
    cron_expr: Option<String>,
    #[serde(default)]
    tz: Option<String>,
    #[serde(default)]
    at: Option<String>,
    #[serde(default)]
    job_id: Option<String>,
    #[serde(default)]
    deliver: Option<bool>,
    #[serde(default)]
    channel: Option<String>,
    #[serde(default)]
    to: Option<String>,
    #[serde(default)]
    delete_after_run: Option<bool>,
    #[serde(default)]
    force: Option<bool>,
}

#[async_trait]
impl Tool for CronTool {
    fn name(&self) -> &str {
        "cron"
    }

    fn description(&self) -> &str {
        "管理计划任务。actions: add/list/remove/enable/disable/run/status/export"
    }

    fn parameters(&self) -> JsonValue {
        serde_json::json!({
            "type":"object",
            "properties":{
                "action":{"type":"string","enum":["add","list","remove","enable","disable","run","status","export"]},
                "name":{"type":"string"},
                "message":{"type":"string"},
                "every_seconds":{"type":"integer"},
                "cron_expr":{"type":"string"},
                "tz":{"type":"string"},
                "at":{"type":"string","description":"ISO datetime 或 unix 毫秒字符串"},
                "job_id":{"type":"string"},
                "deliver":{"type":"boolean"},
                "channel":{"type":"string"},
                "to":{"type":"string"},
                "delete_after_run":{"type":"boolean"},
                "force":{"type":"boolean"}
            },
            "required":["action"]
        })
    }

    async fn run(&self, ctx: &ToolContext, args: JsonValue) -> ZeroBotResult<ToolOutput> {
        let args: CronToolArgs = serde_json::from_value(args).map_err(param_error)?;
        match args.action.as_str() {
            "add" => {
                let message = args
                    .message
                    .ok_or_else(|| ZeroBotError::Tool("cron add 缺少 message".to_string()))?;
                let schedule = if let Some(sec) = args.every_seconds {
                    CronSchedule {
                        kind: CronScheduleKind::Every,
                        at_ms: None,
                        every_ms: Some(sec.saturating_mul(1000)),
                        expr: None,
                        tz: None,
                    }
                } else if let Some(expr) = args.cron_expr {
                    CronSchedule {
                        kind: CronScheduleKind::Cron,
                        at_ms: None,
                        every_ms: None,
                        expr: Some(expr),
                        tz: args.tz,
                    }
                } else if let Some(at) = args.at {
                    let at_ms = parse_at_to_millis(&at)?;
                    CronSchedule {
                        kind: CronScheduleKind::At,
                        at_ms: Some(at_ms),
                        every_ms: None,
                        expr: None,
                        tz: None,
                    }
                } else {
                    return Err(ZeroBotError::Tool(
                        "cron add 需要 every_seconds / cron_expr / at 之一".to_string(),
                    ));
                };

                let route = ctx.route.clone();
                let payload = CronPayload {
                    kind: "agent_turn".to_string(),
                    message: message.clone(),
                    deliver: args.deliver.unwrap_or(false),
                    channel: args
                        .channel
                        .or_else(|| route.as_ref().map(|r| r.channel.clone())),
                    to: args
                        .to
                        .or_else(|| route.as_ref().map(|r| r.chat_id.clone())),
                };
                let name = args
                    .name
                    .unwrap_or_else(|| message.chars().take(30).collect::<String>());
                let job = self
                    .service
                    .add_job(
                        name,
                        schedule,
                        payload,
                        args.delete_after_run.unwrap_or(false),
                    )
                    .await?;
                let rendered = serde_json::to_string_pretty(&job)
                    .map_err(|err| ZeroBotError::Tool(err.to_string()))?;
                Ok(ToolOutput::new(rendered))
            }
            "list" => {
                let jobs = self.service.list_jobs(false).await?;
                let rendered = serde_json::to_string_pretty(&jobs)
                    .map_err(|err| ZeroBotError::Tool(err.to_string()))?;
                Ok(ToolOutput::new(rendered))
            }
            "remove" => {
                let id = args
                    .job_id
                    .ok_or_else(|| ZeroBotError::Tool("cron remove 缺少 job_id".to_string()))?;
                let removed = self.service.remove_job(&id).await?;
                Ok(ToolOutput::new(if removed {
                    "removed"
                } else {
                    "not_found"
                }))
            }
            "enable" => {
                let id = args
                    .job_id
                    .ok_or_else(|| ZeroBotError::Tool("cron enable 缺少 job_id".to_string()))?;
                let job = self.service.enable_job(&id, true).await?;
                let rendered = serde_json::to_string_pretty(&job)
                    .map_err(|err| ZeroBotError::Tool(err.to_string()))?;
                Ok(ToolOutput::new(rendered))
            }
            "disable" => {
                let id = args
                    .job_id
                    .ok_or_else(|| ZeroBotError::Tool("cron disable 缺少 job_id".to_string()))?;
                let job = self.service.enable_job(&id, false).await?;
                let rendered = serde_json::to_string_pretty(&job)
                    .map_err(|err| ZeroBotError::Tool(err.to_string()))?;
                Ok(ToolOutput::new(rendered))
            }
            "run" => {
                let id = args
                    .job_id
                    .ok_or_else(|| ZeroBotError::Tool("cron run 缺少 job_id".to_string()))?;
                let ok = self
                    .service
                    .run_job(&id, args.force.unwrap_or(false))
                    .await?;
                Ok(ToolOutput::new(if ok {
                    "ok"
                } else {
                    "not_found_or_disabled"
                }))
            }
            "status" => {
                let status = self.service.status().await?;
                let rendered = serde_json::to_string_pretty(&status)
                    .map_err(|err| ZeroBotError::Tool(err.to_string()))?;
                Ok(ToolOutput::new(rendered))
            }
            "export" => {
                let path = self.service.export_snapshot().await?;
                let body = path
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|| "disabled".to_string());
                Ok(ToolOutput::new(body))
            }
            _ => Err(ZeroBotError::Tool("未知 cron action".to_string())),
        }
    }
}

fn parse_at_to_millis(value: &str) -> ZeroBotResult<i64> {
    if let Ok(ms) = value.parse::<i64>() {
        return Ok(ms);
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(value) {
        return Ok(dt.timestamp_millis());
    }
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S") {
        let local = chrono::Local
            .from_local_datetime(&dt)
            .single()
            .ok_or_else(|| ZeroBotError::Tool("at 时间非法".to_string()))?;
        return Ok(local.timestamp_millis());
    }
    Err(ZeroBotError::Tool(
        "at 格式非法，支持 RFC3339 或毫秒时间戳".to_string(),
    ))
}

struct RequestUserInputTool;

#[derive(Deserialize)]
struct RequestUserInputArgs {
    id: String,
    #[serde(default)]
    title: Option<String>,
    questions: Vec<RequestUserInputQuestionArgs>,
}

#[derive(Deserialize)]
struct RequestUserInputQuestionArgs {
    id: String,
    prompt: String,
    #[serde(default)]
    options: Option<Vec<RequestUserInputOptionArgs>>,
}

#[derive(Deserialize)]
struct RequestUserInputOptionArgs {
    id: String,
    label: String,
}

#[async_trait]
impl Tool for RequestUserInputTool {
    fn name(&self) -> &str {
        "request_user_input"
    }

    fn description(&self) -> &str {
        "询问用户并收集结构化输入（支持多问题与选项）"
    }

    fn parameters(&self) -> JsonValue {
        serde_json::json!({
            "type": "object",
            "properties": {
                "id": {"type": "string", "description": "请求 ID"},
                "title": {"type": "string", "description": "可选标题"},
                "questions": {
                    "type": "array",
                    "description": "问题列表",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": {"type": "string", "description": "问题 ID"},
                            "prompt": {"type": "string", "description": "问题内容"},
                            "options": {
                                "type": "array",
                                "description": "可选项",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "id": {"type": "string"},
                                        "label": {"type": "string"}
                                    },
                                    "required": ["id", "label"]
                                }
                            }
                        },
                        "required": ["id", "prompt"]
                    }
                }
            },
            "required": ["id", "questions"]
        })
    }

    async fn run(&self, ctx: &ToolContext, args: JsonValue) -> ZeroBotResult<ToolOutput> {
        let args: RequestUserInputArgs =
            serde_json::from_value(args).map_err(|err| ZeroBotError::Tool(err.to_string()))?;
        let interaction = ctx
            .interaction()
            .ok_or_else(|| ZeroBotError::Tool("需要用户输入，但当前无交互处理器".to_string()))?;
        let request = UserInputRequest {
            id: args.id,
            title: args.title,
            questions: args
                .questions
                .into_iter()
                .map(|q| crate::interaction::UserInputQuestion {
                    id: q.id,
                    prompt: q.prompt,
                    options: q.options.map(|opts| {
                        opts.into_iter()
                            .map(|opt| crate::interaction::UserInputOption {
                                id: opt.id,
                                label: opt.label,
                            })
                            .collect()
                    }),
                })
                .collect(),
        };
        let response: UserInputResponse = interaction.request_user_input(request).await?;
        let content =
            serde_json::to_string(&response).map_err(|err| ZeroBotError::Tool(err.to_string()))?;
        Ok(ToolOutput::new(content))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interaction::{InteractionHandler, UserInputAnswer};
    use crate::provider::{Provider, ProviderFactory, ProviderRequest, ProviderResponse};
    use crate::session::{SessionKind, SqliteSessionStore};
    use async_trait::async_trait;
    use httpmock::Method::POST;
    use httpmock::MockServer;
    use pretty_assertions::assert_eq;
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;
    use tempfile::TempDir;

    fn extract_tag(content: &str, tag: &str) -> Option<String> {
        let start_tag = format!("<{tag}>");
        let end_tag = format!("</{tag}>");
        let start = content.find(&start_tag)? + start_tag.len();
        let end = content[start..].find(&end_tag)? + start;
        Some(content[start..end].to_string())
    }

    #[tokio::test]
    async fn registry_runs_builtin_tool() {
        let dir = TempDir::new().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf(), vec![], "s1", None, None);
        let registry = ToolRegistry::with_builtin();
        let args = serde_json::json!({"path": "test.txt", "content": "hi"});
        let output = registry.run(&ctx, "write", args).await.unwrap();
        assert!(output.content.contains("<action>write</action>"));
    }

    #[tokio::test]
    async fn truncates_tool_output_without_persisting() {
        let settings = ToolOutputSettings {
            max_lines: 2,
            max_bytes: 16,
            direction: ToolOutputDirection::Head,
        };
        let dir = TempDir::new().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf(), vec![], "s1", None, None);
        let content = "line1\nline2\nline3\nline4";
        let output = truncate_tool_content(&ctx, content, &settings)
            .await
            .unwrap();
        assert!(output.truncated);
        assert!(output
            .summary
            .unwrap_or_default()
            .contains("输出过长已截断"));
        let path = output.output_path.unwrap();
        assert!(tokio::fs::metadata(path).await.is_ok());
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
        let registry =
            ToolRegistry::with_builtin_async(&settings, &std::path::PathBuf::from("."), None, None)
                .await
                .unwrap();
        let tool_name = "mcp__remote-one__echo";
        assert!(registry.get(tool_name).is_some());

        let ctx = ToolContext::new(std::path::PathBuf::from("."), vec![], "s1", None, None);
        let output = registry
            .run(&ctx, tool_name, serde_json::json!({"text":"hi"}))
            .await
            .unwrap();
        let inner = extract_tag(&output.content, "content").unwrap_or_default();
        assert!(inner.contains("ok"));
    }

    #[tokio::test]
    async fn todo_tools_read_write() {
        let dir = TempDir::new().unwrap();
        let store = SqliteSessionStore::new(dir.path().join("test.db"))
            .await
            .unwrap();
        store.init().await.unwrap();
        let session = store.create_session("test".to_string()).await.unwrap();
        let store = Arc::new(store);

        let ctx = ToolContext::new(
            dir.path().to_path_buf(),
            vec![],
            session.id.clone(),
            Some(store),
            None,
        );
        let registry = ToolRegistry::with_builtin();

        let write_args = serde_json::json!({
            "todos": [
                {"content": "第一步", "status": "pending", "priority": "high"},
                {"content": "第二步", "status": "in_progress", "priority": "medium"}
            ]
        });
        let output = registry.run(&ctx, "todowrite", write_args).await.unwrap();
        let inner = extract_tag(&output.content, "content").unwrap_or_default();
        assert!(inner.contains("第一步"));

        let output = registry
            .run(&ctx, "todoread", serde_json::json!({}))
            .await
            .unwrap();
        let inner = extract_tag(&output.content, "content").unwrap_or_default();
        assert!(inner.contains("第二步"));
    }

    #[tokio::test]
    async fn todo_tools_reject_invalid_status() {
        let dir = TempDir::new().unwrap();
        let store = SqliteSessionStore::new(dir.path().join("test.db"))
            .await
            .unwrap();
        store.init().await.unwrap();
        let session = store.create_session("test".to_string()).await.unwrap();
        let store = Arc::new(store);

        let ctx = ToolContext::new(
            dir.path().to_path_buf(),
            vec![],
            session.id.clone(),
            Some(store),
            None,
        );
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
    async fn write_requires_prior_read() {
        let dir = TempDir::new().unwrap();
        let store = SqliteSessionStore::new(dir.path().join("test.db"))
            .await
            .unwrap();
        store.init().await.unwrap();
        let session = store.create_session("test".to_string()).await.unwrap();
        let store = Arc::new(store);

        let file_path = dir.path().join("demo.txt");
        std::fs::write(&file_path, "hello").unwrap();

        let ctx = ToolContext::new(
            dir.path().to_path_buf(),
            vec![],
            session.id.clone(),
            Some(store),
            None,
        );
        let registry = ToolRegistry::with_builtin();

        let write_args = serde_json::json!({"filePath": "demo.txt", "content": "hi"});
        let result = registry.run(&ctx, "write", write_args).await;
        assert!(result.is_err());

        let _ = registry
            .run(&ctx, "read", serde_json::json!({"filePath": "demo.txt"}))
            .await
            .unwrap();

        let write_args = serde_json::json!({"filePath": "demo.txt", "content": "hi"});
        let result = registry.run(&ctx, "write", write_args).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn skill_tool_loads_content_block() {
        let dir = TempDir::new().unwrap();
        let skill_dir = dir.path().join(".zerobot/skills/demo");
        std::fs::create_dir_all(skill_dir.join("scripts")).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            r#"---
name: demo
description: demo skill
---

# Demo

Use demo skill.
"#,
        )
        .unwrap();
        std::fs::write(skill_dir.join("scripts/helper.txt"), "ok").unwrap();

        let mut settings = Settings::default();
        settings.skills.enabled = true;
        let registry = ToolRegistry::with_builtin_async(&settings, dir.path(), None, None)
            .await
            .unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf(), vec![], "s1", None, None);
        let output = registry
            .run(&ctx, "skill", serde_json::json!({"name": "demo"}))
            .await
            .unwrap();
        assert_eq!(output.title.as_deref(), Some("Loaded skill: demo"));
        assert!(output.content.contains("<skill_content name=\"demo\">"));
        assert!(output.content.contains("Base directory for this skill:"));
        assert!(output.content.contains("<skill_files>"));
        assert!(output.content.contains("<file>"));
    }

    #[tokio::test]
    async fn skill_tool_reports_available_skills_when_not_found() {
        let dir = TempDir::new().unwrap();
        let skill_dir = dir.path().join(".zerobot/skills/demo");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            r#"---
name: demo
description: demo skill
---

content
"#,
        )
        .unwrap();

        let mut settings = Settings::default();
        settings.skills.enabled = true;
        let registry = ToolRegistry::with_builtin_async(&settings, dir.path(), None, None)
            .await
            .unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf(), vec![], "s1", None, None);
        let err = registry
            .run(&ctx, "skill", serde_json::json!({"name": "missing"}))
            .await
            .unwrap_err();
        let text = err.to_string();
        assert!(text.contains("未找到"));
        assert!(text.contains("demo"));
    }

    struct MockInteraction {
        response: UserInputResponse,
    }

    #[async_trait]
    impl InteractionHandler for MockInteraction {
        async fn request_user_input(
            &self,
            _request: UserInputRequest,
        ) -> ZeroBotResult<UserInputResponse> {
            Ok(self.response.clone())
        }

        async fn request_tool_approval(
            &self,
            _request: crate::interaction::ToolApprovalRequest,
        ) -> ZeroBotResult<crate::interaction::ToolApprovalResponse> {
            Ok(crate::interaction::ToolApprovalResponse {
                decision: crate::interaction::ToolApprovalDecision::AllowOnce,
            })
        }
    }

    #[tokio::test]
    async fn request_user_input_tool_returns_json() {
        let dir = TempDir::new().unwrap();
        let response = UserInputResponse {
            answers: HashMap::from([(
                "q1".to_string(),
                UserInputAnswer {
                    option_id: Some("a".to_string()),
                    note: Some("note".to_string()),
                },
            )]),
            cancelled: false,
        };
        let interaction = Arc::new(MockInteraction {
            response: response.clone(),
        });
        let ctx = ToolContext::new(
            dir.path().to_path_buf(),
            vec![],
            "s1",
            None,
            Some(interaction),
        );
        let registry = ToolRegistry::with_builtin();
        let args = serde_json::json!({
            "id": "req1",
            "questions": [
                {
                    "id": "q1",
                    "prompt": "question",
                    "options": [
                        {"id": "a", "label": "A"}
                    ]
                }
            ]
        });
        let output = registry
            .run(&ctx, "request_user_input", args)
            .await
            .unwrap();
        let inner = extract_tag(&output.content, "content").unwrap_or_default();
        let parsed: UserInputResponse = serde_json::from_str(&inner).unwrap();
        assert_eq!(parsed, response);
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
                    usage: None,
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

        let store = SqliteSessionStore::new(dir.path().join("test.db"))
            .await
            .unwrap();
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
            None,
            Arc::new(RwLock::new(HashSet::new())),
        ));

        let ctx = ToolContext::new(
            dir.path().to_path_buf(),
            vec![],
            parent.id.clone(),
            None,
            None,
        );
        let output = registry
            .run(
                &ctx,
                "subagent",
                serde_json::json!({"name":"demo","prompt":"hi"}),
            )
            .await
            .unwrap();
        let inner = extract_tag(&output.content, "content").unwrap_or_default();
        assert!(inner.contains("子代理输出"));
    }
}
