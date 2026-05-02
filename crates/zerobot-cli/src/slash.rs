use std::collections::BTreeMap;
use zerobot_core::commands::{TemplateCommand, TemplateCommandSource};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SlashCommandOrigin {
    Builtin,
    Command,
    Skill,
    Plugin,
}

#[derive(Clone, Debug)]
pub enum SlashCommandKind {
    Builtin,
    Template(TemplateCommand),
}

#[derive(Clone, Debug)]
pub struct SlashCommandSpec {
    pub name: String,
    pub aliases: Vec<String>,
    pub description: String,
    pub usage: String,
    pub origin: SlashCommandOrigin,
    pub kind: SlashCommandKind,
}

impl SlashCommandSpec {
    pub fn source_tag(&self) -> &'static str {
        match self.origin {
            SlashCommandOrigin::Builtin => "builtin",
            SlashCommandOrigin::Command => "command",
            SlashCommandOrigin::Skill => "skill",
            SlashCommandOrigin::Plugin => "plugin",
        }
    }

    pub fn is_builtin(&self, name: &str) -> bool {
        matches!(self.kind, SlashCommandKind::Builtin) && self.name == name
    }

    pub fn template(&self) -> Option<&TemplateCommand> {
        match &self.kind {
            SlashCommandKind::Builtin => None,
            SlashCommandKind::Template(cmd) => Some(cmd),
        }
    }
}

#[derive(Clone, Debug)]
pub struct SlashMatch {
    pub name: String,
    pub description: String,
}

#[derive(Clone, Debug)]
pub struct SlashRegistry {
    commands: Vec<SlashCommandSpec>,
}

impl SlashRegistry {
    pub fn extended(dynamic: Vec<TemplateCommand>) -> Self {
        let mut merged = BTreeMap::<String, SlashCommandSpec>::new();
        for cmd in builtin_commands() {
            merged.insert(cmd.name.clone(), cmd);
        }
        for cmd in dynamic {
            merged.insert(
                cmd.name.clone(),
                SlashCommandSpec {
                    name: cmd.name.clone(),
                    aliases: Vec::new(),
                    description: cmd.description.clone(),
                    usage: cmd.usage.clone(),
                    origin: match cmd.source {
                        TemplateCommandSource::Command => SlashCommandOrigin::Command,
                        TemplateCommandSource::Skill => SlashCommandOrigin::Skill,
                        TemplateCommandSource::Plugin => SlashCommandOrigin::Plugin,
                    },
                    kind: SlashCommandKind::Template(cmd),
                },
            );
        }
        Self {
            commands: merged.into_values().collect(),
        }
    }

    pub fn commands(&self) -> &[SlashCommandSpec] {
        &self.commands
    }

    pub fn find(&self, raw: &str) -> Option<&SlashCommandSpec> {
        let name = raw.trim().to_lowercase();
        if name.is_empty() {
            return None;
        }
        self.commands.iter().find(|cmd| {
            if cmd.name == name {
                return true;
            }
            cmd.aliases.iter().any(|alias| *alias == name)
        })
    }

    pub fn matches(&self, query: &str) -> Vec<SlashMatch> {
        let q = query.trim().to_lowercase();
        let mut results: Vec<SlashMatch> = self
            .commands
            .iter()
            .filter(|cmd| {
                if q.is_empty() {
                    return true;
                }
                if cmd.name.starts_with(&q) {
                    return true;
                }
                cmd.aliases.iter().any(|alias| alias.starts_with(&q))
            })
            .map(|cmd| SlashMatch {
                name: cmd.name.clone(),
                description: cmd.description.clone(),
            })
            .collect();
        results.sort_by(|a, b| a.name.cmp(&b.name));
        results
    }

    pub fn hint(&self, max: usize) -> String {
        let mut names = self
            .commands
            .iter()
            .map(|cmd| format!("/{}", cmd.name))
            .collect::<Vec<_>>();
        names.sort();
        if max > 0 && names.len() > max {
            names.truncate(max);
            names.push("...".to_string());
        }
        names.join(" ")
    }
}

fn builtin_commands() -> Vec<SlashCommandSpec> {
    vec![
        SlashCommandSpec {
            name: "help".to_string(),
            aliases: vec![],
            description: "显示命令列表或命令用法".to_string(),
            usage: "/help [command]".to_string(),
            origin: SlashCommandOrigin::Builtin,
            kind: SlashCommandKind::Builtin,
        },
        SlashCommandSpec {
            name: "clear".to_string(),
            aliases: vec![],
            description: "清空输出".to_string(),
            usage: "/clear".to_string(),
            origin: SlashCommandOrigin::Builtin,
            kind: SlashCommandKind::Builtin,
        },
        SlashCommandSpec {
            name: "exit".to_string(),
            aliases: vec!["quit".to_string(), "q".to_string()],
            description: "退出".to_string(),
            usage: "/exit".to_string(),
            origin: SlashCommandOrigin::Builtin,
            kind: SlashCommandKind::Builtin,
        },
        SlashCommandSpec {
            name: "copy".to_string(),
            aliases: vec![],
            description: "复制最近一次回复".to_string(),
            usage: "/copy".to_string(),
            origin: SlashCommandOrigin::Builtin,
            kind: SlashCommandKind::Builtin,
        },
        SlashCommandSpec {
            name: "tools".to_string(),
            aliases: vec![],
            description: "列出可用工具".to_string(),
            usage: "/tools".to_string(),
            origin: SlashCommandOrigin::Builtin,
            kind: SlashCommandKind::Builtin,
        },
        SlashCommandSpec {
            name: "init".to_string(),
            aliases: vec![],
            description: "分析项目并创建/更新 AGENTS.md".to_string(),
            usage: "/init [extra requirements]".to_string(),
            origin: SlashCommandOrigin::Builtin,
            kind: SlashCommandKind::Builtin,
        },
        SlashCommandSpec {
            name: "model".to_string(),
            aliases: vec![],
            description: "查看或设置模型".to_string(),
            usage: "/model [list|name]".to_string(),
            origin: SlashCommandOrigin::Builtin,
            kind: SlashCommandKind::Builtin,
        },
        SlashCommandSpec {
            name: "provider".to_string(),
            aliases: vec![],
            description: "查看或切换提供商".to_string(),
            usage: "/provider [list|id]".to_string(),
            origin: SlashCommandOrigin::Builtin,
            kind: SlashCommandKind::Builtin,
        },
        SlashCommandSpec {
            name: "config".to_string(),
            aliases: vec![],
            description: "查看配置摘要".to_string(),
            usage: "/config show".to_string(),
            origin: SlashCommandOrigin::Builtin,
            kind: SlashCommandKind::Builtin,
        },
        SlashCommandSpec {
            name: "session".to_string(),
            aliases: vec![],
            description: "会话管理".to_string(),
            usage: "/session list|new [title]|show <id>".to_string(),
            origin: SlashCommandOrigin::Builtin,
            kind: SlashCommandKind::Builtin,
        },
        SlashCommandSpec {
            name: "resume".to_string(),
            aliases: vec![],
            description: "恢复历史会话".to_string(),
            usage: "/resume [id]".to_string(),
            origin: SlashCommandOrigin::Builtin,
            kind: SlashCommandKind::Builtin,
        },
        SlashCommandSpec {
            name: "compact".to_string(),
            aliases: vec![],
            description: "压缩当前会话上下文".to_string(),
            usage: "/compact".to_string(),
            origin: SlashCommandOrigin::Builtin,
            kind: SlashCommandKind::Builtin,
        },
        SlashCommandSpec {
            name: "diff".to_string(),
            aliases: vec![],
            description: "显示当前会话变更摘要".to_string(),
            usage: "/diff".to_string(),
            origin: SlashCommandOrigin::Builtin,
            kind: SlashCommandKind::Builtin,
        },
        SlashCommandSpec {
            name: "cost".to_string(),
            aliases: vec![],
            description: "显示详细成本统计".to_string(),
            usage: "/cost".to_string(),
            origin: SlashCommandOrigin::Builtin,
            kind: SlashCommandKind::Builtin,
        },
        SlashCommandSpec {
            name: "status".to_string(),
            aliases: vec![],
            description: "显示详细状态信息".to_string(),
            usage: "/status".to_string(),
            origin: SlashCommandOrigin::Builtin,
            kind: SlashCommandKind::Builtin,
        },
        SlashCommandSpec {
            name: "permissions".to_string(),
            aliases: vec!["perms".to_string()],
            description: "显示权限规则".to_string(),
            usage: "/permissions".to_string(),
            origin: SlashCommandOrigin::Builtin,
            kind: SlashCommandKind::Builtin,
        },
        SlashCommandSpec {
            name: "hooks".to_string(),
            aliases: vec![],
            description: "显示已加载的 hooks".to_string(),
            usage: "/hooks".to_string(),
            origin: SlashCommandOrigin::Builtin,
            kind: SlashCommandKind::Builtin,
        },
        SlashCommandSpec {
            name: "skills".to_string(),
            aliases: vec![],
            description: "显示可用技能列表".to_string(),
            usage: "/skills".to_string(),
            origin: SlashCommandOrigin::Builtin,
            kind: SlashCommandKind::Builtin,
        },
        SlashCommandSpec {
            name: "mcp".to_string(),
            aliases: vec![],
            description: "显示 MCP 服务器状态".to_string(),
            usage: "/mcp".to_string(),
            origin: SlashCommandOrigin::Builtin,
            kind: SlashCommandKind::Builtin,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dynamic_command(name: &str, source: TemplateCommandSource) -> TemplateCommand {
        TemplateCommand {
            name: name.to_string(),
            description: format!("desc {name}"),
            template: "template".to_string(),
            source,
            usage: format!("/{name} [args]"),
        }
    }

    #[test]
    fn custom_command_overrides_builtin() {
        let registry = SlashRegistry::extended(vec![dynamic_command(
            "init",
            TemplateCommandSource::Command,
        )]);
        let cmd = registry.find("init").unwrap();
        assert!(matches!(cmd.kind, SlashCommandKind::Template(_)));
        assert_eq!(cmd.source_tag(), "command");
    }

    #[test]
    fn skill_does_not_override_existing_command() {
        let mut map = BTreeMap::<String, TemplateCommand>::new();
        map.insert(
            "review".to_string(),
            dynamic_command("review", TemplateCommandSource::Command),
        );
        map.entry("review".to_string())
            .or_insert_with(|| dynamic_command("review", TemplateCommandSource::Skill));

        let registry = SlashRegistry::extended(map.into_values().collect());
        let cmd = registry.find("review").unwrap();
        assert_eq!(cmd.source_tag(), "command");
    }

    #[test]
    fn plugin_command_supported_in_find_and_matches() {
        let registry = SlashRegistry::extended(vec![dynamic_command(
            "demo:run",
            TemplateCommandSource::Plugin,
        )]);
        let found = registry.find("demo:run").unwrap();
        assert_eq!(found.source_tag(), "plugin");

        let list = registry.matches("demo:");
        assert!(list.iter().any(|item| item.name == "demo:run"));
    }
}
