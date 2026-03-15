use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;
use zerobot_core::agent::Agent;
use zerobot_core::config::{ConfigLoader, Settings};
use zerobot_core::events::AgentEvent;
use zerobot_core::provider::{AnthropicProvider, OpenAIProvider, Provider};
use zerobot_core::session::{Session, SessionStore, SqliteSessionStore};
use zerobot_core::tool::ToolRegistry;

pub struct ZeroBot {
    settings: Settings,
    store: Arc<dyn SessionStore>,
    tools: ToolRegistry,
    cwd: PathBuf,
    model: String,
}

impl ZeroBot {
    pub async fn from_default_config(cwd: PathBuf) -> Result<Self> {
        let loader = ConfigLoader::new(cwd.clone());
        let loaded = loader.load()?;
        let settings = loaded.settings;
        let store = SqliteSessionStore::new(expand_home(&settings.session.db_path)).await?;
        store.init().await?;
        let model = resolve_model(&settings, None, None)?;
        Ok(Self {
            settings,
            store: Arc::new(store),
            tools: ToolRegistry::with_builtin(),
            cwd,
            model,
        })
    }

    pub async fn start_session(&self, title: Option<String>) -> Result<SessionHandle> {
        let session = self
            .store
            .create_session(title.unwrap_or_else(|| "新会话".to_string()))
            .await?;
        Ok(SessionHandle {
            session,
            settings: self.settings.clone(),
            store: self.store.clone(),
            tools: self.tools.clone(),
            cwd: self.cwd.clone(),
            model: self.model.clone(),
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
            cwd: self.cwd.clone(),
            model: self.model.clone(),
        })
    }
}

#[derive(Clone)]
pub struct SessionHandle {
    pub session: Session,
    settings: Settings,
    store: Arc<dyn SessionStore>,
    tools: ToolRegistry,
    cwd: PathBuf,
    model: String,
}

impl SessionHandle {
    pub async fn run(&self, input: &str) -> Result<String> {
        let agent = Agent::new(
            build_provider(&self.settings, None)?,
            self.model.clone(),
            self.settings.clone(),
            self.store.clone(),
            self.tools.clone(),
            self.cwd.clone(),
        );
        let output = agent.run_turn(&self.session.id, input, None).await?;
        Ok(output)
    }

    pub async fn run_stream(&self, input: &str) -> Result<mpsc::UnboundedReceiver<AgentEvent>> {
        let agent = Agent::new(
            build_provider(&self.settings, None)?,
            self.model.clone(),
            self.settings.clone(),
            self.store.clone(),
            self.tools.clone(),
            self.cwd.clone(),
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

fn resolve_api_key(api_key: Option<String>, api_key_env: Option<String>, provider_id: &str) -> String {
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

fn expand_home(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}
