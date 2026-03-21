use crate::error::{ZeroBotError, ZeroBotResult};
use crate::prompt::default_system_prompt;
use crate::workspace::resolve_workspace_root;
use serde::{Deserialize, Serialize};
use serde_yaml::Value as YamlValue;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub providers: HashMap<String, ProviderSettings>,
    #[serde(default)]
    pub default_provider: Option<String>,
    #[serde(default)]
    pub default_model: Option<String>,
    #[serde(default)]
    pub session: SessionSettings,
    #[serde(default)]
    pub tools: ToolSettings,
    #[serde(default)]
    pub agent: AgentSettings,
    #[serde(default)]
    pub context: ContextSettings,
    #[serde(default)]
    pub instructions: Vec<String>,
    #[serde(default)]
    pub logging: LoggingSettings,
    #[serde(default)]
    pub gateway: GatewaySettings,
    #[serde(default)]
    pub channels: ChannelsSettings,
    #[serde(default)]
    pub mcp: McpSettings,
    #[serde(default)]
    pub skills: SkillsSettings,
    #[serde(default)]
    pub plugins: PluginsSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProviderSettings {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSettings {
    #[serde(default = "default_max_history")]
    pub max_history: usize,
}

fn default_max_history() -> usize {
    200
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSettings {
    #[serde(default = "default_tool_list")]
    pub enabled: Vec<String>,
    #[serde(default)]
    pub allow_paths: Vec<String>,
    #[serde(default)]
    pub output: ToolOutputSettings,
    #[serde(default)]
    pub approval: ToolApprovalSettings,
}

fn default_tool_list() -> Vec<String> {
    vec![
        "read".to_string(),
        "write".to_string(),
        "edit".to_string(),
        "apply_patch".to_string(),
        "glob".to_string(),
        "grep".to_string(),
        "bash".to_string(),
        "todoread".to_string(),
        "todowrite".to_string(),
        "request_user_input".to_string(),
        "subagent".to_string(),
    ]
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolApprovalMode {
    Auto,
    Prompt,
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolApprovalSettings {
    #[serde(default = "default_tool_approval_mode")]
    pub default: ToolApprovalMode,
    #[serde(default = "default_tool_approval_overrides")]
    pub per_tool: HashMap<String, ToolApprovalMode>,
    #[serde(default)]
    pub bash: CommandApprovalSettings,
}

fn default_tool_approval_mode() -> ToolApprovalMode {
    ToolApprovalMode::Prompt
}

fn default_tool_approval_overrides() -> HashMap<String, ToolApprovalMode> {
    let mut map = HashMap::new();
    for name in ["read", "glob", "grep", "todoread", "request_user_input"] {
        map.insert(name.to_string(), ToolApprovalMode::Auto);
    }
    map
}

impl ToolApprovalSettings {
    pub fn mode_for(&self, tool_name: &str) -> ToolApprovalMode {
        self.per_tool
            .get(tool_name)
            .copied()
            .unwrap_or(self.default)
    }

    pub fn bash_mode_for(&self, command: &str) -> Option<ToolApprovalMode> {
        self.bash.mode_for(command)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CommandApprovalSettings {
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub ask: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
}

impl CommandApprovalSettings {
    pub fn mode_for(&self, command: &str) -> Option<ToolApprovalMode> {
        if matches_any(&self.deny, command) {
            return Some(ToolApprovalMode::Deny);
        }
        if matches_any(&self.ask, command) {
            return Some(ToolApprovalMode::Prompt);
        }
        if matches_any(&self.allow, command) {
            return Some(ToolApprovalMode::Auto);
        }
        None
    }
}

fn matches_any(patterns: &[String], value: &str) -> bool {
    patterns.iter().any(|pattern| {
        glob::Pattern::new(pattern)
            .map(|p| p.matches(value))
            .unwrap_or(false)
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutputSettings {
    #[serde(default = "default_tool_output_max_lines")]
    pub max_lines: usize,
    #[serde(default = "default_tool_output_max_bytes")]
    pub max_bytes: usize,
    #[serde(default = "default_tool_output_direction")]
    pub direction: ToolOutputDirection,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutputDirection {
    Head,
    Tail,
}

fn default_tool_output_max_lines() -> usize {
    2000
}

fn default_tool_output_max_bytes() -> usize {
    50 * 1024
}

fn default_tool_output_direction() -> ToolOutputDirection {
    ToolOutputDirection::Head
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSettings {
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default = "default_max_steps")]
    pub max_steps: usize,
}

fn default_max_steps() -> usize {
    100
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextSettings {
    #[serde(default = "default_context_max_messages")]
    pub max_messages: usize,
    #[serde(default = "default_context_max_chars")]
    pub max_chars: usize,
    #[serde(default = "default_context_include_environment")]
    pub include_environment: bool,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub model_limits: HashMap<String, u32>,
    #[serde(default)]
    pub compaction: CompactionSettings,
}

fn default_context_max_messages() -> usize {
    200
}

fn default_context_max_chars() -> usize {
    120_000
}

fn default_context_include_environment() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionSettings {
    #[serde(default = "default_compaction_enabled")]
    pub enabled: bool,
    #[serde(default = "default_compaction_auto")]
    pub auto: bool,
    #[serde(default = "default_compaction_reserved_tokens")]
    pub reserved_tokens: u32,
    #[serde(default)]
    pub summary_model: Option<String>,
}

fn default_compaction_enabled() -> bool {
    true
}

fn default_compaction_auto() -> bool {
    true
}

fn default_compaction_reserved_tokens() -> u32 {
    2048
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingSettings {
    #[serde(default = "default_log_level")]
    pub level: String,
}

fn default_log_level() -> String {
    "info".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewaySettings {
    #[serde(default)]
    pub heartbeat: HeartbeatSettings,
    #[serde(default)]
    pub cron: CronSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_heartbeat_interval_s")]
    pub interval_s: u64,
    #[serde(default = "default_heartbeat_file")]
    pub file: String,
    #[serde(default)]
    pub target: Option<ChannelTarget>,
}

fn default_heartbeat_interval_s() -> u64 {
    30 * 60
}

fn default_heartbeat_file() -> String {
    "HEARTBEAT.md".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelTarget {
    pub channel: String,
    #[serde(rename = "chat_id")]
    pub chat_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronSettings {
    #[serde(default = "default_cron_history_limit")]
    pub run_history_limit: usize,
    #[serde(default)]
    pub export_json: Option<String>,
}

fn default_cron_history_limit() -> usize {
    20
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelsSettings {
    #[serde(default = "default_channels_send_progress")]
    pub send_progress: bool,
    #[serde(default)]
    pub send_tool_hints: bool,
    #[serde(default)]
    pub feishu: FeishuChannelSettings,
}

fn default_channels_send_progress() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeishuChannelSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub app_id: String,
    #[serde(default)]
    pub app_secret: String,
    #[serde(default)]
    pub encrypt_key: String,
    #[serde(default)]
    pub verification_token: String,
    #[serde(default)]
    pub allow_from: Vec<String>,
    #[serde(default = "default_feishu_group_policy")]
    pub group_policy: FeishuGroupPolicy,
    #[serde(default)]
    pub reply_to_message: bool,
    #[serde(default = "default_feishu_dedup_max_entries")]
    pub dedup_max_entries: usize,
    #[serde(default = "default_feishu_reaction_mode")]
    pub reaction_mode: FeishuReactionMode,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub bot_open_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FeishuGroupPolicy {
    Open,
    Mention,
}

fn default_feishu_group_policy() -> FeishuGroupPolicy {
    FeishuGroupPolicy::Mention
}

fn default_feishu_dedup_max_entries() -> usize {
    5000
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FeishuReactionMode {
    Off,
    Own,
    All,
}

fn default_feishu_reaction_mode() -> FeishuReactionMode {
    FeishuReactionMode::Own
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub servers: Vec<McpServerConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillsSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PluginFailureMode {
    Open,
    Closed,
}

fn default_plugin_failure_mode() -> PluginFailureMode {
    PluginFailureMode::Open
}

fn default_plugin_hook_timeout_ms() -> u64 {
    3000
}

fn default_plugin_tool_timeout_ms() -> u64 {
    120_000
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginEntryConfig {
    pub name: String,
    pub command: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub hook_timeout_ms: Option<u64>,
    #[serde(default)]
    pub tool_timeout_ms: Option<u64>,
    #[serde(default)]
    pub failure_mode: Option<PluginFailureMode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    #[serde(default)]
    pub name: String,
    pub command: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub hook_timeout_ms: Option<u64>,
    #[serde(default)]
    pub tool_timeout_ms: Option<u64>,
    #[serde(default)]
    pub failure_mode: Option<PluginFailureMode>,
}

impl PluginManifest {
    pub fn into_entry(self) -> PluginEntryConfig {
        PluginEntryConfig {
            name: self.name,
            command: self.command,
            env: self.env,
            enabled: self.enabled,
            hook_timeout_ms: self.hook_timeout_ms,
            tool_timeout_ms: self.tool_timeout_ms,
            failure_mode: self.failure_mode,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginsSettings {
    #[serde(default = "default_plugins_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub entries: Vec<PluginEntryConfig>,
    #[serde(default = "default_plugins_auto_enable_tools")]
    pub auto_enable_tools: bool,
    #[serde(default = "default_plugin_hook_timeout_ms")]
    pub default_hook_timeout_ms: u64,
    #[serde(default = "default_plugin_tool_timeout_ms")]
    pub default_tool_timeout_ms: u64,
    #[serde(default = "default_plugin_failure_mode")]
    pub failure_mode: PluginFailureMode,
}

fn default_plugins_enabled() -> bool {
    true
}

fn default_plugins_auto_enable_tools() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum McpServerConfig {
    Local {
        name: String,
        command: Vec<String>,
        #[serde(default)]
        env: std::collections::HashMap<String, String>,
        #[serde(default)]
        protocol: Option<McpLocalProtocol>,
        #[serde(default)]
        timeout_ms: Option<u64>,
        #[serde(default)]
        enabled: Option<bool>,
    },
    Remote {
        name: String,
        url: String,
        #[serde(default)]
        headers: std::collections::HashMap<String, String>,
        #[serde(default)]
        timeout_ms: Option<u64>,
        #[serde(default)]
        enabled: Option<bool>,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpLocalProtocol {
    ContentLength,
    Line,
}

impl Default for McpLocalProtocol {
    fn default() -> Self {
        McpLocalProtocol::ContentLength
    }
}

impl McpServerConfig {
    pub fn name(&self) -> &str {
        match self {
            McpServerConfig::Local { name, .. } => name,
            McpServerConfig::Remote { name, .. } => name,
        }
    }

    pub fn is_enabled(&self) -> bool {
        match self {
            McpServerConfig::Local { enabled, .. } => enabled.unwrap_or(true),
            McpServerConfig::Remote { enabled, .. } => enabled.unwrap_or(true),
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: None,
            providers: HashMap::new(),
            default_provider: None,
            default_model: None,
            session: SessionSettings::default(),
            tools: ToolSettings::default(),
            agent: AgentSettings::default(),
            context: ContextSettings::default(),
            instructions: Vec::new(),
            logging: LoggingSettings::default(),
            gateway: GatewaySettings::default(),
            channels: ChannelsSettings::default(),
            mcp: McpSettings::default(),
            skills: SkillsSettings::default(),
            plugins: PluginsSettings::default(),
        }
    }
}

impl Default for SessionSettings {
    fn default() -> Self {
        Self {
            max_history: default_max_history(),
        }
    }
}

impl Default for ToolSettings {
    fn default() -> Self {
        Self {
            enabled: default_tool_list(),
            allow_paths: Vec::new(),
            output: ToolOutputSettings::default(),
            approval: ToolApprovalSettings::default(),
        }
    }
}

impl Default for ToolApprovalSettings {
    fn default() -> Self {
        Self {
            default: default_tool_approval_mode(),
            per_tool: default_tool_approval_overrides(),
            bash: CommandApprovalSettings::default(),
        }
    }
}

impl Default for ToolOutputSettings {
    fn default() -> Self {
        Self {
            max_lines: default_tool_output_max_lines(),
            max_bytes: default_tool_output_max_bytes(),
            direction: default_tool_output_direction(),
        }
    }
}

impl Default for AgentSettings {
    fn default() -> Self {
        Self {
            system_prompt: Some(default_system_prompt()),
            max_steps: default_max_steps(),
        }
    }
}

impl Default for ContextSettings {
    fn default() -> Self {
        Self {
            max_messages: default_context_max_messages(),
            max_chars: default_context_max_chars(),
            include_environment: default_context_include_environment(),
            max_tokens: None,
            model_limits: HashMap::new(),
            compaction: CompactionSettings::default(),
        }
    }
}

impl Default for CompactionSettings {
    fn default() -> Self {
        Self {
            enabled: default_compaction_enabled(),
            auto: default_compaction_auto(),
            reserved_tokens: default_compaction_reserved_tokens(),
            summary_model: None,
        }
    }
}

impl Default for LoggingSettings {
    fn default() -> Self {
        Self {
            level: default_log_level(),
        }
    }
}

impl Default for GatewaySettings {
    fn default() -> Self {
        Self {
            heartbeat: HeartbeatSettings::default(),
            cron: CronSettings::default(),
        }
    }
}

impl Default for HeartbeatSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_s: default_heartbeat_interval_s(),
            file: default_heartbeat_file(),
            target: None,
        }
    }
}

impl Default for CronSettings {
    fn default() -> Self {
        Self {
            run_history_limit: default_cron_history_limit(),
            export_json: None,
        }
    }
}

impl Default for ChannelsSettings {
    fn default() -> Self {
        Self {
            send_progress: default_channels_send_progress(),
            send_tool_hints: false,
            feishu: FeishuChannelSettings::default(),
        }
    }
}

impl Default for FeishuChannelSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            app_id: String::new(),
            app_secret: String::new(),
            encrypt_key: String::new(),
            verification_token: String::new(),
            allow_from: Vec::new(),
            group_policy: default_feishu_group_policy(),
            reply_to_message: false,
            dedup_max_entries: default_feishu_dedup_max_entries(),
            reaction_mode: default_feishu_reaction_mode(),
            base_url: None,
            bot_open_id: None,
        }
    }
}

impl Default for McpSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            servers: Vec::new(),
        }
    }
}

impl Default for SkillsSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            paths: Vec::new(),
        }
    }
}

impl Default for PluginsSettings {
    fn default() -> Self {
        Self {
            enabled: default_plugins_enabled(),
            paths: Vec::new(),
            entries: Vec::new(),
            auto_enable_tools: default_plugins_auto_enable_tools(),
            default_hook_timeout_ms: default_plugin_hook_timeout_ms(),
            default_tool_timeout_ms: default_plugin_tool_timeout_ms(),
            failure_mode: default_plugin_failure_mode(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigScope {
    Defaults,
    User,
    Project,
    Local,
    Managed,
    Cli,
}

#[derive(Debug, Clone)]
pub struct ConfigLayer {
    pub scope: ConfigScope,
    pub path: Option<PathBuf>,
    pub applied: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct LoadedConfig {
    pub settings: Settings,
    pub layers: Vec<ConfigLayer>,
    pub warnings: Vec<String>,
}

pub struct ConfigLoader {
    cwd: PathBuf,
    cli_overrides: Vec<(String, String)>,
}

impl ConfigLoader {
    pub fn new(cwd: PathBuf) -> Self {
        Self {
            cwd,
            cli_overrides: Vec::new(),
        }
    }

    pub fn with_cli_overrides(mut self, overrides: Vec<(String, String)>) -> Self {
        self.cli_overrides = overrides;
        self
    }

    pub fn load(&self) -> ZeroBotResult<LoadedConfig> {
        let mut layers = Vec::new();
        let mut warnings = Vec::new();

        let mut merged = serde_yaml::to_value(Settings::default())
            .map_err(|err| ZeroBotError::Config(err.to_string()))?;

        layers.push(ConfigLayer {
            scope: ConfigScope::Defaults,
            path: None,
            applied: true,
            reason: None,
        });

        if let Some(user_path) = user_settings_path() {
            if let Some(value) = read_yaml(&user_path)? {
                merged = merge_yaml(merged, value);
                layers.push(ConfigLayer {
                    scope: ConfigScope::User,
                    path: Some(user_path),
                    applied: true,
                    reason: None,
                });
            }
        }

        let project_dir = resolve_workspace_root(&self.cwd);
        let project_settings = project_dir.join(".zerobot").join("settings.yaml");
        let local_settings = project_dir.join(".zerobot").join("settings.local.yaml");

        let zerobot_ignored = is_zerobot_ignored(&project_dir)?;

        if zerobot_ignored {
            layers.push(ConfigLayer {
                scope: ConfigScope::Project,
                path: Some(project_settings.clone()),
                applied: false,
                reason: Some("项目目录已被 .gitignore 忽略".to_string()),
            });
        } else if let Some(value) = read_yaml(&project_settings)? {
            merged = merge_yaml(merged, value);
            layers.push(ConfigLayer {
                scope: ConfigScope::Project,
                path: Some(project_settings),
                applied: true,
                reason: None,
            });
        }

        if let Some(value) = read_yaml(&local_settings)? {
            merged = merge_yaml(merged, value);
            layers.push(ConfigLayer {
                scope: ConfigScope::Local,
                path: Some(local_settings.clone()),
                applied: true,
                reason: None,
            });
        }

        if let Some(path) = managed_settings_path() {
            if let Some(value) = read_yaml(&path)? {
                merged = merge_yaml(merged, value);
                layers.push(ConfigLayer {
                    scope: ConfigScope::Managed,
                    path: Some(path),
                    applied: true,
                    reason: None,
                });
            }
        }

        if !self.cli_overrides.is_empty() {
            let mut overrides = YamlValue::Mapping(Default::default());
            for (key, value) in &self.cli_overrides {
                let v = parse_override_value(value)?;
                set_yaml_path(&mut overrides, key, v)?;
            }
            merged = merge_yaml(merged, overrides);
            layers.push(ConfigLayer {
                scope: ConfigScope::Cli,
                path: None,
                applied: true,
                reason: None,
            });
        }

        if local_settings.exists() && !is_local_settings_ignored(&project_dir)? {
            warnings.push("settings.local.yaml 未加入 .gitignore".to_string());
        }

        let mut settings: Settings =
            serde_yaml::from_value(merged).map_err(|err| ZeroBotError::Config(err.to_string()))?;
        settings.plugins.entries = deduplicate_plugin_entries(settings.plugins.entries);

        Ok(LoadedConfig {
            settings,
            layers,
            warnings,
        })
    }
}

fn read_yaml(path: &Path) -> ZeroBotResult<Option<YamlValue>> {
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(path)?;
    let value =
        serde_yaml::from_str(&content).map_err(|err| ZeroBotError::Config(err.to_string()))?;
    Ok(Some(value))
}

fn parse_override_value(raw: &str) -> ZeroBotResult<YamlValue> {
    serde_yaml::from_str(raw).map_err(|err| ZeroBotError::Config(err.to_string()))
}

fn set_yaml_path(target: &mut YamlValue, path: &str, value: YamlValue) -> ZeroBotResult<()> {
    let parts: Vec<&str> = path.split('.').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return Err(ZeroBotError::Config("CLI 覆盖路径为空".to_string()));
    }

    fn set_inner(
        current: &mut YamlValue,
        parts: &[&str],
        value: YamlValue,
        path: &str,
    ) -> ZeroBotResult<()> {
        if parts.is_empty() {
            return Err(ZeroBotError::Config("CLI 覆盖路径为空".to_string()));
        }

        if parts.len() == 1 {
            if !matches!(current, YamlValue::Mapping(_)) {
                *current = YamlValue::Mapping(Default::default());
            }
            if let YamlValue::Mapping(map) = current {
                map.insert(YamlValue::String(parts[0].to_string()), value);
                return Ok(());
            }
        }

        if !matches!(current, YamlValue::Mapping(_)) {
            *current = YamlValue::Mapping(Default::default());
        }

        if let YamlValue::Mapping(map) = current {
            let key = YamlValue::String(parts[0].to_string());
            let entry = map
                .entry(key)
                .or_insert_with(|| YamlValue::Mapping(Default::default()));
            return set_inner(entry, &parts[1..], value, path);
        }

        Err(ZeroBotError::Config(format!("无法写入覆盖路径: {path}")))
    }

    set_inner(target, &parts, value, path)
}

fn merge_yaml(base: YamlValue, overlay: YamlValue) -> YamlValue {
    merge_yaml_at_path(base, overlay, &[])
}

fn merge_yaml_at_path(base: YamlValue, overlay: YamlValue, path: &[String]) -> YamlValue {
    match (base, overlay) {
        (YamlValue::Mapping(mut base_map), YamlValue::Mapping(overlay_map)) => {
            for (key, value) in overlay_map {
                let key_name = key.as_str().unwrap_or_default().to_string();
                let mut next_path = path.to_vec();
                next_path.push(key_name);
                let existing = base_map.remove(&key);
                let merged = if let Some(existing) = existing {
                    merge_yaml_at_path(existing, value, &next_path)
                } else {
                    value
                };
                base_map.insert(key, merged);
            }
            YamlValue::Mapping(base_map)
        }
        (YamlValue::Sequence(mut base_seq), YamlValue::Sequence(overlay_seq))
            if path.len() == 2 && path[0] == "plugins" && path[1] == "entries" =>
        {
            base_seq.extend(overlay_seq);
            YamlValue::Sequence(base_seq)
        }
        (_, overlay) => overlay,
    }
}

fn deduplicate_plugin_entries(entries: Vec<PluginEntryConfig>) -> Vec<PluginEntryConfig> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for entry in entries.into_iter().rev() {
        if seen.insert(entry.name.clone()) {
            out.push(entry);
        }
    }
    out.reverse();
    out
}

fn user_settings_path() -> Option<PathBuf> {
    Some(home_dir().join(".zerobot").join("settings.yaml"))
}

fn managed_settings_path() -> Option<PathBuf> {
    if cfg!(windows) {
        let base = std::env::var("PROGRAMDATA").unwrap_or_else(|_| "C:\\ProgramData".to_string());
        return Some(
            PathBuf::from(base)
                .join("ZeroBot")
                .join("managed-settings.yaml"),
        );
    }
    Some(PathBuf::from("/etc/zerobot/managed-settings.yaml"))
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

fn is_zerobot_ignored(project_dir: &Path) -> ZeroBotResult<bool> {
    let gitignore_path = project_dir.join(".gitignore");
    if !gitignore_path.exists() {
        return Ok(false);
    }
    let mut builder = ignore::gitignore::GitignoreBuilder::new(project_dir);
    builder.add(gitignore_path);
    let gitignore = builder
        .build()
        .map_err(|err| ZeroBotError::Config(err.to_string()))?;
    let target = project_dir.join(".zerobot");
    Ok(gitignore.matched(&target, true).is_ignore())
}

fn is_local_settings_ignored(project_dir: &Path) -> ZeroBotResult<bool> {
    let gitignore_path = project_dir.join(".gitignore");
    if !gitignore_path.exists() {
        return Ok(false);
    }
    let mut builder = ignore::gitignore::GitignoreBuilder::new(project_dir);
    builder.add(gitignore_path);
    let gitignore = builder
        .build()
        .map_err(|err| ZeroBotError::Config(err.to_string()))?;
    let target = project_dir.join(".zerobot").join("settings.local.yaml");
    Ok(gitignore.matched(&target, false).is_ignore())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    fn write_file(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn config_precedence_respects_overrides() {
        let dir = TempDir::new().unwrap();
        let cwd = dir.path();

        write_file(
            &cwd.join(".zerobot/settings.yaml"),
            "default_provider: openai\n",
        );
        write_file(
            &cwd.join(".zerobot/settings.local.yaml"),
            "default_provider: anthropic\n",
        );

        let loader = ConfigLoader::new(cwd.to_path_buf())
            .with_cli_overrides(vec![("default_provider".to_string(), "cli".to_string())]);
        let loaded = loader.load().unwrap();
        assert_eq!(loaded.settings.default_provider, Some("cli".to_string()));
    }

    #[test]
    fn project_settings_skipped_when_ignored() {
        let dir = TempDir::new().unwrap();
        let cwd = dir.path();
        write_file(&cwd.join(".gitignore"), ".zerobot\n");
        write_file(
            &cwd.join(".zerobot/settings.yaml"),
            "default_provider: openai\n",
        );
        let loader = ConfigLoader::new(cwd.to_path_buf());
        let loaded = loader.load().unwrap();
        assert_eq!(loaded.settings.default_provider, None);
    }

    #[test]
    fn mcp_and_skills_config_parses() {
        let dir = TempDir::new().unwrap();
        let cwd = dir.path();
        write_file(
            &cwd.join(".zerobot/settings.yaml"),
            r#"
mcp:
  enabled: true
  servers:
    - name: "local-one"
      type: "local"
      command: ["mcp-server", "--stdio"]
      env:
        KEY: "VALUE"
      timeout_ms: 3000
      enabled: true
    - name: "remote-one"
      type: "remote"
      url: "https://example.com/mcp"
      headers:
        X-Token: "abc"
      timeout_ms: 5000
      enabled: false
skills:
  enabled: true
  paths:
    - "/tmp/skills"
"#,
        );
        let loader = ConfigLoader::new(cwd.to_path_buf());
        let loaded = loader.load().unwrap();
        assert!(loaded.settings.mcp.enabled);
        assert_eq!(loaded.settings.mcp.servers.len(), 2);
        assert_eq!(loaded.settings.mcp.servers[0].name(), "local-one");
        assert_eq!(loaded.settings.mcp.servers[1].name(), "remote-one");
        assert!(loaded.settings.skills.enabled);
        assert_eq!(loaded.settings.skills.paths.len(), 1);
    }

    #[test]
    fn plugin_entries_merge_and_deduplicate() {
        let dir = TempDir::new().unwrap();
        let cwd = dir.path();
        write_file(
            &cwd.join(".zerobot/settings.yaml"),
            r#"
plugins:
  entries:
    - name: "demo"
      command: ["echo", "user"]
    - name: "keep"
      command: ["echo", "keep"]
"#,
        );
        write_file(
            &cwd.join(".zerobot/settings.local.yaml"),
            r#"
plugins:
  entries:
    - name: "demo"
      command: ["echo", "local"]
"#,
        );
        let loaded = ConfigLoader::new(cwd.to_path_buf()).load().unwrap();
        let entries = loaded.settings.plugins.entries;
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "keep");
        assert_eq!(entries[1].name, "demo");
        assert_eq!(
            entries[1].command,
            vec!["echo".to_string(), "local".to_string()]
        );
    }
}
