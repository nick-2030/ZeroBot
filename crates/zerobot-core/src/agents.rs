use crate::error::{ZeroBotError, ZeroBotResult};
use crate::hooks::HookDefinition;
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct AgentDefinition {
    pub name: String,
    pub description: String,
    pub model: Option<String>,
    pub tools: Option<Vec<String>>,
    pub hooks: Vec<HookDefinition>,
    pub path: PathBuf,
    pub body: String,
}

pub struct AgentManager {
    roots: Vec<PathBuf>,
}

impl AgentManager {
    pub fn new(cwd: &Path) -> Self {
        let mut roots = Vec::new();
        roots.push(cwd.join(".zerobot").join("agents"));
        roots.push(expand_home("~/.zerobot/agents"));
        Self { roots }
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    pub fn discover(&self) -> ZeroBotResult<Vec<AgentDefinition>> {
        let mut out = Vec::new();
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
                    out.push(AgentDefinition {
                        name: meta.name,
                        description: meta.description,
                        model: normalize_model(meta.model),
                        tools: meta.tools.map(normalize_tools),
                        hooks: meta.hooks.unwrap_or_default(),
                        path: path.clone(),
                        body,
                    });
                }
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
                        });
                    }
                }
            }
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
        lines.push(format!("- {}：{}", agent.name, agent.description));
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
        assert_eq!(agents.len(), 1);
        let loaded = manager.load("demo").unwrap();
        assert_eq!(loaded.name, "demo");
        assert_eq!(loaded.description, "示例");
        assert_eq!(loaded.body, "内容");
    }
}
