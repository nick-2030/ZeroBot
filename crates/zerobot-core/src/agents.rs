use crate::agent_dispatch::IsolationMode;
use crate::error::{ZeroBotError, ZeroBotResult};
use crate::hooks::HookDefinition;
use serde::Deserialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Agent 角色
#[derive(Debug, Clone, Default)]
pub enum AgentRole {
    /// 普通 worker：执行具体任务
    #[default]
    Worker,
    /// Coordinator：编排多个 worker
    Coordinator,
    /// Orchestrator：可递归派发的编排器
    Orchestrator { max_depth: u32 },
}

#[derive(Debug, Clone)]
pub struct AgentDefinition {
    pub name: String,
    pub description: String,
    pub model: Option<String>,
    pub tools: Option<Vec<String>>,
    pub hooks: Vec<HookDefinition>,
    pub path: PathBuf,
    pub body: String,
    /// Agent 角色
    pub role: AgentRole,
    /// 工具集限制
    pub toolsets: Option<Vec<String>>,
    /// 最大迭代轮次
    pub max_turns: Option<u32>,
    /// 是否默认后台运行
    pub background: bool,
    /// 是否跳过 CLAUDE.md 等上下文
    pub omit_context: bool,
    /// 隔离模式
    pub isolation: Option<IsolationMode>,
}

pub struct AgentManager {
    roots: Vec<PathBuf>,
}

impl AgentManager {
    pub fn new(cwd: &Path) -> Self {
        let roots = vec![
            cwd.join(".zerobot").join("agents"),
            expand_home("~/.zerobot/agents"),
        ];
        Self { roots }
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    pub fn discover(&self) -> ZeroBotResult<Vec<AgentDefinition>> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for root in &self.roots {
            if !root.exists() {
                continue;
            }
            for entry in std::fs::read_dir(root)? {
                let entry = entry?;
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }
                if path.extension().and_then(|s| s.to_str()) != Some("md") {
                    continue;
                }
                let content = std::fs::read_to_string(&path)?;
                if let Ok((meta, body)) = parse_agent_file(&content) {
                    seen.insert(meta.name.clone());
                    out.push(AgentDefinition {
                        name: meta.name,
                        description: meta.description,
                        model: normalize_model(meta.model),
                        tools: meta.tools.map(normalize_tools),
                        hooks: meta.hooks.unwrap_or_default(),
                        path: path.clone(),
                        body,
                        role: match meta.role.as_deref() {
                            Some("coordinator") => AgentRole::Coordinator,
                            Some("orchestrator") => AgentRole::Orchestrator { max_depth: 3 },
                            _ => AgentRole::Worker,
                        },
                        toolsets: meta.toolsets,
                        max_turns: meta.max_turns,
                        background: meta.background.unwrap_or(false),
                        omit_context: meta.omit_context.unwrap_or(false),
                        isolation: match meta.isolation.as_deref() {
                            Some("worktree") => Some(IsolationMode::Worktree),
                            Some("remote") => Some(IsolationMode::Remote),
                            _ => None,
                        },
                    });
                }
            }
        }
        for builtin in builtin_agents() {
            if !seen.contains(&builtin.name) {
                out.push(builtin);
            }
        }
        Ok(out)
    }

    pub fn load(&self, name: &str) -> ZeroBotResult<AgentDefinition> {
        for root in &self.roots {
            if !root.exists() {
                continue;
            }
            for entry in std::fs::read_dir(root)? {
                let entry = entry?;
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }
                if path.extension().and_then(|s| s.to_str()) != Some("md") {
                    continue;
                }
                let content = std::fs::read_to_string(&path)?;
                if let Ok((meta, body)) = parse_agent_file(&content) {
                    if meta.name == name {
                        return Ok(AgentDefinition {
                            name: meta.name,
                            description: meta.description,
                            model: normalize_model(meta.model),
                            tools: meta.tools.map(normalize_tools),
                            hooks: meta.hooks.unwrap_or_default(),
                            path,
                            body,
                            role: match meta.role.as_deref() {
                                Some("coordinator") => AgentRole::Coordinator,
                                Some("orchestrator") => AgentRole::Orchestrator { max_depth: 3 },
                                _ => AgentRole::Worker,
                            },
                            toolsets: meta.toolsets,
                            max_turns: meta.max_turns,
                            background: meta.background.unwrap_or(false),
                            omit_context: meta.omit_context.unwrap_or(false),
                            isolation: match meta.isolation.as_deref() {
                                Some("worktree") => Some(IsolationMode::Worktree),
                                Some("remote") => Some(IsolationMode::Remote),
                                _ => None,
                            },
                        });
                    }
                }
            }
        }

        if let Some(builtin) = builtin_agent(name) {
            return Ok(builtin);
        }

        Err(ZeroBotError::Agent(format!("未找到子代理定义: {name}")))
    }
}

#[derive(Debug, Deserialize)]
struct AgentFrontmatter {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    tools: Option<ToolsField>,
    #[serde(default)]
    hooks: Option<Vec<HookDefinition>>,
    #[serde(default)]
    pub role: Option<String>,
    pub toolsets: Option<Vec<String>>,
    pub max_turns: Option<u32>,
    #[serde(default)]
    pub background: Option<bool>,
    #[serde(default)]
    pub omit_context: Option<bool>,
    pub isolation: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ToolsField {
    String(String),
    List(Vec<String>),
}

fn parse_agent_file(input: &str) -> ZeroBotResult<(AgentFrontmatter, String)> {
    let mut lines = input.lines();
    let first = lines.next().unwrap_or("");
    if first.trim() != "---" {
        return Err(ZeroBotError::Agent("代理定义缺少 frontmatter".to_string()));
    }
    let mut yaml_lines = Vec::new();
    for line in lines.by_ref() {
        if line.trim() == "---" {
            break;
        }
        yaml_lines.push(line);
    }
    let yaml = yaml_lines.join("\n");
    let meta: AgentFrontmatter = serde_yaml::from_str(&yaml)
        .map_err(|err| ZeroBotError::Agent(format!("代理定义 frontmatter 解析失败: {err}")))?;
    if meta.description.trim().is_empty() {
        return Err(ZeroBotError::Agent("代理定义缺少 description".to_string()));
    }
    let body = lines.collect::<Vec<_>>().join("\n").trim().to_string();
    Ok((meta, body))
}

fn normalize_model(model: Option<String>) -> Option<String> {
    match model.as_deref() {
        Some("inherit") | Some("INHERIT") => None,
        _ => model,
    }
}

fn normalize_tools(field: ToolsField) -> Vec<String> {
    match field {
        ToolsField::String(raw) => raw
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect(),
        ToolsField::List(list) => list
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
    }
}

pub fn format_agent_index(agents: &[AgentDefinition]) -> String {
    if agents.is_empty() {
        return "当前未发现可用的子代理。".to_string();
    }
    let mut lines = Vec::new();
    lines.push("可用子代理列表：".to_string());
    for agent in agents {
        let name = if is_builtin_path(&agent.path) {
            format!("{} (built-in)", agent.name)
        } else {
            agent.name.clone()
        };
        lines.push(format!("- {}：{}", name, agent.description));
    }
    lines.push("需要时调用 subagent 工具，传入 name 和 prompt。".to_string());
    lines.join("\n")
}

fn expand_home(path: &str) -> PathBuf {
    if path == "~" {
        return home_dir();
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return home_dir().join(rest);
    }
    PathBuf::from(path)
}

fn home_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home);
    }
    if let Ok(home) = std::env::var("USERPROFILE") {
        return PathBuf::from(home);
    }
    PathBuf::from(".")
}

fn is_builtin_path(path: &Path) -> bool {
    path.to_string_lossy().starts_with("<builtin:")
}

fn builtin_agents() -> Vec<AgentDefinition> {
    vec![
        AgentDefinition {
            name: "plan".to_string(),
            description: "规划模式：只读探索并输出结构化实现计划".to_string(),
            model: None,
            tools: Some(vec![
                "read".to_string(),
                "glob".to_string(),
                "grep".to_string(),
                "bash".to_string(),
            ]),
            hooks: Vec::new(),
            path: PathBuf::from("<builtin:plan>"),
            body: include_str!("../prompts/modes/plan.md").trim().to_string(),
            role: AgentRole::Worker,
            toolsets: None,
            max_turns: None,
            background: false,
            omit_context: false,
            isolation: None,
        },
        AgentDefinition {
            name: "review".to_string(),
            description: "审查模式：验证实现并找出风险与缺口".to_string(),
            model: None,
            tools: Some(vec![
                "read".to_string(),
                "glob".to_string(),
                "grep".to_string(),
                "bash".to_string(),
            ]),
            hooks: Vec::new(),
            path: PathBuf::from("<builtin:review>"),
            body: include_str!("../prompts/modes/review.md")
                .trim()
                .to_string(),
            role: AgentRole::Worker,
            toolsets: None,
            max_turns: None,
            background: false,
            omit_context: false,
            isolation: None,
        },
        AgentDefinition {
            name: "execute".to_string(),
            description: "执行模式：按系统提示词实现需求并交付改动".to_string(),
            model: None,
            tools: None,
            hooks: Vec::new(),
            path: PathBuf::from("<builtin:execute>"),
            body: include_str!("../prompts/modes/execute.md")
                .trim()
                .to_string(),
            role: AgentRole::Worker,
            toolsets: None,
            max_turns: None,
            background: false,
            omit_context: false,
            isolation: None,
        },
        AgentDefinition {
            name: "coordinator".to_string(),
            description: "编排多个 worker agent 完成复杂任务".to_string(),
            model: None,
            tools: Some(vec![
                "agent".to_string(),
                "send_message".to_string(),
                "todo_read".to_string(),
                "todo_write".to_string(),
                "read".to_string(),
            ]),
            hooks: Vec::new(),
            path: PathBuf::from("<builtin:coordinator>"),
            body: include_str!("../prompts/modes/coordinator.md")
                .trim()
                .to_string(),
            role: AgentRole::Coordinator,
            toolsets: None,
            max_turns: None,
            background: false,
            omit_context: false,
            isolation: None,
        },
    ]
}

fn builtin_agent(name: &str) -> Option<AgentDefinition> {
    builtin_agents()
        .into_iter()
        .find(|agent| agent.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn parses_agent_frontmatter() {
        let content = r#"---
name: demo
description: 示例
model: test-model
---

内容
"#;
        let (meta, body) = parse_agent_file(content).unwrap();
        assert_eq!(meta.name, "demo");
        assert_eq!(meta.description, "示例");
        assert_eq!(meta.model, Some("test-model".to_string()));
        assert_eq!(body, "内容");
    }

    #[test]
    fn discover_and_load_agents() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join(".zerobot/agents");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("demo.md"),
            r#"---
name: demo
description: 示例
---

内容
"#,
        )
        .unwrap();

        let manager = AgentManager::new(dir.path());
        let agents = manager.discover().unwrap();
        assert!(agents.iter().any(|agent| agent.name == "demo"));
        assert!(agents.iter().any(|agent| agent.name == "plan"));
        let loaded = manager.load("demo").unwrap();
        assert_eq!(loaded.name, "demo");
        assert_eq!(loaded.description, "示例");
        assert_eq!(loaded.body, "内容");
    }

    #[test]
    fn builtin_agents_available_when_missing() {
        let dir = TempDir::new().unwrap();
        let manager = AgentManager::new(dir.path());
        let loaded = manager.load("plan").unwrap();
        assert_eq!(loaded.name, "plan");
        assert!(is_builtin_path(&loaded.path));
    }

    #[test]
    fn user_agents_override_builtin() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join(".zerobot/agents");
        std::fs::create_dir_all(&root).unwrap();
        let plan_path = root.join("plan.md");
        std::fs::write(
            &plan_path,
            r#"---
name: plan
description: 自定义计划
---

用户定义
"#,
        )
        .unwrap();

        let manager = AgentManager::new(dir.path());
        let agents = manager.discover().unwrap();
        let plan = agents.iter().find(|agent| agent.name == "plan").unwrap();
        assert_eq!(plan.description, "自定义计划");
        assert_eq!(plan.path, plan_path);
    }
}
