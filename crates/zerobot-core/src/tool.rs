use crate::config::{ToolOutputDirection, ToolOutputSettings};
use crate::error::{ZeroBotError, ZeroBotResult};
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

#[derive(Debug, Clone)]
pub struct ToolContext {
    pub cwd: PathBuf,
    pub allow_paths: Vec<PathBuf>,
}

impl ToolContext {
    pub fn new(cwd: PathBuf, allow_paths: Vec<PathBuf>) -> Self {
        Self { cwd, allow_paths }
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
        registry
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

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    #[tokio::test]
    async fn registry_runs_builtin_tool() {
        let dir = TempDir::new().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf(), vec![]);
        let registry = ToolRegistry::with_builtin();
        let args = serde_json::json!({"path": "test.txt", "content": "hi"});
        let output = registry.run(&ctx, "write", args).await.unwrap();
        assert_eq!(output.content, "写入完成");
    }

    #[tokio::test]
    async fn truncates_tool_output_without_persisting() {
        let _dir = TempDir::new().unwrap();
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
}
