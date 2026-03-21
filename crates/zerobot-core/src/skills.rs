use crate::config::Settings;
use crate::error::{ZeroBotError, ZeroBotResult};
use crate::workspace::resolve_workspace_root;
use regex::Regex;
use reqwest::blocking::Client;
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;
use tracing::warn;
use url::Url;
use uuid::Uuid;
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
    remote_urls: Vec<String>,
    cache_root: PathBuf,
}

impl SkillManager {
    pub fn new(settings: &Settings, cwd: &Path) -> Self {
        let workspace_root = resolve_workspace_root(cwd);
        let home = home_dir();
        let mut roots = Vec::new();

        if settings.skills.import_external {
            // Claude/Agents compatibility (global)
            roots.push(home.join(".claude").join("skills"));
            roots.push(home.join(".agents").join("skills"));
        }
        // ZeroBot native roots (global)
        push_zerobot_skill_roots(&mut roots, &home.join(".zerobot"));

        if settings.skills.import_external {
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
        Self {
            roots,
            remote_urls: settings.skills.urls.clone(),
            cache_root: home.join(".zerobot").join("cache").join("skills"),
        }
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    pub fn discover(&self) -> ZeroBotResult<Vec<SkillInfo>> {
        let mut roots = self.roots.clone();
        roots.extend(self.fetch_remote_roots());
        dedup_paths(&mut roots);

        let mut out = BTreeMap::<String, SkillInfo>::new();
        for root in &roots {
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
                let content = match std::fs::read_to_string(entry.path()) {
                    Ok(content) => content,
                    Err(err) => {
                        warn!(
                            "读取 skill 失败，已跳过: {}: {}",
                            entry.path().display(),
                            err
                        );
                        continue;
                    }
                };
                let (meta, _body) = match parse_skill_file(&content) {
                    Ok(parsed) => parsed,
                    Err(err) => {
                        warn!(
                            "解析 skill 失败，已跳过: {}: {}",
                            entry.path().display(),
                            err
                        );
                        continue;
                    }
                };
                if !is_valid_skill_name(&meta.name) {
                    warn!(
                        "skill 名称不合法，已跳过: {} (name={})",
                        entry.path().display(),
                        meta.name
                    );
                    continue;
                }
                let key = meta.name.clone();
                let next = SkillInfo {
                    name: key.clone(),
                    description: meta.description,
                    path: entry.path().to_path_buf(),
                };
                if let Some(existing) = out.insert(key.clone(), next) {
                    warn!(
                        "发现重复 skill 名称，后者将覆盖前者: name={}, existing={}, duplicate={}",
                        key,
                        existing.path.display(),
                        entry.path().display()
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
struct RemoteSkillIndex {
    #[serde(default)]
    skills: Vec<RemoteSkillEntry>,
}

#[derive(Debug, Deserialize)]
struct RemoteSkillEntry {
    name: String,
    #[serde(default)]
    files: Vec<String>,
}

impl SkillManager {
    fn fetch_remote_roots(&self) -> Vec<PathBuf> {
        if self.remote_urls.is_empty() {
            return Vec::new();
        }
        let client = match Client::builder().timeout(Duration::from_secs(10)).build() {
            Ok(client) => client,
            Err(err) => {
                warn!("创建 skills 远程客户端失败，已跳过 urls: {err}");
                return Vec::new();
            }
        };
        let mut out = Vec::new();
        for raw in &self.remote_urls {
            out.extend(self.fetch_remote_roots_for_url(&client, raw));
        }
        dedup_paths(&mut out);
        out
    }

    fn fetch_remote_roots_for_url(&self, client: &Client, raw_url: &str) -> Vec<PathBuf> {
        let base = match normalize_base_url(raw_url) {
            Ok(base) => base,
            Err(err) => {
                warn!("skills.urls 地址无效，已跳过: {}: {}", raw_url, err);
                return Vec::new();
            }
        };
        let namespace = self.cache_root.join(url_cache_key(base.as_str()));
        let index_url = match base.join("index.json") {
            Ok(url) => url,
            Err(err) => {
                warn!(
                    "skills.urls 拼接 index.json 失败，已跳过: {}: {}",
                    raw_url, err
                );
                return cached_skill_roots(&namespace);
            }
        };
        let index: RemoteSkillIndex = match client
            .get(index_url.clone())
            .send()
            .and_then(|resp| resp.error_for_status())
            .and_then(|resp| resp.json::<RemoteSkillIndex>())
        {
            Ok(index) => index,
            Err(err) => {
                warn!(
                    "skills.urls 拉取 index.json 失败，回退缓存: {}: {}",
                    index_url, err
                );
                return cached_skill_roots(&namespace);
            }
        };

        let mut roots = Vec::new();
        for skill in index.skills {
            if !is_valid_skill_name(&skill.name) {
                warn!(
                    "远程 skill 名称不合法，已跳过: url={}, name={}",
                    raw_url, skill.name
                );
                continue;
            }
            if !skill.files.iter().any(|file| file == "SKILL.md") {
                warn!(
                    "远程 skill 缺少 SKILL.md，已跳过: url={}, skill={}",
                    raw_url, skill.name
                );
                continue;
            }
            let skill_root = namespace.join(&skill.name);
            let skill_base = match base.join(&format!("{}/", skill.name)) {
                Ok(url) => url,
                Err(err) => {
                    warn!(
                        "远程 skill URL 组装失败，已跳过: url={}, skill={}, err={}",
                        raw_url, skill.name, err
                    );
                    continue;
                }
            };
            for file in &skill.files {
                let Some(rel_path) = sanitize_relative_path(file) else {
                    warn!(
                        "远程 skill 文件路径非法，已跳过: url={}, skill={}, file={}",
                        raw_url, skill.name, file
                    );
                    continue;
                };
                let rel = rel_path.to_string_lossy().to_string();
                let file_url = match skill_base.join(&rel) {
                    Ok(url) => url,
                    Err(err) => {
                        warn!(
                            "远程 skill 文件 URL 组装失败，已跳过: url={}, skill={}, file={}, err={}",
                            raw_url, skill.name, rel, err
                        );
                        continue;
                    }
                };
                let bytes = match client
                    .get(file_url.clone())
                    .send()
                    .and_then(|resp| resp.error_for_status())
                    .and_then(|resp| resp.bytes())
                {
                    Ok(bytes) => bytes,
                    Err(err) => {
                        warn!(
                            "远程 skill 文件下载失败: url={}, skill={}, file={}, err={}",
                            raw_url, skill.name, rel, err
                        );
                        continue;
                    }
                };
                let target = skill_root.join(rel_path);
                if let Some(parent) = target.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if let Err(err) = std::fs::write(&target, bytes.as_ref()) {
                    warn!(
                        "远程 skill 文件写入失败: path={}, err={}",
                        target.display(),
                        err
                    );
                }
            }
            if skill_root.join("SKILL.md").exists() {
                roots.push(skill_root);
            }
        }

        if roots.is_empty() {
            cached_skill_roots(&namespace)
        } else {
            roots
        }
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
    let mut meta: SkillFrontmatter = serde_yaml::from_str(&yaml)
        .map_err(|err| ZeroBotError::Skill(format!("SKILL.md frontmatter 解析失败: {err}")))?;
    meta.name = meta.name.trim().to_string();
    meta.description = meta.description.trim().to_string();
    if meta.name.is_empty() {
        return Err(ZeroBotError::Skill(
            "SKILL.md frontmatter 缺少 name".to_string(),
        ));
    }
    if meta.description.is_empty() {
        return Err(ZeroBotError::Skill(
            "SKILL.md frontmatter 缺少 description".to_string(),
        ));
    }
    let body = lines.collect::<Vec<_>>().join("\n").trim().to_string();
    Ok((meta, body))
}

fn is_valid_skill_name(name: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[a-z0-9]+(?:-[a-z0-9]+)*$").expect("valid skill name regex"))
        .is_match(name)
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

fn normalize_base_url(raw: &str) -> Result<Url, String> {
    let mut text = raw.trim().to_string();
    if !text.ends_with('/') {
        text.push('/');
    }
    Url::parse(&text).map_err(|err| err.to_string())
}

fn url_cache_key(url: &str) -> String {
    Uuid::new_v5(&Uuid::NAMESPACE_URL, url.as_bytes()).to_string()
}

fn sanitize_relative_path(input: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for component in Path::new(input).components() {
        match component {
            Component::Normal(seg) => out.push(seg),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if out.as_os_str().is_empty() {
        None
    } else {
        Some(out)
    }
}

fn cached_skill_roots(namespace: &Path) -> Vec<PathBuf> {
    if !namespace.exists() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for entry in WalkDir::new(namespace).into_iter().filter_map(Result::ok) {
        if !entry.file_type().is_file() {
            continue;
        }
        if entry.file_name() != "SKILL.md" {
            continue;
        }
        if let Some(parent) = entry.path().parent() {
            out.push(parent.to_path_buf());
        }
    }
    dedup_paths(&mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::Method::GET;
    use httpmock::MockServer;
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
        settings.skills.import_external = false;
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

    #[test]
    fn external_skill_import_can_be_disabled() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        let cwd = workspace.join("project");
        std::fs::create_dir_all(&cwd).unwrap();
        let external = cwd.join(".claude/skills/ext");
        std::fs::create_dir_all(&external).unwrap();
        std::fs::write(
            external.join("SKILL.md"),
            r#"---
name: ext-skill
description: external
---

external
"#,
        )
        .unwrap();

        let mut settings = Settings::default();
        settings.skills.enabled = true;
        settings.skills.import_external = false;
        let manager = SkillManager::new(&settings, &cwd);
        let names = manager
            .discover()
            .unwrap()
            .into_iter()
            .map(|item| item.name)
            .collect::<Vec<_>>();
        assert!(!names.contains(&"ext-skill".to_string()));

        settings.skills.import_external = true;
        let manager = SkillManager::new(&settings, &cwd);
        let names = manager
            .discover()
            .unwrap()
            .into_iter()
            .map(|item| item.name)
            .collect::<Vec<_>>();
        assert!(names.contains(&"ext-skill".to_string()));
    }

    #[test]
    fn remote_skill_urls_are_discovered_and_cached() {
        let dir = TempDir::new().unwrap();
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/skills/index.json");
            then.status(200).json_body_obj(&serde_json::json!({
                "skills": [
                    {
                        "name": "remote-demo",
                        "files": ["SKILL.md", "references/readme.md"]
                    }
                ]
            }));
        });
        server.mock(|when, then| {
            when.method(GET).path("/skills/remote-demo/SKILL.md");
            then.status(200).body(
                r#"---
name: remote-demo
description: remote skill
---

remote body
"#,
            );
        });
        server.mock(|when, then| {
            when.method(GET)
                .path("/skills/remote-demo/references/readme.md");
            then.status(200).body("ref");
        });

        let manager = SkillManager {
            roots: Vec::new(),
            remote_urls: vec![server.url("/skills/")],
            cache_root: dir.path().join("cache"),
        };
        let list = manager.discover().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "remote-demo");
        assert!(list[0].path.exists());
        assert!(list[0]
            .path
            .parent()
            .unwrap()
            .join("references/readme.md")
            .exists());
    }
}
