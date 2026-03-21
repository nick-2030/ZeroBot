use anyhow::Result;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::RwLock;
use zerobot_core::agent::Agent;
use zerobot_core::config::{ConfigLoader, Settings};
use zerobot_core::events::AgentEvent;
use zerobot_core::plugin::PluginManager;
use zerobot_core::provider::{AnthropicProvider, OpenAIProvider, Provider};
use zerobot_core::session::{create_session_with_hooks, Session, SessionStore, SqliteSessionStore};
use zerobot_core::tool::{SubagentTool, ToolRegistry};
use zerobot_core::workspace::{resolve_session_db_path, resolve_workspace_root};
use zerobot_core::ZeroBotError;

pub struct ZeroBot {
    settings: Settings,
    store: Arc<dyn SessionStore>,
    tools: ToolRegistry,
    hooks: zerobot_core::hooks::HookManager,
    cwd: PathBuf,
    model: String,
    plugins: Option<Arc<PluginManager>>,
    tool_approvals: Arc<RwLock<HashSet<String>>>,
}

impl ZeroBot {
    pub async fn from_default_config(cwd: PathBuf) -> Result<Self> {
        let loader = ConfigLoader::new(cwd.clone());
        let loaded = loader.load()?;
        let settings = loaded.settings;
        let workspace_root = resolve_workspace_root(&cwd);
        let db_path = resolve_session_db_path(&workspace_root);
        let store = SqliteSessionStore::new(db_path).await?;
        store.init().await?;
        let approvals = store.list_tool_approvals().await.unwrap_or_default();
        let store = Arc::new(store);
        let model = resolve_model(&settings, None, None)?;
        let hooks = zerobot_core::hooks::HookManager::load(&settings, &cwd, None)?;
        let plugins = PluginManager::new(&settings, &cwd).await?;
        let tools =
            ToolRegistry::with_builtin_async(&settings, &cwd, Some(store.clone()), plugins.clone())
                .await?;
        Ok(Self {
            settings: settings.clone(),
            store: store.clone(),
            tools,
            hooks,
            cwd,
            model,
            plugins,
            tool_approvals: Arc::new(RwLock::new(approvals.into_iter().collect::<HashSet<_>>())),
        })
    }

    pub async fn shutdown(&self) {
        if let Some(plugins) = &self.plugins {
            plugins.shutdown().await;
        }
    }

    pub async fn start_session(&self, title: Option<String>) -> Result<SessionHandle> {
        let session = create_session_with_hooks(
            self.store.as_ref(),
            &self.hooks,
            title.unwrap_or_else(|| "新会话".to_string()),
            None,
            zerobot_core::session::SessionKind::Main,
        )
        .await?;
        Ok(SessionHandle {
            session,
            settings: self.settings.clone(),
            store: self.store.clone(),
            tools: self.tools.clone(),
            hooks: self.hooks.clone(),
            cwd: self.cwd.clone(),
            model: self.model.clone(),
            plugins: self.plugins.clone(),
            tool_approvals: self.tool_approvals.clone(),
        })
    }

    pub async fn resume_session(&self, session_id: &str) -> Result<SessionHandle> {
        let session = self
            .store
            .get_session(session_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("会话不存在"))?;
        Ok(SessionHandle {
            session,
            settings: self.settings.clone(),
            store: self.store.clone(),
            tools: self.tools.clone(),
            hooks: self.hooks.clone(),
            cwd: self.cwd.clone(),
            model: self.model.clone(),
            plugins: self.plugins.clone(),
            tool_approvals: self.tool_approvals.clone(),
        })
    }
}

#[derive(Clone)]
pub struct SessionHandle {
    pub session: Session,
    settings: Settings,
    store: Arc<dyn SessionStore>,
    tools: ToolRegistry,
    hooks: zerobot_core::hooks::HookManager,
    cwd: PathBuf,
    model: String,
    plugins: Option<Arc<PluginManager>>,
    tool_approvals: Arc<RwLock<HashSet<String>>>,
}

impl SessionHandle {
    pub async fn run(&self, input: &str) -> Result<String> {
        let provider_factory = {
            let settings = self.settings.clone();
            Arc::new(move || {
                build_provider(&settings, None)
                    .map_err(|err| ZeroBotError::Provider(err.to_string()))
            })
        };
        let provider = (provider_factory)()?;
        let mut tools = self.tools.clone();
        let subagent_tools = tools.clone();
        tools.register(SubagentTool::new(
            self.settings.clone(),
            self.store.clone(),
            subagent_tools,
            self.cwd.clone(),
            provider_factory.clone(),
            self.model.clone(),
            self.hooks.clone(),
            None,
            self.tool_approvals.clone(),
        ));
        let agent = Agent::new(
            provider,
            self.model.clone(),
            self.settings.clone(),
            self.store.clone(),
            tools,
            self.cwd.clone(),
            self.hooks.clone(),
            None,
            self.plugins.clone(),
            self.tool_approvals.clone(),
            None,
            None,
        );
        let output = agent.run_turn(&self.session.id, input, None).await?;
        Ok(output)
    }

    pub async fn run_stream(&self, input: &str) -> Result<mpsc::UnboundedReceiver<AgentEvent>> {
        let provider_factory = {
            let settings = self.settings.clone();
            Arc::new(move || {
                build_provider(&settings, None)
                    .map_err(|err| ZeroBotError::Provider(err.to_string()))
            })
        };
        let provider = (provider_factory)()?;
        let mut tools = self.tools.clone();
        let subagent_tools = tools.clone();
        tools.register(SubagentTool::new(
            self.settings.clone(),
            self.store.clone(),
            subagent_tools,
            self.cwd.clone(),
            provider_factory.clone(),
            self.model.clone(),
            self.hooks.clone(),
            None,
            self.tool_approvals.clone(),
        ));
        let agent = Agent::new(
            provider,
            self.model.clone(),
            self.settings.clone(),
            self.store.clone(),
            tools,
            self.cwd.clone(),
            self.hooks.clone(),
            None,
            self.plugins.clone(),
            self.tool_approvals.clone(),
            None,
            None,
        );
        let (tx, rx) = mpsc::unbounded_channel();
        let session_id = self.session.id.clone();
        let input = input.to_string();
        tokio::spawn(async move {
            let _ = agent.run_turn(&session_id, &input, Some(tx)).await;
        });
        Ok(rx)
    }
}

fn build_provider(settings: &Settings, override_id: Option<&str>) -> Result<Box<dyn Provider>> {
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
        _ => anyhow::bail!("不支持的提供商类型: {kind}"),
    }
}

fn resolve_model(
    settings: &Settings,
    provider_override: Option<&str>,
    model_override: Option<&str>,
) -> Result<String> {
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

    anyhow::bail!("未配置默认模型")
}

fn resolve_api_key(
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
