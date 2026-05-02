use std::path::PathBuf;
use std::sync::Arc;

use zerobot_core::hooks::HookManager;
use zerobot_core::interaction::InteractionHandler;
use zerobot_core::session::SessionStore;

pub use zerobot_core::config::PermissionMode;

/// Top-level configuration for creating a ZeroBot client.
///
/// Build via `Options::builder()`.
pub struct Options {
    pub(crate) cwd: PathBuf,
    pub(crate) model: Option<String>,
    pub(crate) provider: Option<String>,
    pub(crate) max_turns: Option<usize>,
    pub(crate) max_budget_usd: Option<f64>,
    pub(crate) permission_mode: Option<PermissionMode>,
    pub(crate) system_prompt: Option<String>,
    pub(crate) append_system_prompt: Option<String>,
    pub(crate) custom_tools: Vec<crate::tool::ToolDefinition>,
    pub(crate) interaction_handler: Option<Arc<dyn InteractionHandler>>,
    pub(crate) hooks: Option<HookManager>,
    pub(crate) session_store: Option<Arc<dyn SessionStore>>,
    pub(crate) cli_overrides: Vec<(String, String)>,
}

impl Options {
    pub fn builder() -> OptionsBuilder {
        OptionsBuilder::default()
    }
}

#[derive(Default)]
pub struct OptionsBuilder {
    cwd: Option<PathBuf>,
    model: Option<String>,
    provider: Option<String>,
    max_turns: Option<usize>,
    max_budget_usd: Option<f64>,
    permission_mode: Option<PermissionMode>,
    system_prompt: Option<String>,
    append_system_prompt: Option<String>,
    custom_tools: Vec<crate::tool::ToolDefinition>,
    interaction_handler: Option<Arc<dyn InteractionHandler>>,
    hooks: Option<HookManager>,
    session_store: Option<Arc<dyn SessionStore>>,
    cli_overrides: Vec<(String, String)>,
}

impl OptionsBuilder {
    /// Working directory. Defaults to `std::env::current_dir()`.
    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// Override the model (e.g. "claude-sonnet-4-20250514").
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Override the provider ID (e.g. "openai", "anthropic").
    pub fn provider(mut self, provider: impl Into<String>) -> Self {
        self.provider = Some(provider.into());
        self
    }

    /// Maximum agentic loop turns per query.
    pub fn max_turns(mut self, max: usize) -> Self {
        self.max_turns = Some(max);
        self
    }

    /// Maximum estimated USD spend per query.
    pub fn max_budget_usd(mut self, budget: f64) -> Self {
        self.max_budget_usd = Some(budget);
        self
    }

    /// Session-level permission mode override.
    pub fn permission_mode(mut self, mode: PermissionMode) -> Self {
        self.permission_mode = Some(mode);
        self
    }

    /// Replace the system prompt entirely.
    pub fn system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }

    /// Append text to the system prompt (after the default).
    pub fn append_system_prompt(mut self, text: impl Into<String>) -> Self {
        self.append_system_prompt = Some(text.into());
        self
    }

    /// Register a custom tool.
    pub fn tool(mut self, tool: crate::tool::ToolDefinition) -> Self {
        self.custom_tools.push(tool);
        self
    }

    /// Register multiple custom tools.
    pub fn tools(mut self, tools: Vec<crate::tool::ToolDefinition>) -> Self {
        self.custom_tools.extend(tools);
        self
    }

    /// Provide an InteractionHandler for tool approval prompts.
    pub fn interaction_handler(mut self, handler: Arc<dyn InteractionHandler>) -> Self {
        self.interaction_handler = Some(handler);
        self
    }

    /// Provide lifecycle hooks.
    pub fn hooks(mut self, hooks: HookManager) -> Self {
        self.hooks = Some(hooks);
        self
    }

    /// Provide a custom SessionStore (defaults to SqliteSessionStore from config).
    pub fn session_store(mut self, store: Arc<dyn SessionStore>) -> Self {
        self.session_store = Some(store);
        self
    }

    /// Add a CLI config override (KEY=VALUE style).
    pub fn cli_override(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.cli_overrides.push((key.into(), value.into()));
        self
    }

    pub fn build(self) -> Options {
        Options {
            cwd: self.cwd.unwrap_or_else(|| {
                std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
            }),
            model: self.model,
            provider: self.provider,
            max_turns: self.max_turns,
            max_budget_usd: self.max_budget_usd,
            permission_mode: self.permission_mode,
            system_prompt: self.system_prompt,
            append_system_prompt: self.append_system_prompt,
            custom_tools: self.custom_tools,
            interaction_handler: self.interaction_handler,
            hooks: self.hooks,
            session_store: self.session_store,
            cli_overrides: self.cli_overrides,
        }
    }
}
