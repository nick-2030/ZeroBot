use crate::error::{ZeroBotError, ZeroBotResult};
use crate::prompt::DEFAULT_SYSTEM_PROMPT;
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
    pub logging: LoggingSettings,
    #[serde(default)]
    pub mcp: McpSettings,
    #[serde(default)]
    pub skills: SkillsSettings,
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
    #[serde(default = "default_db_path")]
    pub db_path: String,
    #[serde(default = "default_max_history")]
    pub max_history: usize,
}

fn default_db_path() -> String {
    default_state_dir()
        .join("zerobot.db")
        .to_string_lossy()
        .to_string()
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
}

fn default_tool_list() -> Vec<String> {
    vec![
        "read".to_string(),
        "write".to_string(),
        "edit".to_string(),
        "patch".to_string(),
        "glob".to_string(),
        "grep".to_string(),
        "shell".to_string(),
    ]
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
pub struct LoggingSettings {
    #[serde(default = "default_log_level")]
    pub level: String,
}

fn default_log_level() -> String {
    "info".to_string()
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
            logging: LoggingSettings::default(),
            mcp: McpSettings::default(),
            skills: SkillsSettings::default(),
        }
    }
}

impl Default for SessionSettings {
    fn default() -> Self {
        Self {
            db_path: default_db_path(),
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
            system_prompt: Some(DEFAULT_SYSTEM_PROMPT.to_string()),
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

        let project_dir = self.cwd.clone();
        let project_settings = project_dir.join(".zerobot").join("settings.yaml");
        let local_settings = project_dir
            .join(".zerobot")
            .join("settings.local.yaml");

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

        let settings: Settings = serde_yaml::from_value(merged)
            .map_err(|err| ZeroBotError::Config(err.to_string()))?;

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
    let value = serde_yaml::from_str(&content)
        .map_err(|err| ZeroBotError::Config(err.to_string()))?;
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

        Err(ZeroBotError::Config(format!(
            "无法写入覆盖路径: {path}"
        )))
    }

    set_inner(target, &parts, value, path)
}

fn merge_yaml(base: YamlValue, overlay: YamlValue) -> YamlValue {
    match (base, overlay) {
        (YamlValue::Mapping(mut base_map), YamlValue::Mapping(overlay_map)) => {
            for (key, value) in overlay_map {
                let entry = base_map.remove(&key);
                let merged = if let Some(existing) = entry {
                    merge_yaml(existing, value)
                } else {
                    value
                };
                base_map.insert(key, merged);
            }
            YamlValue::Mapping(base_map)
        }
        (_, overlay) => overlay,
    }
}

fn default_state_dir() -> PathBuf {
    let home = home_dir();
    home.join(".zerobot").join("state")
}

fn user_settings_path() -> Option<PathBuf> {
    Some(home_dir().join(".zerobot").join("settings.yaml"))
}

fn managed_settings_path() -> Option<PathBuf> {
    if cfg!(windows) {
        let base = std::env::var("PROGRAMDATA").unwrap_or_else(|_| "C:\\ProgramData".to_string());
        return Some(PathBuf::from(base).join("ZeroBot").join("managed-settings.yaml"));
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

        let loader = ConfigLoader::new(cwd.to_path_buf()).with_cli_overrides(vec![
            ("default_provider".to_string(), "cli".to_string()),
        ]);
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
}
