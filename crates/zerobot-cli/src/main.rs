use anyhow::Result;
use chrono::TimeZone;
use clap::{Parser, Subcommand};
use console::style;
use futures::FutureExt;
use serde_json::json;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, RwLock as StdRwLock};
use tokio::sync::mpsc;
use tokio::sync::RwLock as TokioRwLock;
use tokio::task::LocalSet;
use zerobot_core::acp::{run_stdio as run_acp_stdio, AcpServerConfig};
use zerobot_core::agent::Agent;
use zerobot_core::bus::OutboundMessage;
use zerobot_core::channel::{feishu::FeishuChannel, ChatChannel};
use zerobot_core::config::{ConfigLoader, Settings};
use zerobot_core::cron::{CronPayload, CronSchedule, CronScheduleKind, CronService};
use zerobot_core::gateway::GatewayRuntime;
use zerobot_core::heartbeat::HeartbeatService;
use zerobot_core::logging::{init_logging, init_logging_with_stdout};
use zerobot_core::plugin::PluginManager;
use zerobot_core::provider::{AnthropicProvider, OpenAIProvider, Provider};
use zerobot_core::session::{
    create_session_with_hooks, end_session_with_hooks, SessionStore, SqliteSessionStore,
};
use zerobot_core::tool::{SubagentTool, ToolRegistry};
use zerobot_core::workspace::{resolve_session_db_path, resolve_workspace_root};
use zerobot_core::ZeroBotError;

mod slash;
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
    #[arg(long)]
    resume: Option<String>,
    #[arg(long, default_value_t = false)]
    no_alt_screen: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    Exec {
        prompt: String,
    },
    Session {
        #[command(subcommand)]
        cmd: SessionCmd,
    },
    Config {
        #[command(subcommand)]
        cmd: ConfigCmd,
    },
    Provider {
        #[command(subcommand)]
        cmd: ProviderCmd,
    },
    Gateway,
    Cron {
        #[command(subcommand)]
        cmd: CronCmd,
    },
    Heartbeat {
        #[command(subcommand)]
        cmd: HeartbeatCmd,
    },
    Acp {
        #[arg(long)]
        cwd: Option<PathBuf>,
        #[arg(long)]
        provider: Option<String>,
        #[arg(long)]
        model: Option<String>,
    },
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
    AuthMethods {
        provider: String,
    },
    AuthAuthorize {
        provider: String,
        plugin: String,
        method_index: usize,
        #[arg(long = "input", value_name = "KEY=VALUE")]
        inputs: Vec<String>,
    },
    AuthCallback {
        provider: String,
        plugin: String,
        method_index: usize,
        #[arg(long)]
        code: Option<String>,
    },
}

#[derive(Subcommand)]
enum CronCmd {
    List {
        #[arg(long, default_value_t = false)]
        all: bool,
    },
    Add {
        name: String,
        #[arg(long)]
        message: String,
        #[arg(long)]
        every_seconds: Option<i64>,
        #[arg(long)]
        cron_expr: Option<String>,
        #[arg(long)]
        tz: Option<String>,
        #[arg(long)]
        at: Option<String>,
        #[arg(long, default_value_t = false)]
        deliver: bool,
        #[arg(long)]
        channel: Option<String>,
        #[arg(long)]
        to: Option<String>,
        #[arg(long, default_value_t = false)]
        delete_after_run: bool,
    },
    Remove {
        id: String,
    },
    Enable {
        id: String,
    },
    Disable {
        id: String,
    },
    Run {
        id: String,
        #[arg(long, default_value_t = false)]
        force: bool,
    },
    Status,
    Export,
}

#[derive(Subcommand)]
enum HeartbeatCmd {
    Trigger,
    Status,
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

    if cli.command.is_some() && cli.resume.is_some() {
        anyhow::bail!("--resume 只能在交互模式使用");
    }

    match cli.command {
        Some(Command::Exec { prompt }) => {
            run_exec(&settings, &cwd, cli.provider, cli.model, &prompt).await?;
        }
        Some(Command::Session { cmd }) => {
            handle_session_cmd(&settings, &cwd, cmd).await?;
        }
        Some(Command::Config { cmd }) => {
            handle_config_cmd(&loaded, cmd)?;
        }
        Some(Command::Provider { cmd }) => {
            handle_provider_cmd(&settings, &cwd, cmd).await?;
        }
        Some(Command::Gateway) => {
            run_gateway(&settings, &cwd, cli.provider, cli.model).await?;
        }
        Some(Command::Cron { cmd }) => {
            handle_cron_cmd(&settings, &cwd, cmd).await?;
        }
        Some(Command::Heartbeat { cmd }) => {
            handle_heartbeat_cmd(&settings, &cwd, cli.provider, cli.model, cmd).await?;
        }
        Some(Command::Acp {
            cwd: acp_cwd,
            provider: acp_provider,
            model: acp_model,
        }) => {
            let effective_cwd = acp_cwd.unwrap_or(cwd);
            run_acp(
                &settings,
                &effective_cwd,
                acp_provider.or(cli.provider),
                acp_model.or(cli.model),
            )
            .await?;
        }
        None => {
            run_repl(
                &settings,
                &cwd,
                cli.provider,
                cli.model,
                cli.resume,
                !cli.no_alt_screen,
            )
            .await?;
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

fn parse_key_values(values: Vec<String>) -> Result<std::collections::HashMap<String, String>> {
    let mut map = std::collections::HashMap::new();
    for raw in values {
        if let Some((key, value)) = raw.split_once('=') {
            map.insert(key.trim().to_string(), value.trim().to_string());
        } else {
            anyhow::bail!("键值格式错误，需使用 KEY=VALUE: {raw}");
        }
    }
    Ok(map)
}

async fn handle_session_cmd(_settings: &Settings, cwd: &PathBuf, cmd: SessionCmd) -> Result<()> {
    let workspace_root = resolve_workspace_root(cwd);
    let db_path = resolve_session_db_path(&workspace_root);
    let store = SqliteSessionStore::new(db_path).await?;
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
                let summary = session_summary_for_display(&store, &session).await;
                if summary.is_empty() {
                    println!("{}  {}", session.id, session.title);
                } else {
                    println!("{}  {}  {}", session.id, session.title, summary);
                }
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

async fn handle_provider_cmd(settings: &Settings, cwd: &PathBuf, cmd: ProviderCmd) -> Result<()> {
    match cmd {
        ProviderCmd::List => {
            for (name, info) in &settings.providers {
                println!("{}\t{}", name, info.kind);
            }
        }
        ProviderCmd::AuthMethods { provider } => {
            let manager = PluginManager::new(settings, cwd).await?;
            let Some(manager) = manager else {
                println!("no plugin auth methods");
                return Ok(());
            };
            let methods = manager.list_auth_methods(&provider).await?;
            if methods.is_empty() {
                println!("no plugin auth methods");
            } else {
                for method in methods {
                    println!(
                        "{}\t{}\t{}\t{}\t{}",
                        method.provider,
                        method.plugin,
                        method.index,
                        method.method_type,
                        method.label
                    );
                }
            }
            manager.shutdown().await;
        }
        ProviderCmd::AuthAuthorize {
            provider,
            plugin,
            method_index,
            inputs,
        } => {
            let manager = PluginManager::new(settings, cwd).await?;
            let Some(manager) = manager else {
                anyhow::bail!("插件系统未启用");
            };
            let parsed = parse_key_values(inputs)?;
            let result = manager
                .authorize(&provider, &plugin, method_index, parsed)
                .await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            manager.shutdown().await;
        }
        ProviderCmd::AuthCallback {
            provider,
            plugin,
            method_index,
            code,
        } => {
            let manager = PluginManager::new(settings, cwd).await?;
            let Some(manager) = manager else {
                anyhow::bail!("插件系统未启用");
            };
            let result = manager
                .callback(&provider, &plugin, method_index, code)
                .await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            manager.shutdown().await;
        }
    }
    Ok(())
}

async fn run_exec(
    _settings: &Settings,
    cwd: &PathBuf,
    provider_override: Option<String>,
    model_override: Option<String>,
    prompt: &str,
) -> Result<()> {
    let mut builder = zerobot_sdk::Options::builder().cwd(cwd.clone());
    if let Some(provider) = provider_override {
        builder = builder.provider(provider);
    }
    if let Some(model) = model_override {
        builder = builder.model(model);
    }
    let client = zerobot_sdk::ZeroBot::new(builder.build()).await?;
    let session = client.start_session(Some("一次性执行".to_string())).await?;
    let result = session.run(prompt).await?;
    client.shutdown().await;
    println!("{result}");
    Ok(())
}

async fn run_repl(
    settings: &Settings,
    cwd: &PathBuf,
    provider_override: Option<String>,
    model_override: Option<String>,
    resume_id: Option<String>,
    use_alt_screen: bool,
) -> Result<()> {
    let workspace_root = resolve_workspace_root(cwd);
    let db_path = resolve_session_db_path(&workspace_root);
    let store = SqliteSessionStore::new(db_path).await?;
    store.init().await?;
    let hooks = zerobot_core::hooks::HookManager::load(settings, cwd, None)?;
    let session_id = if let Some(resume) = resume_id.clone() {
        let exists = store
            .get_session(&resume)
            .await?
            .ok_or_else(|| anyhow::anyhow!("会话不存在: {resume}"))?;
        exists.id
    } else {
        create_session_with_hooks(
            &store,
            &hooks,
            "交互会话".to_string(),
            None,
            zerobot_core::session::SessionKind::Main,
        )
        .await?
        .id
    };
    let _log_guard = init_logging_with_stdout(settings, Some(&session_id), false)?;

    let model = resolve_model(
        settings,
        provider_override.as_deref(),
        model_override.as_deref(),
    )?;
    let approvals = store.list_tool_approvals().await.unwrap_or_default();
    let store = Arc::new(store);
    let provider_state = Arc::new(StdRwLock::new(resolve_provider_id(
        settings,
        provider_override.as_deref(),
    )));
    let provider_factory = {
        let settings = settings.clone();
        let provider_state = provider_state.clone();
        Arc::new(move || {
            let current = provider_state
                .read()
                .map_err(|_| ZeroBotError::Provider("provider 状态锁已损坏".to_string()))?
                .clone();
            build_provider(&settings, Some(&current))
                .map_err(|err| ZeroBotError::Provider(err.to_string()))
        })
    };
    let tool_approvals = Arc::new(TokioRwLock::new(
        approvals.into_iter().collect::<HashSet<_>>(),
    ));
    let plugins = PluginManager::new(settings, cwd).await?;
    let mut tools =
        ToolRegistry::with_builtin_async(settings, cwd, Some(store.clone()), plugins.clone())
            .await?;
    let subagent_tools = tools.clone();
    tools.register(SubagentTool::new(
        settings.clone(),
        store.clone(),
        subagent_tools,
        cwd.clone(),
        provider_factory.clone(),
        model.clone(),
        hooks.clone(),
        None,
        tool_approvals.clone(),
    ));
    let provider_id = resolve_provider_id(settings, provider_override.as_deref());
    let final_session_id = tui::run_tui(
        settings.clone(),
        cwd.clone(),
        session_id.clone(),
        store.clone(),
        tools.clone(),
        provider_factory.clone(),
        model.clone(),
        provider_id,
        hooks.clone(),
        resume_id.is_some(),
        use_alt_screen,
        provider_state.clone(),
        plugins.clone(),
        tool_approvals.clone(),
    )
    .await?;

    let messages = store.list_messages(&final_session_id).await?;
    if messages.is_empty() {
        store.delete_session(&final_session_id).await?;
    } else {
        end_session_with_hooks(&hooks, &final_session_id).await;
        println!("恢复本会话: zerobot --resume {}", final_session_id);
    }
    if let Some(plugins) = &plugins {
        plugins.shutdown().await;
    }
    Ok(())
}

async fn run_gateway(
    settings: &Settings,
    cwd: &PathBuf,
    provider_override: Option<String>,
    model_override: Option<String>,
) -> Result<()> {
    let _log_guard = init_logging_with_stdout(settings, Some("gateway"), true)?;
    tracing::info!("gateway 启动中, cwd={}", cwd.display());

    let workspace_root = resolve_workspace_root(cwd);
    let db_path = resolve_session_db_path(&workspace_root);
    let store = SqliteSessionStore::new(db_path).await?;
    store.init().await?;
    let hooks = zerobot_core::hooks::HookManager::load(settings, cwd, None)?;

    let model = resolve_model(
        settings,
        provider_override.as_deref(),
        model_override.as_deref(),
    )?;
    let approvals = store.list_tool_approvals().await.unwrap_or_default();
    let store = Arc::new(store);
    let provider_factory = {
        let settings = settings.clone();
        let provider_override = provider_override.clone();
        Arc::new(move || {
            build_provider(&settings, provider_override.as_deref())
                .map_err(|err| ZeroBotError::Provider(err.to_string()))
        })
    };
    let tool_approvals = Arc::new(TokioRwLock::new(
        approvals.into_iter().collect::<HashSet<_>>(),
    ));
    let plugins = PluginManager::new(settings, cwd).await?;
    let mut tools =
        ToolRegistry::with_builtin_async(settings, cwd, Some(store.clone()), plugins.clone())
            .await?;
    let subagent_tools = tools.clone();
    tools.register(SubagentTool::new(
        settings.clone(),
        store.clone(),
        subagent_tools,
        cwd.clone(),
        provider_factory.clone(),
        model.clone(),
        hooks.clone(),
        None,
        tool_approvals.clone(),
    ));

    let mut runtime = GatewayRuntime::new(
        settings.clone(),
        cwd.clone(),
        store,
        tools,
        hooks,
        plugins,
        provider_factory,
        model,
        tool_approvals,
    )
    .await?;

    tracing::info!("gateway 启动完成，进入事件循环");
    println!("{}", style("gateway 已启动，按 Ctrl+C 结束").green());
    runtime.run().await?;
    tracing::info!("gateway 事件循环已退出");
    Ok(())
}

async fn run_acp(
    settings: &Settings,
    cwd: &PathBuf,
    provider_override: Option<String>,
    model_override: Option<String>,
) -> Result<()> {
    let _log_guard = init_logging_with_stdout(settings, Some("acp"), true)?;
    tracing::info!("acp 启动中, cwd={}", cwd.display());

    let workspace_root = resolve_workspace_root(cwd);
    let db_path = resolve_session_db_path(&workspace_root);
    let store = SqliteSessionStore::new(db_path).await?;
    store.init().await?;
    let approvals = store.list_tool_approvals().await.unwrap_or_default();
    let store = Arc::new(store);

    let hooks = zerobot_core::hooks::HookManager::load(settings, cwd, None)?;
    let plugins = PluginManager::new(settings, cwd).await?;
    let plugins_for_shutdown = plugins.clone();

    let default_provider = resolve_provider_id(settings, provider_override.as_deref());
    let default_model = resolve_model(
        settings,
        Some(default_provider.as_str()),
        model_override.as_deref(),
    )?;
    let tool_approvals = Arc::new(TokioRwLock::new(
        approvals.into_iter().collect::<HashSet<_>>(),
    ));

    let config = AcpServerConfig {
        settings: settings.clone(),
        cwd: cwd.clone(),
        store,
        base_hooks: hooks,
        plugins,
        tool_approvals,
        default_provider,
        default_model,
    };

    let local_set = LocalSet::new();
    let result = local_set
        .run_until(async move { run_acp_stdio(config).await })
        .await;

    if let Some(plugins) = &plugins_for_shutdown {
        plugins.shutdown().await;
    }

    result?;
    Ok(())
}

async fn build_cron_service(settings: &Settings, cwd: &PathBuf) -> Result<CronService> {
    let workspace_root = resolve_workspace_root(cwd);
    let db_path = resolve_session_db_path(&workspace_root);
    let export_path = settings
        .gateway
        .cron
        .export_json
        .as_ref()
        .map(PathBuf::from)
        .or_else(|| Some(workspace_root.join(".zerobot").join("cron-jobs.json")));
    let service = CronService::new(
        db_path,
        export_path,
        settings.gateway.cron.run_history_limit,
    )
    .await?;
    Ok(service)
}

async fn handle_cron_cmd(settings: &Settings, cwd: &PathBuf, cmd: CronCmd) -> Result<()> {
    let service = build_cron_service(settings, cwd).await?;
    match cmd {
        CronCmd::List { all } => {
            let jobs = service.list_jobs(all).await?;
            println!("{}", serde_json::to_string_pretty(&jobs)?);
        }
        CronCmd::Add {
            name,
            message,
            every_seconds,
            cron_expr,
            tz,
            at,
            deliver,
            channel,
            to,
            delete_after_run,
        } => {
            let configured = usize::from(every_seconds.is_some())
                + usize::from(cron_expr.is_some())
                + usize::from(at.is_some());
            if configured != 1 {
                anyhow::bail!("cron add 需要且只能指定一种调度：every_seconds / cron_expr / at");
            }

            let schedule = if let Some(seconds) = every_seconds {
                if seconds <= 0 {
                    anyhow::bail!("every_seconds 必须大于 0");
                }
                CronSchedule {
                    kind: CronScheduleKind::Every,
                    at_ms: None,
                    every_ms: Some(seconds.saturating_mul(1000)),
                    expr: None,
                    tz: None,
                }
            } else if let Some(expr) = cron_expr {
                CronSchedule::cron(expr, tz)
            } else {
                CronSchedule::at(parse_at_to_millis(
                    &at.ok_or_else(|| anyhow::anyhow!("cron add 缺少 at"))?,
                )?)
            };

            let payload = CronPayload {
                kind: "agent_turn".to_string(),
                message,
                deliver,
                channel,
                to,
            };
            let job = service
                .add_job(name, schedule, payload, delete_after_run)
                .await?;
            println!("{}", serde_json::to_string_pretty(&job)?);
        }
        CronCmd::Remove { id } => {
            let removed = service.remove_job(&id).await?;
            println!("{}", if removed { "removed" } else { "not_found" });
        }
        CronCmd::Enable { id } => {
            let job = service.enable_job(&id, true).await?;
            println!("{}", serde_json::to_string_pretty(&job)?);
        }
        CronCmd::Disable { id } => {
            let job = service.enable_job(&id, false).await?;
            println!("{}", serde_json::to_string_pretty(&job)?);
        }
        CronCmd::Run { id, force } => {
            let ok = service.run_job(&id, force).await?;
            println!("{}", if ok { "ok" } else { "not_found_or_disabled" });
        }
        CronCmd::Status => {
            let status = service.status().await?;
            println!("{}", serde_json::to_string_pretty(&status)?);
        }
        CronCmd::Export => {
            let path = service.export_snapshot().await?;
            println!(
                "{}",
                path.map(|p| p.display().to_string())
                    .unwrap_or_else(|| "disabled".to_string())
            );
        }
    }
    Ok(())
}

fn parse_at_to_millis(raw: &str) -> Result<i64> {
    if let Ok(ms) = raw.parse::<i64>() {
        return Ok(ms);
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(raw) {
        return Ok(dt.timestamp_millis());
    }
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S") {
        let local = chrono::Local
            .from_local_datetime(&dt)
            .single()
            .ok_or_else(|| anyhow::anyhow!("at 时间非法"))?;
        return Ok(local.timestamp_millis());
    }
    anyhow::bail!("at 格式非法，支持 RFC3339 或毫秒时间戳")
}

async fn send_channel_message(
    settings: &Settings,
    channel: &str,
    chat_id: &str,
    content: String,
) -> Result<()> {
    match channel {
        "feishu" => {
            if !settings.channels.feishu.enabled {
                anyhow::bail!("channels.feishu.enabled=false，无法投递到飞书");
            }
            let (tx, _rx) = mpsc::unbounded_channel();
            let feishu = FeishuChannel::new(settings.channels.feishu.clone(), tx);
            feishu
                .send(OutboundMessage::new("feishu", chat_id.to_string(), content))
                .await?;
            Ok(())
        }
        _ => anyhow::bail!("不支持的 heartbeat 投递 channel: {channel}"),
    }
}

async fn handle_heartbeat_cmd(
    settings: &Settings,
    cwd: &PathBuf,
    provider_override: Option<String>,
    model_override: Option<String>,
    cmd: HeartbeatCmd,
) -> Result<()> {
    let heartbeat_cfg = settings.gateway.heartbeat.clone();
    let heartbeat_file = {
        let path = PathBuf::from(&heartbeat_cfg.file);
        if path.is_absolute() {
            path
        } else {
            cwd.join(path)
        }
    };

    match cmd {
        HeartbeatCmd::Status => {
            let status = json!({
                "enabled": heartbeat_cfg.enabled,
                "interval_s": heartbeat_cfg.interval_s,
                "file": heartbeat_file,
                "exists": heartbeat_file.exists(),
                "target": heartbeat_cfg.target,
            });
            println!("{}", serde_json::to_string_pretty(&status)?);
        }
        HeartbeatCmd::Trigger => {
            let workspace_root = resolve_workspace_root(cwd);
            let db_path = resolve_session_db_path(&workspace_root);
            let store = SqliteSessionStore::new(db_path).await?;
            store.init().await?;
            let hooks = zerobot_core::hooks::HookManager::load(settings, cwd, None)?;
            let model = resolve_model(
                settings,
                provider_override.as_deref(),
                model_override.as_deref(),
            )?;
            let approvals = store.list_tool_approvals().await.unwrap_or_default();
            let store = Arc::new(store);
            let provider_factory = {
                let settings = settings.clone();
                let provider_override = provider_override.clone();
                Arc::new(move || {
                    build_provider(&settings, provider_override.as_deref())
                        .map_err(|err| ZeroBotError::Provider(err.to_string()))
                })
            };
            let tool_approvals = Arc::new(TokioRwLock::new(
                approvals.into_iter().collect::<HashSet<_>>(),
            ));
            let plugins = PluginManager::new(settings, cwd).await?;
            let mut tools = ToolRegistry::with_builtin_async(
                settings,
                cwd,
                Some(store.clone()),
                plugins.clone(),
            )
            .await?;
            let subagent_tools = tools.clone();
            tools.register(SubagentTool::new(
                settings.clone(),
                store.clone(),
                subagent_tools,
                cwd.clone(),
                provider_factory.clone(),
                model.clone(),
                hooks.clone(),
                None,
                tool_approvals.clone(),
            ));

            let session_id_slot = Arc::new(TokioRwLock::new(None::<String>));
            let route_target = heartbeat_cfg.target.clone();
            let exec = {
                let store = store.clone();
                let tools = tools.clone();
                let cwd = cwd.clone();
                let hooks = hooks.clone();
                let provider_factory = provider_factory.clone();
                let model = model.clone();
                let settings = settings.clone();
                let tool_approvals = tool_approvals.clone();
                let session_id_slot = session_id_slot.clone();
                let plugins = plugins.clone();
                Arc::new(move |tasks: String| {
                    let store = store.clone();
                    let tools = tools.clone();
                    let cwd = cwd.clone();
                    let hooks = hooks.clone();
                    let provider_factory = provider_factory.clone();
                    let model = model.clone();
                    let settings = settings.clone();
                    let tool_approvals = tool_approvals.clone();
                    let session_id_slot = session_id_slot.clone();
                    let plugins = plugins.clone();
                    let route =
                        route_target
                            .clone()
                            .map(|target| zerobot_core::tool::ToolRouteContext {
                                channel: target.channel,
                                chat_id: target.chat_id,
                                message_id: None,
                            });
                    async move {
                        let session_id =
                            if let Some(existing) = session_id_slot.read().await.clone() {
                                existing
                            } else {
                                let session = create_session_with_hooks(
                                    store.as_ref(),
                                    &hooks,
                                    "heartbeat".to_string(),
                                    None,
                                    zerobot_core::session::SessionKind::Main,
                                )
                                .await?;
                                *session_id_slot.write().await = Some(session.id.clone());
                                session.id
                            };
                        let provider = (provider_factory)()?;
                        let agent = Agent::new(
                            provider,
                            model,
                            settings,
                            store,
                            tools,
                            cwd,
                            hooks,
                            None,
                            plugins,
                            tool_approvals,
                            route,
                            None,
                        );
                        agent.run_turn(&session_id, &tasks, None).await
                    }
                    .boxed()
                })
            };

            let notify = heartbeat_cfg.target.clone().map(|target| {
                let settings = settings.clone();
                Arc::new(move |content: String| {
                    let settings = settings.clone();
                    let target = target.clone();
                    async move {
                        send_channel_message(&settings, &target.channel, &target.chat_id, content)
                            .await
                            .map_err(|err| ZeroBotError::Tool(err.to_string()))
                    }
                    .boxed()
                }) as zerobot_core::heartbeat::HeartbeatNotifyHandler
            });

            let service = HeartbeatService::new(
                cwd.clone(),
                provider_factory,
                model,
                heartbeat_cfg.file.clone(),
                heartbeat_cfg.interval_s,
                true,
                Some(exec),
                notify,
            );
            let output = service.trigger_now().await?;
            if let Some(content) = output {
                println!("{content}");
            } else {
                println!("skip");
            }
            if let Some(plugins) = &plugins {
                plugins.shutdown().await;
            }
        }
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

fn resolve_provider_id(settings: &Settings, provider_override: Option<&str>) -> String {
    provider_override
        .map(|s| s.to_string())
        .or_else(|| settings.default_provider.clone())
        .unwrap_or_else(|| "openai".to_string())
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

async fn session_summary_for_display(
    store: &SqliteSessionStore,
    session: &zerobot_core::session::Session,
) -> String {
    if let Ok(messages) = store.list_messages(&session.id).await {
        if let Some(first) = messages.into_iter().find(|msg| {
            matches!(msg.role, zerobot_core::session::MessageRole::User)
                && !msg.content.trim().is_empty()
        }) {
            return summarize_user_message(&first.content);
        }
    }
    session.summary.clone().unwrap_or_default()
}

fn summarize_user_message(content: &str) -> String {
    let mut text = content.trim().replace('\n', " ").replace('\r', " ");
    if text.chars().count() > 20 {
        text = text.chars().take(20).collect();
    }
    text
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

    #[test]
    fn cli_parses_acp_with_overrides() {
        let args = [
            "zerobot",
            "acp",
            "--cwd",
            "/tmp/project",
            "--provider",
            "openai",
            "--model",
            "gpt-4o-mini",
        ];
        let cli = Cli::parse_from(args);
        match cli.command {
            Some(Command::Acp {
                cwd,
                provider,
                model,
            }) => {
                assert_eq!(cwd, Some(PathBuf::from("/tmp/project")));
                assert_eq!(provider.as_deref(), Some("openai"));
                assert_eq!(model.as_deref(), Some("gpt-4o-mini"));
            }
            _ => panic!("acp 命令解析失败"),
        }
    }
}
