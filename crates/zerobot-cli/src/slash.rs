#[derive(Clone, Debug)]
pub struct SlashCommandSpec {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub description: &'static str,
    pub usage: &'static str,
}

#[derive(Clone, Debug)]
pub struct SlashMatch {
    pub name: String,
    pub description: String,
    pub usage: String,
}

#[derive(Clone, Debug)]
pub struct SlashRegistry {
    commands: Vec<SlashCommandSpec>,
}

impl SlashRegistry {
    pub fn extended() -> Self {
        Self {
            commands: vec![
                SlashCommandSpec {
                    name: "help",
                    aliases: &[],
                    description: "显示命令列表或命令用法",
                    usage: "/help [command]",
                },
                SlashCommandSpec {
                    name: "clear",
                    aliases: &[],
                    description: "清空输出",
                    usage: "/clear",
                },
                SlashCommandSpec {
                    name: "exit",
                    aliases: &["quit", "q"],
                    description: "退出",
                    usage: "/exit",
                },
                SlashCommandSpec {
                    name: "copy",
                    aliases: &[],
                    description: "复制最近一次回复",
                    usage: "/copy",
                },
                SlashCommandSpec {
                    name: "tools",
                    aliases: &[],
                    description: "列出可用工具",
                    usage: "/tools",
                },
                SlashCommandSpec {
                    name: "init",
                    aliases: &[],
                    description: "分析项目并创建/更新 AGENTS.md",
                    usage: "/init [extra requirements]",
                },
                SlashCommandSpec {
                    name: "model",
                    aliases: &[],
                    description: "查看或设置模型",
                    usage: "/model [list|name]",
                },
                SlashCommandSpec {
                    name: "provider",
                    aliases: &[],
                    description: "查看或切换提供商",
                    usage: "/provider [list|id]",
                },
                SlashCommandSpec {
                    name: "config",
                    aliases: &[],
                    description: "查看配置摘要",
                    usage: "/config show",
                },
                SlashCommandSpec {
                    name: "session",
                    aliases: &[],
                    description: "会话管理",
                    usage: "/session list|new [title]|show <id>",
                },
                SlashCommandSpec {
                    name: "resume",
                    aliases: &[],
                    description: "恢复历史会话",
                    usage: "/resume [id]",
                },
                SlashCommandSpec {
                    name: "compact",
                    aliases: &[],
                    description: "压缩当前会话上下文",
                    usage: "/compact",
                },
            ],
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
                name: cmd.name.to_string(),
                description: cmd.description.to_string(),
                usage: cmd.usage.to_string(),
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
