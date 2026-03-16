use anyhow::Result;
use clap::{Parser, Subcommand};
use console::style;
use std::path::PathBuf;
use std::sync::Arc;
use zerobot_core::agent::Agent;
use zerobot_core::config::{ConfigLoader, Settings};
use zerobot_core::logging::init_logging;
use zerobot_core::provider::{AnthropicProvider, OpenAIProvider, Provider};
use zerobot_core::session::{
    create_session_with_hooks,
    end_session_with_hooks,
    SessionStore,
    SqliteSessionStore,
};
use zerobot_core::tool::{SubagentTool, ToolRegistry};
use zerobot_core::ZeroBotError;

mod tui;

#[derive(Parser)]
#[command(name = "zerobot")]
#[command(about = "ZeroBot CLI", version = "0.1.0")]
struct Cli {
    #[arg(long = "set", value_name = "KEY=VALUE")]
    set: Vec<String>,
    #[arg(long)]
    provider: Option<String>,
    #[arg(long)]
    model: Option<String>,
    #[arg(long)]
    cwd: Option<PathBuf>,
    #[arg(long, default_value_t = false)]
    no_alt_screen: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    Exec { prompt: String },
    Session { #[command(subcommand)] cmd: SessionCmd },
    Config { #[command(subcommand)] cmd: ConfigCmd },
    Provider { #[command(subcommand)] cmd: ProviderCmd },
}

#[derive(Subcommand)]
enum SessionCmd {
    New { title: Option<String> },
    List,
    Show { id: String },
}

#[derive(Subcommand)]
enum ConfigCmd {
    Show,
    Layers,
}

#[derive(Subcommand)]
enum ProviderCmd {
    List,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let cwd = cli
        .cwd
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let overrides = parse_overrides(cli.set)?;

    let loader = ConfigLoader::new(cwd.clone()).with_cli_overrides(overrides);
    let loaded = loader.load()?;

    if !loaded.warnings.is_empty() {
        for warning in &loaded.warnings {
            eprintln!("{} {}", style("警告").yellow(), warning);
        }
    }

    let settings = loaded.settings.clone();

    match cli.command {
        Some(Command::Exec { prompt }) => {
            run_exec(&settings, &cwd, cli.provider, cli.model, &prompt).await?;
        }
        Some(Command::Session { cmd }) => {
            handle_session_cmd(&settings, cmd).await?;
        }
        Some(Command::Config { cmd }) => {
            handle_config_cmd(&loaded, cmd)?;
        }
        Some(Command::Provider { cmd }) => {
            handle_provider_cmd(&settings, cmd)?;
        }
        None => {
            run_repl(&settings, &cwd, cli.provider, cli.model, !cli.no_alt_screen).await?;
        }
    }

    Ok(())
}

fn parse_overrides(values: Vec<String>) -> Result<Vec<(String, String)>> {
    let mut overrides = Vec::new();
    for raw in values {
        if let Some((key, value)) = raw.split_once('=') {
            overrides.push((key.trim().to_string(), value.trim().to_string()));
        } else {
            anyhow::bail!("覆盖配置格式错误，需使用 KEY=VALUE: {raw}");
        }
    }
    Ok(overrides)
}

async fn handle_session_cmd(settings: &Settings, cmd: SessionCmd) -> Result<()> {
    let store = SqliteSessionStore::new(expand_home(&settings.session.db_path)).await?;
    store.init().await?;

    match cmd {
        SessionCmd::New { title } => {
            let session = store
                .create_session(title.unwrap_or_else(|| "新会话".to_string()))
                .await?;
            println!("{} {}", style("会话创建成功:").green(), session.id);
        }
        SessionCmd::List => {
            let sessions = store.list_sessions().await?;
            for session in sessions {
                println!("{}\t{}", session.id, session.title);
            }
        }
        SessionCmd::Show { id } => {
            let messages = store.list_messages(&id).await?;
            for message in messages {
                println!("[{}] {}", message.role.to_string(), message.content);
            }
        }
    }

    Ok(())
}

fn handle_config_cmd(loaded: &zerobot_core::config::LoadedConfig, cmd: ConfigCmd) -> Result<()> {
    match cmd {
        ConfigCmd::Show => {
            let yaml = serde_yaml::to_string(&loaded.settings)?;
            println!("{yaml}");
        }
        ConfigCmd::Layers => {
            for layer in &loaded.layers {
                let scope = format!("{:?}", layer.scope);
                let path = layer
                    .path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "<内置>".to_string());
                let applied = if layer.applied { "应用" } else { "跳过" };
                println!("{}\t{}\t{}", scope, applied, path);
                if let Some(reason) = &layer.reason {
                    println!("  原因: {reason}");
                }
            }
        }
    }
    Ok(())
}

fn handle_provider_cmd(settings: &Settings, cmd: ProviderCmd) -> Result<()> {
    match cmd {
        ProviderCmd::List => {
            for (name, info) in &settings.providers {
                println!("{}\t{}", name, info.kind);
            }
        }
    }
    Ok(())
}

async fn run_exec(
    settings: &Settings,
    cwd: &PathBuf,
    provider_override: Option<String>,
    model_override: Option<String>,
    prompt: &str,
) -> Result<()> {
    let store = SqliteSessionStore::new(expand_home(&settings.session.db_path)).await?;
    store.init().await?;
    let hooks = zerobot_core::hooks::HookManager::load(settings, cwd, None)?;
    let session = create_session_with_hooks(
        &store,
        &hooks,
        "一次性执行".to_string(),
        None,
        zerobot_core::session::SessionKind::Main,
    )
    .await?;
    let _log_guard = init_logging(settings, Some(&session.id))?;

    let model = resolve_model(settings, provider_override.as_deref(), model_override.as_deref())?;
    let store = Arc::new(store);
    let provider_factory = {
        let settings = settings.clone();
        let provider_override = provider_override.clone();
        Arc::new(move || {
            build_provider(&settings, provider_override.as_deref())
                .map_err(|err| ZeroBotError::Provider(err.to_string()))
        })
    };
    let provider = (provider_factory)()?;
    let mut tools = ToolRegistry::with_builtin_async(settings, cwd, Some(store.clone())).await?;
    let subagent_tools = tools.clone();
    tools.register(SubagentTool::new(
        settings.clone(),
        store.clone(),
        subagent_tools,
        cwd.clone(),
        provider_factory.clone(),
        model.clone(),
        hooks.clone(),
    ));
    let agent = Agent::new(
        provider,
        model,
        settings.clone(),
        store,
        tools,
        cwd.clone(),
        hooks.clone(),
    );

    let result = agent.run_turn(&session.id, prompt, None).await;
    end_session_with_hooks(&hooks, &session.id).await;
    let output = result?;
    println!("{output}");
    Ok(())
}

async fn run_repl(
    settings: &Settings,
    cwd: &PathBuf,
    provider_override: Option<String>,
    model_override: Option<String>,
    use_alt_screen: bool,
) -> Result<()> {
    let store = SqliteSessionStore::new(expand_home(&settings.session.db_path)).await?;
    store.init().await?;
    let hooks = zerobot_core::hooks::HookManager::load(settings, cwd, None)?;
    let session = create_session_with_hooks(
        &store,
        &hooks,
        "交互会话".to_string(),
        None,
        zerobot_core::session::SessionKind::Main,
    )
    .await?;
    let _log_guard = init_logging(settings, Some(&session.id))?;

    let model = resolve_model(settings, provider_override.as_deref(), model_override.as_deref())?;
    let store = Arc::new(store);
    let provider_factory = {
        let settings = settings.clone();
        let provider_override = provider_override.clone();
        Arc::new(move || {
            build_provider(&settings, provider_override.as_deref())
                .map_err(|err| ZeroBotError::Provider(err.to_string()))
        })
    };
    let mut tools = ToolRegistry::with_builtin_async(settings, cwd, Some(store.clone())).await?;
    let subagent_tools = tools.clone();
    tools.register(SubagentTool::new(
        settings.clone(),
        store.clone(),
        subagent_tools,
        cwd.clone(),
        provider_factory.clone(),
        model.clone(),
        hooks.clone(),
    ));
    let provider_id = resolve_provider_id(settings, provider_override.as_deref());
    tui::run_tui(
        settings.clone(),
        cwd.clone(),
        session.id.clone(),
        store.clone(),
        tools.clone(),
        provider_factory.clone(),
        model.clone(),
        provider_id,
        hooks.clone(),
        use_alt_screen,
    )
    .await?;

    end_session_with_hooks(&hooks, &session.id).await;
    Ok(())
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

    anyhow::bail!("未配置默认模型，请在 settings.yaml 中设置 default_model 或在 CLI 指定 --model")
}

fn resolve_provider_id(settings: &Settings, provider_override: Option<&str>) -> String {
    provider_override
        .map(|s| s.to_string())
        .or_else(|| settings.default_provider.clone())
        .unwrap_or_else(|| "openai".to_string())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_parses_exec() {
        let args = ["zerobot", "exec", "hello"];
        let cli = Cli::parse_from(args);
        match cli.command {
            Some(Command::Exec { prompt }) => assert_eq!(prompt, "hello"),
            _ => panic!("命令解析失败"),
        }
    }
}
