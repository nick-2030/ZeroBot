use anyhow::Result;
use clap::{Parser, Subcommand};
use console::style;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{self, AsyncBufReadExt};
use tokio::sync::mpsc;
use tracing_subscriber::EnvFilter;
use zerobot_core::agent::Agent;
use zerobot_core::config::{ConfigLoader, Settings};
use zerobot_core::events::AgentEvent;
use zerobot_core::provider::{AnthropicProvider, OpenAIProvider, Provider};
use zerobot_core::session::{SessionStore, SqliteSessionStore};
use zerobot_core::tool::ToolRegistry;

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
    init_tracing();

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
            run_repl(&settings, &cwd, cli.provider, cli.model).await?;
        }
    }

    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
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
    let session = store
        .create_session("一次性执行".to_string())
        .await?;

    let provider = build_provider(settings, provider_override.as_deref())?;
    let model = resolve_model(settings, provider_override.as_deref(), model_override.as_deref())?;

    let agent = Agent::new(
        provider,
        model,
        settings.clone(),
        Arc::new(store),
        ToolRegistry::with_builtin(),
        cwd.clone(),
    );

    let output = agent.run_turn(&session.id, prompt, None).await?;
    println!("{output}");
    Ok(())
}

async fn run_repl(
    settings: &Settings,
    cwd: &PathBuf,
    provider_override: Option<String>,
    model_override: Option<String>,
) -> Result<()> {
    let store = SqliteSessionStore::new(expand_home(&settings.session.db_path)).await?;
    store.init().await?;
    let session = store
        .create_session("交互会话".to_string())
        .await?;

    println!("{} {}", style("会话已启动:").green(), session.id);
    println!("输入 /exit 退出");

    let provider = build_provider(settings, provider_override.as_deref())?;
    let model = resolve_model(settings, provider_override.as_deref(), model_override.as_deref())?;

    let agent = Agent::new(
        provider,
        model,
        settings.clone(),
        Arc::new(store),
        ToolRegistry::with_builtin(),
        cwd.clone(),
    );

    let stdin = io::BufReader::new(io::stdin());
    let mut lines = stdin.lines();

    while let Some(line) = lines.next_line().await? {
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }
        if line == "/exit" || line == "exit" {
            break;
        }

        let (tx, mut rx) = mpsc::unbounded_channel();
        let handler = tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                match event {
                    AgentEvent::ToolCallStarted { name } => {
                        println!("{} {}", style("调用工具:").cyan(), name);
                    }
                    AgentEvent::ToolCallFinished { name, output } => {
                        println!("{} {}", style("工具完成:").cyan(), name);
                        if !output.trim().is_empty() {
                            println!("{}\n{}", style("工具输出:").dim(), output);
                        }
                    }
                    AgentEvent::AssistantMessage { content } => {
                        println!("{} {}", style("助手:").green(), content);
                    }
                    AgentEvent::Error { message } => {
                        println!("{} {}", style("错误:").red(), message);
                    }
                    _ => {}
                }
            }
        });

        let result = agent.run_turn(&session.id, &line, Some(tx)).await;
        if let Err(err) = result {
            println!("{} {}", style("错误:").red(), err);
        }
        let _ = handler.await;
    }

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
