use crate::config::Settings;
use crate::error::{ZeroBotError, ZeroBotResult};
use crate::workspace::resolve_workspace_root;
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use url::Url;
use walkdir::WalkDir;

#[derive(Debug, Clone)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
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
        let workspace_root = resolve_workspace_root(cwd);
        let home = home_dir();
        let mut roots = Vec::new();

        // Claude/Agents compatibility (global)
        roots.push(home.join(".claude").join("skills"));
        roots.push(home.join(".agents").join("skills"));
        // ZeroBot native roots (global)
        push_zerobot_skill_roots(&mut roots, &home.join(".zerobot"));

        // Claude/Agents compatibility (project and ancestors)
        let mut cursor = cwd;
        loop {
            roots.push(cursor.join(".claude").join("skills"));
            roots.push(cursor.join(".agents").join("skills"));
            if cursor == workspace_root {
                break;
            }
            let Some(parent) = cursor.parent() else {
                break;
            };
            cursor = parent;
        }

        // ZeroBot native roots (workspace)
        push_zerobot_skill_roots(&mut roots, &workspace_root.join(".zerobot"));

        for path in &settings.skills.paths {
            let expanded = expand_home(path);
            if expanded.is_absolute() {
                roots.push(expanded);
            } else {
                roots.push(cwd.join(expanded));
            }
        }

        dedup_paths(&mut roots);
        Self { roots }
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    pub fn discover(&self) -> ZeroBotResult<Vec<SkillInfo>> {
        let mut out = BTreeMap::<String, SkillInfo>::new();
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
                    out.insert(
                        meta.name.clone(),
                        SkillInfo {
                            name: meta.name,
                            description: meta.description,
                            path: entry.path().to_path_buf(),
                        },
                    );
                }
            }
        }
        Ok(out.into_values().collect())
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
    lines.push("Skills 提供任务特定的专用流程与指令。".to_string());
    lines.push("当任务匹配某个 Skill 的描述时，调用 skill 工具按名称加载内容。".to_string());
    lines.push("<available_skills>".to_string());
    for skill in skills {
        lines.push("  <skill>".to_string());
        lines.push(format!("    <name>{}</name>", skill.name));
        lines.push(format!(
            "    <description>{}</description>",
            skill.description
        ));
        lines.push(format!(
            "    <location>{}</location>",
            file_url(&skill.path)
        ));
        lines.push("  </skill>".to_string());
    }
    lines.push("</available_skills>".to_string());
    lines.join("\n")
}

pub fn format_skill_summary(skills: &[SkillInfo]) -> String {
    if skills.is_empty() {
        return "No skills are currently available.".to_string();
    }
    let mut lines = Vec::new();
    lines.push("## Available Skills".to_string());
    for skill in skills {
        lines.push(format!("- **{}**: {}", skill.name, skill.description));
    }
    lines.join("\n")
}

fn file_url(path: &Path) -> String {
    Url::from_file_path(path)
        .map(|url| url.to_string())
        .unwrap_or_else(|_| format!("file://{}", path.display()))
}

fn push_zerobot_skill_roots(out: &mut Vec<PathBuf>, base: &Path) {
    out.push(base.join("skill"));
    out.push(base.join("skills"));
}

fn dedup_paths(paths: &mut Vec<PathBuf>) {
    let mut seen = HashSet::new();
    paths.retain(|path| {
        let key = path.to_string_lossy().to_string();
        seen.insert(key)
    });
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
        let workspace = dir.path().join("workspace");
        let cwd = workspace.join("nested/repo");
        let extra = dir.path().join("extra");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(&extra).unwrap();

        let root = workspace.join(".zerobot/skills/zerobot-test-skill-demo-a");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("SKILL.md"),
            r#"---
name: zerobot-test-skill-demo-a
description: 项目技能
---

项目内容
"#,
        )
        .unwrap();
        std::fs::create_dir_all(workspace.join(".zerobot/skill/zerobot-test-skill-demo-b"))
            .unwrap();
        std::fs::write(
            workspace.join(".zerobot/skill/zerobot-test-skill-demo-b/SKILL.md"),
            r#"---
name: zerobot-test-skill-demo-b
description: 备用技能
---

备用内容
"#,
        )
        .unwrap();
        std::fs::create_dir_all(extra.join("zerobot-test-skill-python")).unwrap();
        std::fs::write(
            extra.join("zerobot-test-skill-python/SKILL.md"),
            r#"---
name: zerobot-test-skill-python
description: Python 技能
---

py-body
"#,
        )
        .unwrap();

        let mut settings = Settings::default();
        settings.skills.paths = vec![extra.to_string_lossy().to_string()];
        let manager = SkillManager::new(&settings, &cwd);
        let skills = manager.discover().unwrap();
        assert!(skills
            .iter()
            .any(|item| item.name == "zerobot-test-skill-demo-a"));
        assert!(skills
            .iter()
            .any(|item| item.name == "zerobot-test-skill-demo-b"));
        assert!(skills
            .iter()
            .any(|item| item.name == "zerobot-test-skill-python"));
        let loaded = manager.load("zerobot-test-skill-demo-a").unwrap();
        assert_eq!(loaded.body, "项目内容");
    }
}
