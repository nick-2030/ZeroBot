pub mod abort;
pub mod engine;
pub mod error;
pub mod message;
pub mod options;
pub mod result;
pub mod session;
pub mod tool;

pub use abort::AbortHandle;
pub use error::{SdkError, SdkResult};
pub use message::SDKMessage;
pub use options::{Options, OptionsBuilder};
pub use result::QueryResult;
pub use session::SessionInfo;
pub use tool::ToolDefinition;

// Re-export key core types so callers don't need zerobot-core
pub use zerobot_core::config::PermissionMode;
pub use zerobot_core::interaction::{
    InteractionHandler, ToolApprovalDecision, ToolApprovalRequest, ToolApprovalResponse,
};
pub use zerobot_core::session::{Message, MessageRole, Session, SessionKind};

use std::collections::HashSet;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::RwLock;
use zerobot_core::config::ConfigLoader;
use zerobot_core::hooks::HookManager;
use zerobot_core::plugin::PluginManager;
use zerobot_core::session::{SessionStore, SqliteSessionStore};
use zerobot_core::tool::ToolRegistry;
use zerobot_core::workspace::{resolve_session_db_path, resolve_workspace_root};

mod helpers {
    use zerobot_core::config::Settings;
    use zerobot_core::provider::{AnthropicProvider, OpenAIProvider, Provider};

    pub(crate) fn build_provider(
        settings: &Settings,
        override_id: Option<&str>,
    ) -> anyhow::Result<Box<dyn Provider>> {
        let provider_id = override_id
            .map(|s| s.to_string())
            .or_else(|| settings.default_provider.clone())
            .unwrap_or_else(|| "openai".to_string());

        let info = settings.providers.get(&provider_id);
        let (kind, key, base_url) = if let Some(info) = info {
            (
                info.kind.clone(),
                resolve_api_key(info.api_key.clone(), info.api_key_env.clone(), &provider_id),
                info.base_url.clone(),
            )
        } else {
            (
                provider_id.clone(),
                resolve_api_key(None, None, &provider_id),
                None,
            )
        };

        match kind.as_str() {
            "openai" => Ok(Box::new(OpenAIProvider::new(key, base_url))),
            "anthropic" => Ok(Box::new(AnthropicProvider::new(key, base_url))),
            _ => anyhow::bail!("unsupported provider: {kind}"),
        }
    }

    pub(crate) fn resolve_api_key(
        api_key: Option<String>,
        api_key_env: Option<String>,
        provider_id: &str,
    ) -> String {
        if let Some(key) = api_key {
            return key;
        }
        if let Some(env) = api_key_env {
            if let Ok(value) = std::env::var(env) {
                return value;
            }
        }
        let env_name = match provider_id {
            "anthropic" => "ANTHROPIC_API_KEY",
            _ => "OPENAI_API_KEY",
        };
        std::env::var(env_name).unwrap_or_default()
    }

    pub(crate) fn resolve_model(
        settings: &Settings,
        provider_override: Option<&str>,
        model_override: Option<&str>,
    ) -> anyhow::Result<String> {
        if let Some(model) = model_override {
            return Ok(model.to_string());
        }
        let provider_id = provider_override
            .map(|s| s.to_string())
            .or_else(|| settings.default_provider.clone())
            .unwrap_or_else(|| "openai".to_string());
        if let Some(info) = settings.providers.get(&provider_id) {
            if let Some(model) = &info.model {
                return Ok(model.clone());
            }
        }
        if let Some(model) = &settings.default_model {
            return Ok(model.clone());
        }
        anyhow::bail!("no default model configured")
    }
}

/// The ZeroBot SDK client.
///
/// Created via `ZeroBot::new(options)` or `ZeroBot::from_default_config(cwd)`.
///
/// Provides:
/// - `query()` / `query_stream()` for one-shot queries
/// - `start_session()` / `resume_session()` for multi-turn sessions
/// - `list_sessions()` / `get_session()` / `rename_session()` / `delete_session()` for session management
/// - `shutdown()` for cleanup
pub struct ZeroBot {
    engine: engine::QueryEngine,
    plugins: Option<Arc<PluginManager>>,
}

impl ZeroBot {
    /// Create a new ZeroBot client from the given options.
    pub async fn new(options: Options) -> SdkResult<Self> {
        let loader = ConfigLoader::new(options.cwd.clone())
            .with_cli_overrides(options.cli_overrides.clone());
        let loaded = loader.load()?;
        let mut settings = loaded.settings;

        // Apply SDK-level overrides to settings
        if let Some(ref prompt) = options.system_prompt {
            settings.agent.system_prompt = Some(prompt.clone());
        } else if let Some(ref append) = options.append_system_prompt {
            let existing = settings.agent.system_prompt.unwrap_or_default();
            settings.agent.system_prompt = Some(format!("{existing}\n\n{append}"));
        }

        let workspace_root = resolve_workspace_root(&options.cwd);
        let db_path = resolve_session_db_path(&workspace_root);

        let store: Arc<dyn SessionStore> = if let Some(custom_store) = options.session_store {
            custom_store
        } else {
            let sqlite_store = SqliteSessionStore::new(db_path).await?;
            sqlite_store.init().await?;
            Arc::new(sqlite_store)
        };

        let approvals = store.list_tool_approvals().await.unwrap_or_default();
        let model = helpers::resolve_model(
            &settings,
            options.provider.as_deref(),
            options.model.as_deref(),
        )?;
        let hooks = match options.hooks {
            Some(h) => h,
            None => HookManager::load(&settings, &options.cwd, None)?,
        };
        let plugins = PluginManager::new(&settings, &options.cwd).await?;
        let mut tools =
            ToolRegistry::with_builtin_async(&settings, &options.cwd, Some(store.clone()), plugins.clone())
                .await?;

        // Register custom SDK tools
        for def in options.custom_tools {
            tools.register(tool::SdkToolAdapter::from_definition(def));
        }

        let engine = engine::QueryEngine {
            settings: settings.clone(),
            store: store.clone(),
            tools,
            hooks,
            cwd: options.cwd,
            model,
            plugins: plugins.clone(),
            tool_approvals: Arc::new(RwLock::new(
                approvals.into_iter().collect::<HashSet<_>>(),
            )),
            max_turns: options.max_turns,
            max_budget_usd: options.max_budget_usd,
        };

        Ok(Self { engine, plugins })
    }

    /// Create from default config (backward compatible).
    pub async fn from_default_config(cwd: PathBuf) -> SdkResult<Self> {
        Self::new(Options::builder().cwd(cwd).build()).await
    }

    /// Shutdown background resources (plugins, MCP clients).
    pub async fn shutdown(&self) {
        if let Some(plugins) = &self.plugins {
            plugins.shutdown().await;
        }
    }

    // -- Query methods --

    /// Execute a one-shot query and return the structured result.
    pub async fn query(
        &self,
        session_id: &str,
        input: &str,
        abort: Option<&AbortHandle>,
    ) -> SdkResult<QueryResult> {
        self.engine.query(session_id, input, abort).await
    }

    /// Execute a query and return a stream of typed `SDKMessage`s.
    pub fn query_stream(
        &self,
        session_id: &str,
        input: &str,
        abort: Option<AbortHandle>,
    ) -> SdkResult<Pin<Box<dyn futures::Stream<Item = SDKMessage> + Send>>> {
        self.engine.query_stream(session_id, input, abort)
    }

    // -- Session lifecycle --

    /// Start a new session. Returns a `SessionHandle` for multi-turn conversations.
    pub async fn start_session(&self, title: Option<String>) -> SdkResult<SessionHandle<'_>> {
        let session = zerobot_core::session::create_session_with_hooks(
            self.engine.store.as_ref(),
            &self.engine.hooks,
            title.unwrap_or_else(|| "New Session".to_string()),
            None,
            zerobot_core::session::SessionKind::Main,
        )
        .await?;
        Ok(SessionHandle {
            engine: &self.engine,
            session,
        })
    }

    /// Resume an existing session.
    pub async fn resume_session(&self, session_id: &str) -> SdkResult<SessionHandle<'_>> {
        let session = self
            .engine
            .store
            .get_session(session_id)
            .await?
            .ok_or_else(|| SdkError::Session(format!("session not found: {session_id}")))?;
        Ok(SessionHandle {
            engine: &self.engine,
            session,
        })
    }

    // -- Session management --

    /// List all sessions in the workspace.
    pub async fn list_sessions(&self) -> SdkResult<Vec<SessionInfo>> {
        let sessions = self.engine.store.list_sessions().await?;
        Ok(sessions.into_iter().map(SessionInfo::from).collect())
    }

    /// Get a specific session by ID.
    pub async fn get_session(&self, id: &str) -> SdkResult<Option<SessionInfo>> {
        let session = self.engine.store.get_session(id).await?;
        Ok(session.map(SessionInfo::from))
    }

    /// Rename a session.
    pub async fn rename_session(&self, session_id: &str, new_title: &str) -> SdkResult<()> {
        self.engine
            .store
            .update_session_title(session_id, new_title)
            .await?;
        Ok(())
    }

    /// Delete a session.
    pub async fn delete_session(&self, session_id: &str) -> SdkResult<()> {
        self.engine.store.delete_session(session_id).await?;
        Ok(())
    }
}

/// Per-session handle for multi-turn conversations.
///
/// Backward-compatible with the old API: `run()` and `run_stream()` still work.
/// New callers should prefer `ZeroBot::query()` / `ZeroBot::query_stream()` directly.
pub struct SessionHandle<'a> {
    engine: &'a engine::QueryEngine,
    pub session: Session,
}

impl<'a> SessionHandle<'a> {
    /// Run a turn and get the final text response.
    pub async fn run(&self, input: &str) -> SdkResult<String> {
        let result = self.engine.query(&self.session.id, input, None).await?;
        if result.is_error {
            Err(SdkError::Agent(result.error.unwrap_or_default()))
        } else {
            Ok(result.response)
        }
    }

    /// Run a turn with streaming events (backward compatible).
    /// Returns the raw core `AgentEvent` stream.
    pub async fn run_stream(
        &self,
        input: &str,
    ) -> SdkResult<tokio::sync::mpsc::UnboundedReceiver<zerobot_core::events::AgentEvent>> {
        let agent = self.engine.build_agent()?;
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let session_id = self.session.id.clone();
        let input = input.to_string();
        tokio::spawn(async move {
            let _ = agent.run_turn(&session_id, &input, Some(tx)).await;
        });
        Ok(rx)
    }

    /// Run a turn and get the typed `SDKMessage` stream.
    pub fn query_stream(
        &self,
        input: &str,
        abort: Option<AbortHandle>,
    ) -> SdkResult<Pin<Box<dyn futures::Stream<Item = SDKMessage> + Send>>> {
        self.engine.query_stream(&self.session.id, input, abort)
    }
}
