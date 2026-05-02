use crate::config::Settings;
use crate::error::{ZeroBotError, ZeroBotResult};
use crate::plugin::PluginAssetRoot;
use crate::skills::SkillManager;
use crate::workspace::resolve_workspace_root;
use regex::Regex;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tokio::process::Command;
use tracing::warn;
use walkdir::WalkDir;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplateCommandSource {
    Command,
    Skill,
    Plugin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateCommand {
    pub name: String,
    pub description: String,
    pub template: String,
    pub source: TemplateCommandSource,
    pub usage: String,
}

#[derive(Debug, Deserialize)]
struct CommandFrontmatter {
    description: String,
}

#[derive(Debug, Clone)]
struct ParsedCommandFile {
    name: String,
    description: String,
    template: String,
}

pub fn init_prompt(cwd: &Path, extra: &str) -> String {
    let mut prompt = format!(
        "Please analyze this codebase and create an AGENTS.md file containing:\n\
1. Build/lint/test commands - especially for running a single test\n\
2. Code style guidelines including imports, formatting, types, naming conventions, error handling, etc.\n\n\
The file you create will be given to agentic coding agents (such as yourself) that operate in this repository. Make it about 150 lines long.\n\
If there are Cursor rules (in .cursor/rules/ or .cursorrules) or Copilot rules (in .github/copilot-instructions.md), make sure to include them.\n\n\
If there's already an AGENTS.md, improve it if it's located in {}\n",
        cwd.display()
    );
    let extra = extra.trim();
    if !extra.is_empty() {
        prompt.push('\n');
        prompt.push_str(extra);
        prompt.push('\n');
    }
    prompt
}

pub fn discover_template_commands(
    settings: &Settings,
    cwd: &Path,
    plugin_assets: &[PluginAssetRoot],
) -> ZeroBotResult<Vec<TemplateCommand>> {
    let workspace = resolve_workspace_root(cwd);
    let global_root = home_dir().join(".zerobot");
    discover_template_commands_with_roots(settings, cwd, plugin_assets, &global_root, &workspace)
}

fn discover_template_commands_with_roots(
    settings: &Settings,
    cwd: &Path,
    plugin_assets: &[PluginAssetRoot],
    global_root: &Path,
    workspace_root: &Path,
) -> ZeroBotResult<Vec<TemplateCommand>> {
    let mut commands = BTreeMap::<String, TemplateCommand>::new();

    for file in discover_command_files(global_root)? {
        insert_command(
            &mut commands,
            file,
            TemplateCommandSource::Command,
            true,
            None,
        );
    }
    for file in discover_command_files(&workspace_root.join(".zerobot"))? {
        insert_command(
            &mut commands,
            file,
            TemplateCommandSource::Command,
            true,
            None,
        );
    }

    if settings.skills.enabled {
        let manager = SkillManager::new(settings, cwd);
        let mut skills = manager.discover()?;
        skills.sort_by(|a, b| a.name.cmp(&b.name));
        for skill in skills {
            let content = match std::fs::read_to_string(&skill.path) {
                Ok(text) => text,
                Err(err) => {
                    warn!("读取 skill 失败，跳过: {}: {}", skill.path.display(), err);
                    continue;
                }
            };
            let body = match parse_markdown_body(&content) {
                Ok(body) => body,
                Err(err) => {
                    warn!("解析 skill 失败，跳过: {}: {}", skill.path.display(), err);
                    continue;
                }
            };
            let name = normalize_command_name(&skill.name);
            if name.is_empty() || commands.contains_key(&name) {
                continue;
            }
            commands.insert(
                name.clone(),
                TemplateCommand {
                    name: name.clone(),
                    description: skill.description,
                    template: body,
                    source: TemplateCommandSource::Skill,
                    usage: format!("/{name} [args]"),
                },
            );
        }
    }

    for asset in plugin_assets {
        for file in discover_command_files(&asset.root)? {
            insert_command(
                &mut commands,
                file,
                TemplateCommandSource::Plugin,
                true,
                Some(&asset.plugin),
            );
        }
        for skill in discover_skill_files(&asset.root)? {
            insert_command(
                &mut commands,
                skill,
                TemplateCommandSource::Plugin,
                false,
                Some(&asset.plugin),
            );
        }
    }

    Ok(commands.into_values().collect())
}

pub async fn render_template_prompt(
    command: &TemplateCommand,
    arguments: &str,
    cwd: &Path,
) -> ZeroBotResult<String> {
    let with_args = apply_argument_placeholders(&command.template, arguments);
    let workspace_root = resolve_workspace_root(cwd);
    let with_shell = apply_shell_injections(&with_args, &workspace_root).await?;
    Ok(with_shell.trim().to_string())
}

fn insert_command(
    out: &mut BTreeMap<String, TemplateCommand>,
    file: ParsedCommandFile,
    source: TemplateCommandSource,
    allow_override: bool,
    plugin_prefix: Option<&str>,
) {
    let mut name = normalize_command_name(&file.name);
    if let Some(prefix) = plugin_prefix {
        let prefix = normalize_command_name(prefix);
        name = format!("{prefix}:{name}");
    }
    if name.is_empty() {
        return;
    }
    if !allow_override && out.contains_key(&name) {
        return;
    }
    out.insert(
        name.clone(),
        TemplateCommand {
            name: name.clone(),
            description: file.description,
            template: file.template,
            source,
            usage: format!("/{name} [args]"),
        },
    );
}

fn discover_command_files(root: &Path) -> ZeroBotResult<Vec<ParsedCommandFile>> {
    let mut out = Vec::new();
    for dir_name in ["command", "commands"] {
        let dir = root.join(dir_name);
        if !dir.exists() {
            continue;
        }
        for entry in WalkDir::new(&dir).into_iter().filter_map(Result::ok) {
            if !entry.file_type().is_file() {
                continue;
            }
            if entry.path().extension().and_then(|s| s.to_str()) != Some("md") {
                continue;
            }
            let rel = entry.path().strip_prefix(&dir).unwrap_or(entry.path());
            let mut name = rel.to_string_lossy().replace('\\', "/");
            if name.ends_with(".md") {
                name.truncate(name.len().saturating_sub(3));
            }
            let text = match std::fs::read_to_string(entry.path()) {
                Ok(text) => text,
                Err(err) => {
                    warn!(
                        "读取命令文件失败，跳过: {}: {}",
                        entry.path().display(),
                        err
                    );
                    continue;
                }
            };
            let (description, template) = match parse_command_markdown(&text) {
                Ok(value) => value,
                Err(err) => {
                    warn!(
                        "解析命令文件失败，跳过: {}: {}",
                        entry.path().display(),
                        err
                    );
                    continue;
                }
            };
            out.push(ParsedCommandFile {
                name,
                description,
                template,
            });
        }
    }
    Ok(out)
}

fn discover_skill_files(root: &Path) -> ZeroBotResult<Vec<ParsedCommandFile>> {
    let mut out = Vec::new();
    for dir_name in ["skill", "skills"] {
        let dir = root.join(dir_name);
        if !dir.exists() {
            continue;
        }
        for entry in WalkDir::new(&dir).into_iter().filter_map(Result::ok) {
            if !entry.file_type().is_file() {
                continue;
            }
            if entry.file_name() != "SKILL.md" {
                continue;
            }
            let text = match std::fs::read_to_string(entry.path()) {
                Ok(text) => text,
                Err(err) => {
                    warn!(
                        "读取插件 skill 失败，跳过: {}: {}",
                        entry.path().display(),
                        err
                    );
                    continue;
                }
            };
            let (name, description, template) = match parse_skill_markdown(&text) {
                Ok(value) => value,
                Err(err) => {
                    warn!(
                        "解析插件 skill 失败，跳过: {}: {}",
                        entry.path().display(),
                        err
                    );
                    continue;
                }
            };
            out.push(ParsedCommandFile {
                name,
                description,
                template,
            });
        }
    }
    Ok(out)
}

fn parse_command_markdown(input: &str) -> ZeroBotResult<(String, String)> {
    let (frontmatter, body) = split_markdown_frontmatter(input)?;
    let meta: CommandFrontmatter = serde_yaml::from_str(&frontmatter)
        .map_err(|err| ZeroBotError::Config(format!("命令 frontmatter 解析失败: {err}")))?;
    let description = meta.description.trim();
    if description.is_empty() {
        return Err(ZeroBotError::Config(
            "命令 frontmatter 缺少 description".to_string(),
        ));
    }
    if body.trim().is_empty() {
        return Err(ZeroBotError::Config("命令模板内容为空".to_string()));
    }
    Ok((description.to_string(), body.trim().to_string()))
}

#[derive(Debug, Deserialize)]
struct SkillFrontmatter {
    name: String,
    description: String,
}

fn parse_skill_markdown(input: &str) -> ZeroBotResult<(String, String, String)> {
    let (frontmatter, body) = split_markdown_frontmatter(input)?;
    let meta: SkillFrontmatter = serde_yaml::from_str(&frontmatter)
        .map_err(|err| ZeroBotError::Config(format!("skill frontmatter 解析失败: {err}")))?;
    if meta.name.trim().is_empty() {
        return Err(ZeroBotError::Config(
            "skill frontmatter 缺少 name".to_string(),
        ));
    }
    if meta.description.trim().is_empty() {
        return Err(ZeroBotError::Config(
            "skill frontmatter 缺少 description".to_string(),
        ));
    }
    if body.trim().is_empty() {
        return Err(ZeroBotError::Config("skill 内容为空".to_string()));
    }
    Ok((
        meta.name.trim().to_string(),
        meta.description.trim().to_string(),
        body.trim().to_string(),
    ))
}

fn parse_markdown_body(input: &str) -> ZeroBotResult<String> {
    let (_frontmatter, body) = split_markdown_frontmatter(input)?;
    Ok(body)
}

fn split_markdown_frontmatter(input: &str) -> ZeroBotResult<(String, String)> {
    let mut lines = input.lines();
    if lines.next().unwrap_or("").trim() != "---" {
        return Err(ZeroBotError::Config(
            "markdown 缺少 frontmatter".to_string(),
        ));
    }
    let mut yaml_lines = Vec::new();
    for line in lines.by_ref() {
        if line.trim() == "---" {
            let body = lines.collect::<Vec<_>>().join("\n");
            return Ok((yaml_lines.join("\n"), body));
        }
        yaml_lines.push(line);
    }
    Err(ZeroBotError::Config(
        "markdown frontmatter 缺少结束分隔符 ---".to_string(),
    ))
}

fn normalize_command_name(raw: &str) -> String {
    raw.trim()
        .trim_matches('/')
        .replace('\\', "/")
        .to_lowercase()
}

fn args_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?:\[Image\s+\d+\]|"[^"]*"|'[^']*'|[^\s"']+)"#)
            .expect("args regex must be valid")
    })
}

fn placeholder_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\$(\d+)").expect("placeholder regex must be valid"))
}

fn shell_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"!`([^`]+)`").expect("shell regex must be valid"))
}

fn apply_argument_placeholders(template: &str, arguments: &str) -> String {
    let args = args_regex()
        .find_iter(arguments)
        .map(|m| trim_quotes(m.as_str()))
        .collect::<Vec<_>>();

    let mut max_placeholder = 0usize;
    for cap in placeholder_regex().captures_iter(template) {
        if let Ok(idx) = cap[1].parse::<usize>() {
            max_placeholder = max_placeholder.max(idx);
        }
    }

    let replaced = placeholder_regex()
        .replace_all(template, |caps: &regex::Captures<'_>| {
            let position = caps[1].parse::<usize>().unwrap_or(0);
            if position == 0 {
                return String::new();
            }
            let arg_idx = position - 1;
            if arg_idx >= args.len() {
                return String::new();
            }
            if position == max_placeholder {
                return args[arg_idx..].join(" ");
            }
            args[arg_idx].clone()
        })
        .to_string();

    let uses_arguments = template.contains("$ARGUMENTS");
    let mut result = replaced.replace("$ARGUMENTS", arguments);
    if max_placeholder == 0 && !uses_arguments && !arguments.trim().is_empty() {
        result.push_str("\n\n");
        result.push_str(arguments.trim());
    }
    result
}

async fn apply_shell_injections(template: &str, cwd: &Path) -> ZeroBotResult<String> {
    let mut out = String::new();
    let mut last_end = 0usize;
    for cap in shell_regex().captures_iter(template) {
        let full = cap
            .get(0)
            .ok_or_else(|| ZeroBotError::Tool("shell 注入匹配异常".to_string()))?;
        let cmd = cap
            .get(1)
            .ok_or_else(|| ZeroBotError::Tool("shell 注入命令为空".to_string()))?
            .as_str();
        out.push_str(&template[last_end..full.start()]);
        let output = run_shell_command(cmd, cwd).await?;
        out.push_str(&output);
        last_end = full.end();
    }
    out.push_str(&template[last_end..]);
    Ok(out)
}

async fn run_shell_command(cmd: &str, cwd: &Path) -> ZeroBotResult<String> {
    let output = Command::new("/bin/sh")
        .arg("-lc")
        .arg(cmd)
        .current_dir(cwd)
        .output()
        .await
        .map_err(|err| ZeroBotError::Tool(format!("执行 shell 注入命令失败: {cmd}: {err}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let code = output.status.code().unwrap_or(-1);
        return Err(ZeroBotError::Tool(format!(
            "shell 注入命令失败: `{cmd}` (exit={code}) {}",
            if stderr.is_empty() {
                "".to_string()
            } else {
                format!("stderr: {stderr}")
            }
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn trim_quotes(raw: &str) -> String {
    raw.trim_matches('"').trim_matches('\'').to_string()
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

    fn command_md(description: &str, body: &str) -> String {
        format!("---\ndescription: {description}\n---\n\n{body}\n")
    }

    #[test]
    fn discover_commands_and_skills_with_precedence() {
        let dir = TempDir::new().unwrap();
        let global = dir.path().join("global");
        let workspace = dir.path().join("workspace");
        let cwd = workspace.join("repo");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(global.join("commands")).unwrap();
        std::fs::create_dir_all(workspace.join(".zerobot/commands")).unwrap();
        std::fs::create_dir_all(cwd.join(".zerobot/skills/my-skill")).unwrap();

        std::fs::write(
            global.join("commands/init.md"),
            command_md("global init", "global-init"),
        )
        .unwrap();
        std::fs::write(
            global.join("commands/shared.md"),
            command_md("global shared", "global-shared"),
        )
        .unwrap();
        std::fs::write(
            workspace.join(".zerobot/commands/shared.md"),
            command_md("project shared", "project-shared"),
        )
        .unwrap();
        std::fs::create_dir_all(workspace.join(".zerobot/commands/review")).unwrap();
        std::fs::write(
            workspace.join(".zerobot/commands/review/backend.md"),
            command_md("project nested", "project-nested"),
        )
        .unwrap();
        std::fs::write(
            cwd.join(".zerobot/skills/my-skill/SKILL.md"),
            r#"---
name: shared
description: skill shared
---
skill-shared
"#,
        )
        .unwrap();

        let mut settings = Settings::default();
        settings.skills.enabled = true;
        let plugin_assets: Vec<PluginAssetRoot> = Vec::new();
        let list = discover_template_commands_with_roots(
            &settings,
            &cwd,
            &plugin_assets,
            &global,
            &workspace,
        )
        .unwrap();
        let mut map = BTreeMap::new();
        for cmd in list {
            map.insert(cmd.name.clone(), cmd);
        }

        assert_eq!(map["init"].description, "global init");
        assert_eq!(map["shared"].description, "project shared");
        assert_eq!(map["shared"].source, TemplateCommandSource::Command);
        assert!(map.contains_key("review/backend"));
    }

    #[test]
    fn discover_plugin_namespaced_commands() {
        let dir = TempDir::new().unwrap();
        let global = dir.path().join("global");
        let workspace = dir.path().join("workspace");
        let cwd = workspace.join("repo");
        let plugin_root = dir.path().join("plugins/demo");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(plugin_root.join("commands")).unwrap();
        std::fs::create_dir_all(plugin_root.join("skills/check")).unwrap();

        std::fs::write(
            plugin_root.join("commands/run.md"),
            command_md("plugin run", "run-body"),
        )
        .unwrap();
        std::fs::write(
            plugin_root.join("skills/check/SKILL.md"),
            r#"---
name: inspect
description: inspect skill
---
inspect-body
"#,
        )
        .unwrap();

        let assets = vec![PluginAssetRoot {
            plugin: "demo".to_string(),
            root: plugin_root,
        }];
        let list = discover_template_commands_with_roots(
            &Settings::default(),
            &cwd,
            &assets,
            &global,
            &workspace,
        )
        .unwrap();
        let names = list.into_iter().map(|c| c.name).collect::<Vec<_>>();
        assert!(names.contains(&"demo:run".to_string()));
        assert!(names.contains(&"demo:inspect".to_string()));
    }

    #[test]
    fn apply_argument_rules() {
        let rendered = apply_argument_placeholders("cmd $1 $2", "a b c");
        assert_eq!(rendered, "cmd a b c");

        let rendered = apply_argument_placeholders("cmd $ARGUMENTS", "x y");
        assert_eq!(rendered, "cmd x y");

        let rendered = apply_argument_placeholders("cmd", "tail");
        assert_eq!(rendered, "cmd\n\ntail");
    }

    #[tokio::test]
    async fn shell_injection_success_and_failure() {
        let dir = TempDir::new().unwrap();
        let ok = apply_shell_injections("value: !`printf hello`", dir.path())
            .await
            .unwrap();
        assert_eq!(ok, "value: hello");

        let err = apply_shell_injections("!`exit 7`", dir.path())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("shell 注入命令失败"));
    }
}
