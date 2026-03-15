use crate::config::Settings;
use crate::error::{ZeroBotError, ZeroBotResult};
use crate::hooks::HookDefinition;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Debug, Clone)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    pub hooks: Vec<HookDefinition>,
    pub path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct SkillContent {
    pub info: SkillInfo,
    pub body: String,
}

pub struct SkillManager {
    roots: Vec<PathBuf>,
}

impl SkillManager {
    pub fn new(settings: &Settings, cwd: &Path) -> Self {
        let mut roots = Vec::new();
        roots.push(expand_home("~/.zerobot/skills"));
        roots.push(cwd.join(".zerobot").join("skills"));
        for path in &settings.skills.paths {
            roots.push(expand_home(path));
        }
        Self { roots }
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    pub fn discover(&self) -> ZeroBotResult<Vec<SkillInfo>> {
        let mut out = Vec::new();
        for root in &self.roots {
            if !root.exists() {
                continue;
            }
            for entry in WalkDir::new(root).into_iter().filter_map(Result::ok) {
                if !entry.file_type().is_file() {
                    continue;
                }
                if entry.file_name() != "SKILL.md" {
                    continue;
                }
                let content = std::fs::read_to_string(entry.path())?;
                if let Ok((meta, _body)) = parse_skill_file(&content) {
                    out.push(SkillInfo {
                        name: meta.name,
                        description: meta.description,
                        hooks: meta.hooks.unwrap_or_default(),
                        path: entry.path().to_path_buf(),
                    });
                }
            }
        }
        Ok(out)
    }

    pub fn load(&self, name: &str) -> ZeroBotResult<SkillContent> {
        let skills = self.discover()?;
        let info = skills
            .into_iter()
            .find(|skill| skill.name == name)
            .ok_or_else(|| ZeroBotError::Skill(format!("未找到 skill: {name}")))?;
        let content = std::fs::read_to_string(&info.path)?;
        let (_meta, body) = parse_skill_file(&content)?;
        Ok(SkillContent { info, body })
    }
}

#[derive(Debug, Deserialize)]
struct SkillFrontmatter {
    name: String,
    description: String,
    #[allow(dead_code)]
    #[serde(default)]
    metadata: HashMap<String, serde_yaml::Value>,
    #[serde(default)]
    hooks: Option<Vec<HookDefinition>>,
}

fn parse_skill_file(input: &str) -> ZeroBotResult<(SkillFrontmatter, String)> {
    let mut lines = input.lines();
    let first = lines.next().unwrap_or("");
    if first.trim() != "---" {
        return Err(ZeroBotError::Skill("SKILL.md 缺少 frontmatter".to_string()));
    }
    let mut yaml_lines = Vec::new();
    for line in lines.by_ref() {
        if line.trim() == "---" {
            break;
        }
        yaml_lines.push(line);
    }
    let yaml = yaml_lines.join("\n");
    let meta: SkillFrontmatter = serde_yaml::from_str(&yaml)
        .map_err(|err| ZeroBotError::Skill(format!("SKILL.md frontmatter 解析失败: {err}")))?;
    let body = lines.collect::<Vec<_>>().join("\n").trim().to_string();
    Ok((meta, body))
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

pub fn format_skill_index(skills: &[SkillInfo]) -> String {
    if skills.is_empty() {
        return "当前未发现可用的 Skill。".to_string();
    }
    let mut lines = Vec::new();
    lines.push("可用 Skill 列表：".to_string());
    for skill in skills {
        lines.push(format!("- {}：{}", skill.name, skill.description));
    }
    lines.push("当需要某个 Skill 时，调用 skill 工具加载具体内容。".to_string());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn parses_skill_frontmatter() {
        let content = r#"---
name: demo
description: 示例
---

内容
"#;
        let (meta, body) = parse_skill_file(content).unwrap();
        assert_eq!(meta.name, "demo");
        assert_eq!(meta.description, "示例");
        assert_eq!(body, "内容");
    }

    #[test]
    fn discover_and_load_skills() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("skills/demo");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("SKILL.md"),
            r#"---
name: demo
description: 示例
---

内容
"#,
        )
        .unwrap();

        let mut settings = Settings::default();
        settings.skills.paths = vec![dir.path().join("skills").to_string_lossy().to_string()];
        let manager = SkillManager::new(&settings, Path::new("."));
        let skills = manager.discover().unwrap();
        assert_eq!(skills.len(), 1);
        let loaded = manager.load("demo").unwrap();
        assert_eq!(loaded.body, "内容");
    }
}
