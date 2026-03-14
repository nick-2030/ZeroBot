use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PermissionRules {
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
    #[serde(default)]
    pub ask: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolPolicy {
    pub allow_bash: bool,
    pub allow_write: bool,
    pub allow_edit: bool,
    pub allow_delete: bool,
}

impl Default for ToolPolicy {
    fn default() -> Self {
        Self {
            allow_bash: false,
            allow_write: true,
            allow_edit: true,
            allow_delete: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionContext {
    pub api_key: String,
}

impl ToolPolicy {
    pub fn is_allowed(&self, tool: &str) -> bool {
        match tool {
            "bash" => self.allow_bash,
            "write" => self.allow_write,
            "edit" => self.allow_edit,
            "delete" => self.allow_delete,
            _ => true,
        }
    }
}
