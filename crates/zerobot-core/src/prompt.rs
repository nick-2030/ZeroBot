pub struct PromptSection {
    pub name: &'static str,
    pub content: &'static str,
}

pub const SYSTEM_PROMPT_SECTIONS: &[PromptSection] = &[
    PromptSection {
        name: "identity",
        content: include_str!("../prompts/system/identity.md"),
    },
    PromptSection {
        name: "behavior",
        content: include_str!("../prompts/system/behavior.md"),
    },
    PromptSection {
        name: "security",
        content: include_str!("../prompts/system/security.md"),
    },
    PromptSection {
        name: "tools",
        content: include_str!("../prompts/system/tools.md"),
    },
    PromptSection {
        name: "tools_parallel",
        content: include_str!("../prompts/system/tools_parallel.md"),
    },
    PromptSection {
        name: "skills",
        content: include_str!("../prompts/system/skills.md"),
    },
    PromptSection {
        name: "hooks",
        content: include_str!("../prompts/system/hooks.md"),
    },
    PromptSection {
        name: "output",
        content: include_str!("../prompts/system/output.md"),
    },
    PromptSection {
        name: "task_mgmt",
        content: include_str!("../prompts/system/task_mgmt.md"),
    },
    PromptSection {
        name: "style",
        content: include_str!("../prompts/system/style.md"),
    },
];

pub fn default_system_prompt() -> String {
    let mut out = String::new();
    for section in SYSTEM_PROMPT_SECTIONS {
        let trimmed = section.content.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(trimmed);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::default_system_prompt;

    #[test]
    fn default_prompt_contains_key_sections() {
        let prompt = default_system_prompt();
        assert!(prompt.contains("行为原则"));
        assert!(prompt.contains("工具使用"));
        assert!(prompt.contains("Skill 协议"));
        assert!(prompt.contains("Hook"));
    }
}
