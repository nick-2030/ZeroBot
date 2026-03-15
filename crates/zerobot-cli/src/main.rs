use anyhow::Result;
use clap::{Parser, Subcommand};
use console::style;
use rustyline::error::ReadlineError;
use rustyline::history::DefaultHistory;
use rustyline::{ColorMode, Config, Editor};
use std::path::PathBuf;
use std::sync::Arc;
use std::io::Write;
use tokio::time::{self, Duration};
use tokio::sync::mpsc;
use zerobot_core::agent::Agent;
use zerobot_core::config::{ConfigLoader, Settings};
use zerobot_core::events::AgentEvent;
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
    let mut tools = ToolRegistry::with_builtin_async(settings, cwd).await?;
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

    print_logo();
    println!("{} {}", style("会话已启动:").green(), session.id);
    println!("输入 /exit 退出");

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
    let mut tools = ToolRegistry::with_builtin_async(settings, cwd).await?;
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

    let rl_config = Config::builder()
        .auto_add_history(true)
        .color_mode(ColorMode::Enabled)
        .build();
    let mut rl = Editor::<(), DefaultHistory>::with_config(rl_config)?;

    loop {
        let prompt = format!("{} ", style(">").cyan());
        let line = tokio::task::block_in_place(|| rl.readline(&prompt));
        let line = match line {
            Ok(line) => line,
            Err(ReadlineError::Interrupted) => continue,
            Err(ReadlineError::Eof) => break,
            Err(err) => return Err(anyhow::anyhow!("读取输入失败: {err}")),
        };
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }
        if line == "/exit" || line == "exit" {
            break;
        }

        println!();

        let (tx, mut rx) = mpsc::unbounded_channel();
        let session_id = session.id.clone();
        let input = line.clone();
        let provider = (provider_factory)()?;
        let agent = Agent::new(
            provider,
            model.clone(),
            settings.clone(),
            store.clone(),
            tools.clone(),
            cwd.clone(),
            hooks.clone(),
        );
        let mut runner =
            tokio::spawn(async move { agent.run_turn(&session_id, &input, Some(tx)).await });

        let mut blink = Blink::new();
        blink.start("思考中...");
        let mut ticker = time::interval(Duration::from_millis(350));

        let mut stream = StreamPrinter::new(DotColor::White);
        let mut streaming = false;
        let mut last_tool_label: Option<String> = None;

        loop {
            tokio::select! {
                Some(event) = rx.recv() => {
                    match event {
                        AgentEvent::AssistantDelta { content } => {
                            if !streaming {
                                blink.stop();
                                stream.start();
                                streaming = true;
                            }
                            stream.push(&content);
                        }
                        AgentEvent::AssistantMessage { content } => {
                            blink.stop();
                            print_block(DotColor::White, &content);
                            print_gap();
                        }
                        AgentEvent::ToolCallStarted { name, input } => {
                            if streaming {
                                stream.finish();
                                streaming = false;
                            }
                            let args = one_line(&input);
                            let label = format_tool_label(&name, &args);
                            last_tool_label = Some(label.clone());
                            blink.start(&label);
                        }
                        AgentEvent::ToolCallFinished { name: _name, output, ok } => {
                            blink.stop();
                            let color = if ok { DotColor::Green } else { DotColor::Red };
                            print_tool_output(color, last_tool_label.as_deref(), output.trim());
                            last_tool_label = None;
                            print_gap();
                            blink.start("思考中...");
                        }
                        AgentEvent::Error { message } => {
                            blink.stop();
                            print_block(DotColor::Red, &message);
                            print_gap();
                        }
                        _ => {}
                    }
                }
                result = &mut runner => {
                    blink.stop();
                    if streaming {
                        stream.finish();
                    }
                    if let Ok(Err(err)) = result {
                        print_block(DotColor::Red, &format!("{}", err));
                        print_gap();
                    }
                    break;
                }
                _ = ticker.tick() => {
                    blink.render();
                }
            }
        }
    }

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

fn print_logo() {
    println!("{}", style("ZeroBot").cyan().bold());
}

#[derive(Copy, Clone)]
enum DotColor {
    White,
    Green,
    Red,
}

fn print_block(color: DotColor, text: &str) {
    let dot = match color {
        DotColor::White => style("●").white(),
        DotColor::Green => style("●").green(),
        DotColor::Red => style("●").red(),
    };
    let cleaned = text.trim_end_matches('\n');

    if cleaned.trim().is_empty() {
        println!("{} ", dot);
        return;
    }

    for (idx, line) in cleaned.lines().enumerate() {
        if idx == 0 {
            println!("{} {}", dot, line);
        } else {
            println!("  {}", line);
        }
    }
}

fn print_tool_output(color: DotColor, label: Option<&str>, output: &str) {
    let (lines, omitted) = truncate_lines(output, 3);
    if lines.is_empty() {
        if let Some(label) = label {
            print_block(color, label);
        } else {
            print_block(color, "");
        }
        return;
    }
    let mut joined = String::new();
    if let Some(label) = label {
        joined.push_str(label);
        joined.push('\n');
    }
    joined.push_str(&lines.join("\n"));
    if omitted > 0 {
        joined.push_str(&format!("\n... 已省略 {} 行", omitted));
    }
    print_block(color, &joined);
}

fn truncate_lines(text: &str, max: usize) -> (Vec<String>, usize) {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= max {
        return (lines.into_iter().map(|s| s.to_string()).collect(), 0);
    }
    let kept = lines[..max].iter().map(|s| s.to_string()).collect();
    (kept, lines.len() - max)
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn format_tool_label(name: &str, args: &str) -> String {
    let base = name.to_string();
    if args.is_empty() {
        return base;
    }
    let mut full = format!("{base} {args}");
    let max_label = terminal_width().unwrap_or(160).saturating_sub(2);
    if full.chars().count() <= max_label {
        return full;
    }
    let max_args = max_label.saturating_sub(base.chars().count() + 1);
    if max_args == 0 {
        return base;
    }
    let trimmed = truncate_chars(args, max_args);
    full = format!("{base} {trimmed}");
    full
}

fn terminal_width() -> Option<usize> {
    let (cols, _rows) = console::Term::stdout().size();
    if cols == 0 {
        None
    } else {
        Some(cols as usize)
    }
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_string();
    }
    if max_chars <= 3 {
        return text.chars().take(max_chars).collect();
    }
    let keep = max_chars - 3;
    let mut out: String = text.chars().take(keep).collect();
    out.push_str("...");
    out
}

struct StreamPrinter {
    started: bool,
    ended_with_newline: bool,
    color: DotColor,
    at_line_start: bool,
    trailing_newlines: usize,
}

impl StreamPrinter {
    fn new(color: DotColor) -> Self {
        Self {
            started: false,
            ended_with_newline: false,
            color,
            at_line_start: false,
            trailing_newlines: 0,
        }
    }

    fn start(&mut self) {
        if !self.started {
            let dot = match self.color {
                DotColor::White => style("●").white(),
                DotColor::Green => style("●").green(),
                DotColor::Red => style("●").red(),
            };
            print!("{} ", dot);
            let _ = std::io::stdout().flush();
            self.started = true;
            self.at_line_start = false;
        }
    }

    fn push(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let chunk = if self.started { text } else { text.trim_start_matches('\n') };
        for ch in chunk.chars() {
            if self.at_line_start {
                print!("  ");
                self.at_line_start = false;
            }
            print!("{ch}");
            if ch == '\n' {
                self.at_line_start = true;
                self.trailing_newlines += 1;
            } else {
                self.trailing_newlines = 0;
            }
        }
        let _ = std::io::stdout().flush();
        self.ended_with_newline = chunk.ends_with('\n');
    }

    fn finish(&mut self) {
        if !self.started {
            return;
        }
        match self.trailing_newlines {
            0 => {
                println!();
                print_gap();
            }
            1 => {
                print_gap();
            }
            _ => {}
        }
        self.started = false;
        self.ended_with_newline = false;
        self.at_line_start = false;
        self.trailing_newlines = 0;
    }
}

fn print_gap() {
    println!();
}

struct Blink {
    active: bool,
    visible: bool,
    label: String,
}

impl Blink {
    fn new() -> Self {
        Self {
            active: false,
            visible: false,
            label: String::new(),
        }
    }

    fn start(&mut self, label: &str) {
        self.active = true;
        self.visible = false;
        self.label = label.to_string();
        self.visible = true;
        let symbol = "●";
        let dot = style(symbol).white();
        print!("\r\x1b[2K{} {}", dot, self.label);
        let _ = std::io::stdout().flush();
    }

    fn stop(&mut self) {
        if self.active {
            self.clear_line();
        }
        self.active = false;
        self.visible = false;
        self.label.clear();
    }

    fn render(&mut self) {
        if !self.active {
            return;
        }
        self.visible = !self.visible;
        self.draw();
    }

    fn clear_line(&self) {
        print!("\r\x1b[2K\r");
        let _ = std::io::stdout().flush();
    }

    fn draw(&self) {
        let symbol = if self.visible { "●" } else { " " };
        let dot = style(symbol).white();
        print!("\x1b[s\r{}\x1b[u", dot);
        let _ = std::io::stdout().flush();
    }
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
