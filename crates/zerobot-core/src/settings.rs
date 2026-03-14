use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::PermissionRules;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LlmProviderSettings {
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub model: Option<String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LlmSettings {
    pub default_provider: Option<String>,
    pub default_model: Option<String>,
    pub openai: Option<LlmProviderSettings>,
    pub anthropic: Option<LlmProviderSettings>,
}

impl LlmSettings {
    pub fn merge(&mut self, other: LlmSettings) {
        if let Some(value) = other.default_provider {
            self.default_provider = Some(value);
        }
        if let Some(value) = other.default_model {
            self.default_model = Some(value);
        }
        if let Some(value) = other.openai {
            self.openai = Some(merge_provider(self.openai.take(), value));
        }
        if let Some(value) = other.anthropic {
            self.anthropic = Some(merge_provider(self.anthropic.take(), value));
        }
    }
}

fn merge_provider(current: Option<LlmProviderSettings>, other: LlmProviderSettings) -> LlmProviderSettings {
    let mut merged = current.unwrap_or_default();
    if let Some(value) = other.base_url {
        merged.base_url = Some(value);
    }
    if let Some(value) = other.api_key {
        merged.api_key = Some(value);
    }
    if let Some(value) = other.model {
        merged.model = Some(value);
    }
    for (k, v) in other.headers {
        merged.headers.insert(k, v);
    }
    merged
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ZeroSettings {
    #[serde(rename = "$schema")]
    pub schema: Option<String>,

    #[serde(default)]
    pub permissions: PermissionRules,

    #[serde(default)]
    pub env: BTreeMap<String, String>,

    #[serde(default)]
    pub llm: LlmSettings,

    pub api_key_helper: Option<String>,

    pub otel_headers_helper: Option<String>,

    #[serde(default)]
    pub hooks: Vec<serde_json::Value>,

    pub disable_all_hooks: Option<bool>,

    pub allow_managed_hooks_only: Option<bool>,

    pub allow_managed_permission_rules_only: Option<bool>,

    pub allow_managed_mcp_servers_only: Option<bool>,

    #[serde(default)]
    pub enabled_plugins: Vec<String>,

    #[serde(default)]
    pub allowed_mcp_servers: Vec<String>,

    #[serde(default)]
    pub denied_mcp_servers: Vec<String>,

    #[serde(default)]
    pub allowed_http_hook_urls: Vec<String>,

    #[serde(default)]
    pub http_hook_allowed_env_vars: Vec<String>,

    #[serde(default)]
    pub company_announcements: Vec<String>,

    pub cleanup_period_days: Option<u32>,

    pub model: Option<String>,

    #[serde(default)]
    pub available_models: Vec<String>,

    #[serde(default)]
    pub model_overrides: BTreeMap<String, String>,

    pub output_style: Option<String>,

    pub status_line: Option<serde_json::Value>,

    pub file_suggestion: Option<serde_json::Value>,

    pub respect_gitignore: Option<bool>,

    pub attribution: Option<serde_json::Value>,

    pub include_git_instructions: Option<bool>,

    pub include_co_authored_by: Option<bool>,

    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl ZeroSettings {
    pub fn merge(&mut self, other: ZeroSettings) {
        if let Some(value) = other.schema {
            self.schema = Some(value);
        }
        self.permissions.allow.extend(other.permissions.allow);
        self.permissions.deny.extend(other.permissions.deny);
        self.permissions.ask.extend(other.permissions.ask);

        for (k, v) in other.env {
            self.env.insert(k, v);
        }

        self.llm.merge(other.llm);

        if let Some(value) = other.api_key_helper {
            self.api_key_helper = Some(value);
        }

        if let Some(value) = other.otel_headers_helper {
            self.otel_headers_helper = Some(value);
        }

        if !other.hooks.is_empty() {
            self.hooks.extend(other.hooks);
        }

        if let Some(value) = other.disable_all_hooks {
            self.disable_all_hooks = Some(value);
        }

        if let Some(value) = other.allow_managed_hooks_only {
            self.allow_managed_hooks_only = Some(value);
        }

        if let Some(value) = other.allow_managed_permission_rules_only {
            self.allow_managed_permission_rules_only = Some(value);
        }

        if let Some(value) = other.allow_managed_mcp_servers_only {
            self.allow_managed_mcp_servers_only = Some(value);
        }

        if let Some(value) = other.cleanup_period_days {
            self.cleanup_period_days = Some(value);
        }

        if let Some(value) = other.model {
            self.model = Some(value);
        }

        if !other.available_models.is_empty() {
            self.available_models.extend(other.available_models);
        }

        for (k, v) in other.model_overrides {
            self.model_overrides.insert(k, v);
        }

        if let Some(value) = other.output_style {
            self.output_style = Some(value);
        }

        if let Some(value) = other.status_line {
            self.status_line = Some(value);
        }

        if let Some(value) = other.file_suggestion {
            self.file_suggestion = Some(value);
        }

        if let Some(value) = other.respect_gitignore {
            self.respect_gitignore = Some(value);
        }

        if let Some(value) = other.attribution {
            self.attribution = Some(value);
        }

        if let Some(value) = other.include_git_instructions {
            self.include_git_instructions = Some(value);
        }

        if let Some(value) = other.include_co_authored_by {
            self.include_co_authored_by = Some(value);
        }

        if !other.enabled_plugins.is_empty() {
            self.enabled_plugins.extend(other.enabled_plugins);
        }

        if !other.allowed_mcp_servers.is_empty() {
            self.allowed_mcp_servers.extend(other.allowed_mcp_servers);
        }

        if !other.denied_mcp_servers.is_empty() {
            self.denied_mcp_servers.extend(other.denied_mcp_servers);
        }

        if !other.allowed_http_hook_urls.is_empty() {
            self.allowed_http_hook_urls.extend(other.allowed_http_hook_urls);
        }

        if !other.http_hook_allowed_env_vars.is_empty() {
            self.http_hook_allowed_env_vars
                .extend(other.http_hook_allowed_env_vars);
        }

        if !other.company_announcements.is_empty() {
            self.company_announcements.extend(other.company_announcements);
        }

        for (k, v) in other.extra {
            self.extra.insert(k, v);
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SettingsScope {
    Managed,
    User,
    Project,
    Local,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingsSource {
    pub scope: SettingsScope,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SettingsBundle {
    pub active: ZeroSettings,
    pub sources: Vec<SettingsSource>,
}
