use anyhow::Result;
use base64::Engine as _;
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::{execute, Command};
use pulldown_cmark::{Alignment, Event as MdEvent, Options, Parser, Tag, TagEnd};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap, Clear, BorderType};
use ratatui::{Frame, Terminal};
use std::time::Instant;
use tokio::sync::mpsc;
use tokio::time::{self, Duration};
use tokio_stream::StreamExt;
use unicode_width::UnicodeWidthStr;
use zerobot_core::agent::Agent;
use zerobot_core::config::Settings;
use zerobot_core::events::AgentEvent;
use zerobot_core::hooks::HookManager;
use zerobot_core::provider::{ProviderFactory, TokenUsage};
use zerobot_core::session::{SessionStore, TodoItem, TodoStatus};
use zerobot_core::skills::SkillStackEntry;
use zerobot_core::tool::ToolRegistry;

#[derive(Copy, Clone)]
enum DotColor {
    White,
    Green,
    Red,
}

const BORDER_COLOR: Color = Color::DarkGray;
const LOGO_COLOR: Color = Color::Cyan;

#[derive(Clone)]
enum Status {
    Idle,
    Thinking,
    Tool(String),
    Error(String),
}

struct PermissionPrompt {
    title: String,
    options: Vec<String>,
    selected: usize,
}

struct App {
    session_id: String,
    provider_id: String,
    model: String,
    status: Status,
    output: Vec<Line<'static>>,
    stream_buffer: String,
    streaming: bool,
    last_tool_label: Option<String>,
    input: String,
    cursor: usize,
    scroll: u16,
    stick_to_bottom: bool,
    todos: Vec<TodoItem>,
    skills: Vec<SkillStackEntry>,
    usage: Option<TokenUsage>,
    permission_prompt: Option<PermissionPrompt>,
    viewport_width: u16,
    blink_on: bool,
    last_blink: Instant,
    last_copyable_output: Option<String>,
}

impl App {
    fn new(session_id: String, provider_id: String, model: String) -> Self {
        Self {
            session_id,
            provider_id,
            model,
            status: Status::Idle,
            output: Vec::new(),
            stream_buffer: String::new(),
            streaming: false,
            last_tool_label: None,
            input: String::new(),
            cursor: 0,
            scroll: 0,
            stick_to_bottom: true,
            todos: Vec::new(),
            skills: Vec::new(),
            usage: None,
            permission_prompt: None,
            viewport_width: 0,
            blink_on: true,
            last_blink: Instant::now(),
            last_copyable_output: None,
        }
    }

    fn push_line(&mut self, line: Line<'static>) {
        if !self.output.is_empty() {
            self.output.push(Line::from(Span::raw("")));
        }
        self.output.push(line);
    }

    fn push_block(&mut self, color: DotColor, text: &str) {
        if !self.output.is_empty() {
            self.output.push(Line::from(Span::raw("")));
        }
        self.output.extend(format_block_lines(color, text));
    }

    fn push_markdown_block(&mut self, text: &str) {
        if !self.output.is_empty() {
            self.output.push(Line::from(Span::raw("")));
        }
        self.output.extend(format_markdown_lines(text));
    }

    fn push_tool_output(
        &mut self,
        color: DotColor,
        label: Option<&str>,
        output: &str,
        width: u16,
    ) {
        let (lines, omitted) = truncate_lines(output, 3);
        if let Some(label) = label {
            if !self.output.is_empty() {
                self.output.push(Line::from(Span::raw("")));
            }
            let mut line = Vec::new();
            line.push(tool_dot_span(color));
            line.push(Span::raw(" "));
            line.push(Span::raw(label.to_string()));
            self.output.push(Line::from(line));
        }
        let mut content_lines = Vec::new();
        content_lines.extend(lines);
        if omitted > 0 {
            content_lines.push(format!("... 已省略 {} 行", omitted));
        }
        let box_lines = format_tool_box_lines(&content_lines, width);
        self.output.extend(box_lines);
    }

    fn append_stream_delta(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if !self.streaming {
            self.streaming = true;
            self.stream_buffer.clear();
            if !self.output.is_empty() {
                if let Some(last) = self.output.last() {
                    if !line_is_blank(last) {
                        self.output.push(Line::from(Span::raw("")));
                    }
                }
            }
        }
        let chunk = if self.stream_buffer.is_empty() {
            text.trim_start_matches('\n')
        } else {
            text
        };
        self.stream_buffer.push_str(chunk);
    }

    fn finalize_stream(&mut self) {
        if !self.streaming {
            return;
        }
        let content = self.stream_buffer.clone();
        self.output.extend(format_markdown_lines(&content));
        if !content.trim().is_empty() {
            self.last_copyable_output = Some(content);
        }
        self.stream_buffer.clear();
        self.streaming = false;
    }

    fn display_lines(&self) -> Vec<Line<'static>> {
        let mut lines = self.output.clone();
        if self.streaming {
            lines.extend(format_block_lines(DotColor::White, &self.stream_buffer));
        }
        lines
    }

    fn info_lines(&self) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let status_line = match &self.status {
            Status::Idle => Line::from(Span::raw("状态: 空闲")),
            Status::Thinking => {
                let dot = if self.blink_on { "●" } else { " " };
                Line::from(Span::raw(format!("状态: {dot} 思考中")))
            }
            Status::Tool(name) => Line::from(Span::raw(format!("状态: 工具执行中: {name}"))),
            Status::Error(message) => Line::from(Span::raw(format!("状态: 错误: {message}"))),
        };
        lines.push(status_line);
        if !self.todos.is_empty() {
            lines.push(Line::from(Span::styled(
                "Todo:",
                Style::default().add_modifier(Modifier::BOLD),
            )));
            for item in self.todos.iter().take(2) {
                let status = match item.status {
                    TodoStatus::Pending => "pending",
                    TodoStatus::InProgress => "in_progress",
                    TodoStatus::Completed => "completed",
                    TodoStatus::Cancelled => "cancelled",
                };
                lines.push(Line::from(Span::raw(format!("  [{status}] {}", item.content))));
            }
        }
        if !self.skills.is_empty() {
            lines.push(Line::from(Span::styled(
                "Skill 栈:",
                Style::default().add_modifier(Modifier::BOLD),
            )));
            for skill in self.skills.iter().rev().take(2) {
                lines.push(Line::from(Span::raw(format!(
                    "  {}: {}",
                    skill.name, skill.description
                ))));
            }
        }
        lines
    }

    fn command_hint(&self) -> String {
        if self.input.trim_start().starts_with('/') {
            "/exit /help /clear /copy".to_string()
        } else {
            String::new()
        }
    }
}

pub async fn run_tui(
    settings: Settings,
    cwd: std::path::PathBuf,
    session_id: String,
    store: std::sync::Arc<dyn SessionStore>,
    tools: ToolRegistry,
    provider_factory: ProviderFactory,
    model: String,
    provider_id: String,
    hooks: HookManager,
    use_alt_screen: bool,
) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    if use_alt_screen {
        execute!(stdout, EnterAlternateScreen, EnableAlternateScroll)?;
    }
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_tui_inner(
        &mut terminal,
        settings,
        cwd,
        session_id,
        store,
        tools,
        provider_factory,
        model,
        provider_id,
        hooks,
    )
    .await;

    disable_raw_mode()?;
    if use_alt_screen {
        execute!(
            terminal.backend_mut(),
            DisableAlternateScroll,
            LeaveAlternateScreen
        )?;
    }
    terminal.show_cursor()?;

    result
}

async fn run_tui_inner(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    settings: Settings,
    cwd: std::path::PathBuf,
    session_id: String,
    store: std::sync::Arc<dyn SessionStore>,
    tools: ToolRegistry,
    provider_factory: ProviderFactory,
    model: String,
    provider_id: String,
    hooks: HookManager,
) -> Result<()> {
    let mut app = App::new(session_id.clone(), provider_id.clone(), model.clone());
    let (cols, _rows) = crossterm::terminal::size().unwrap_or((120, 0));
    let welcome = build_welcome_lines(
        env!("CARGO_PKG_VERSION"),
        &provider_id,
        &model,
        &cwd.display().to_string(),
        cols as usize,
    );
    app.output.extend(welcome);

    refresh_session_state(&mut app, &store).await;

    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
    let mut runner: Option<tokio::task::JoinHandle<zerobot_core::error::ZeroBotResult<String>>> =
        None;
    let mut reader = EventStream::new();
    let mut tick = time::interval(Duration::from_millis(50));
    let mut should_quit = false;
    let mut dirty = true;

    loop {
        if should_quit {
            break;
        }

        if let Some(handle) = &mut runner {
            tokio::select! {
                _ = tick.tick() => {
                    if update_blink(&mut app) {
                        dirty = true;
                    }
                    if dirty {
                        terminal.draw(|f| draw(f, &mut app))?;
                        dirty = false;
                    }
                }
                maybe_event = reader.next() => {
                    if let Some(Ok(event)) = maybe_event {
                        if handle_event(event, &mut app, &mut runner, &settings, &cwd, &store, &tools, &provider_factory, &model, &hooks, &tx, &mut should_quit).await? {
                            dirty = true;
                        }
                    }
                }
                Some(event) = rx.recv() => {
                    handle_agent_event(event, &mut app, &store).await;
                    dirty = true;
                }
                result = handle => {
                    if let Ok(Err(err)) = result {
                        app.finalize_stream();
                        app.status = Status::Error(format!("{err}"));
                        app.push_block(DotColor::Red, &format!("{err}"));
                    } else {
                        app.status = Status::Idle;
                    }
                    runner = None;
                    dirty = true;
                }
            }
        } else {
            tokio::select! {
                _ = tick.tick() => {
                    if update_blink(&mut app) {
                        dirty = true;
                    }
                    if dirty {
                        terminal.draw(|f| draw(f, &mut app))?;
                        dirty = false;
                    }
                }
                maybe_event = reader.next() => {
                    if let Some(Ok(event)) = maybe_event {
                        if handle_event(event, &mut app, &mut runner, &settings, &cwd, &store, &tools, &provider_factory, &model, &hooks, &tx, &mut should_quit).await? {
                            dirty = true;
                        }
                    }
                }
                Some(event) = rx.recv() => {
                    handle_agent_event(event, &mut app, &store).await;
                    dirty = true;
                }
            }
        }
    }

    Ok(())
}

async fn handle_event(
    event: Event,
    app: &mut App,
    runner: &mut Option<tokio::task::JoinHandle<zerobot_core::error::ZeroBotResult<String>>>,
    settings: &Settings,
    cwd: &std::path::PathBuf,
    store: &std::sync::Arc<dyn SessionStore>,
    tools: &ToolRegistry,
    provider_factory: &ProviderFactory,
    model: &str,
    hooks: &HookManager,
    tx: &mpsc::UnboundedSender<AgentEvent>,
    should_quit: &mut bool,
) -> Result<bool> {
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => {
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                *should_quit = true;
                return Ok(true);
            }
            match key.code {
                KeyCode::Enter => {
                    if runner.is_some() {
                        return Ok(false);
                    }
                    let input = app.input.trim().to_string();
                    if input.is_empty() {
                        return Ok(false);
                    }
                    if input == "/exit" || input == "exit" {
                        app.push_line(user_input_line(&input));
                        *should_quit = true;
                        return Ok(true);
                    }
                    if input == "/clear" {
                        app.output.clear();
                        app.stream_buffer.clear();
                        app.streaming = false;
                        app.scroll = 0;
                        app.stick_to_bottom = true;
                        app.input.clear();
                        app.cursor = 0;
                        return Ok(true);
                    }
                    app.push_line(user_input_line(&input));
                    if input == "/help" {
                        app.push_block(DotColor::White, "可用命令: /exit /help /clear /copy");
                        app.input.clear();
                        app.cursor = 0;
                        return Ok(true);
                    }
                    if input == "/copy" {
                        let message = match app.last_copyable_output.as_deref() {
                            Some(text) => match copy_text_to_clipboard(text) {
                                Ok(()) => "已复制最新回复到剪贴板".to_string(),
                                Err(err) => format!("复制失败: {err}"),
                            },
                            None => "暂无可复制内容".to_string(),
                        };
                        app.push_block(DotColor::White, &message);
                        app.input.clear();
                        app.cursor = 0;
                        return Ok(true);
                    }

                    app.input.clear();
                    app.cursor = 0;
                    app.status = Status::Thinking;
                    app.blink_on = true;
                    app.last_blink = Instant::now();

                    let provider = (provider_factory)()?;
                    let agent = Agent::new(
                        provider,
                        model.to_string(),
                        settings.clone(),
                        store.clone(),
                        tools.clone(),
                        cwd.clone(),
                        hooks.clone(),
                    );
                    let session_id = app.session_id.clone();
                    let input_clone = input.clone();
                    let tx_clone = tx.clone();
                    *runner = Some(tokio::spawn(async move {
                        agent.run_turn(&session_id, &input_clone, Some(tx_clone)).await
                    }));
                    return Ok(true);
                }
                KeyCode::Backspace => {
                    if app.cursor > 0 {
                        let idx = char_to_byte_idx(&app.input, app.cursor - 1);
                        app.input.remove(idx);
                        app.cursor -= 1;
                        return Ok(true);
                    }
                }
                KeyCode::Delete => {
                    if app.cursor < app.input.chars().count() {
                        let idx = char_to_byte_idx(&app.input, app.cursor);
                        app.input.remove(idx);
                        return Ok(true);
                    }
                }
                KeyCode::Left => {
                    if app.cursor > 0 {
                        app.cursor -= 1;
                        return Ok(true);
                    }
                }
                KeyCode::Right => {
                    if app.cursor < app.input.chars().count() {
                        app.cursor += 1;
                        return Ok(true);
                    }
                }
                KeyCode::Home => {
                    app.cursor = 0;
                    return Ok(true);
                }
                KeyCode::End => {
                    app.cursor = app.input.chars().count();
                    return Ok(true);
                }
                KeyCode::PageUp => {
                    app.stick_to_bottom = false;
                    app.scroll = app.scroll.saturating_sub(5);
                    return Ok(true);
                }
                KeyCode::PageDown => {
                    app.stick_to_bottom = false;
                    app.scroll = app.scroll.saturating_add(5);
                    return Ok(true);
                }
                KeyCode::Up => {
                    app.stick_to_bottom = false;
                    app.scroll = app.scroll.saturating_sub(1);
                    return Ok(true);
                }
                KeyCode::Down => {
                    app.stick_to_bottom = false;
                    app.scroll = app.scroll.saturating_add(1);
                    return Ok(true);
                }
                KeyCode::Char(ch) => {
                    if !key.modifiers.contains(KeyModifiers::CONTROL) {
                        let idx = char_to_byte_idx(&app.input, app.cursor);
                        app.input.insert(idx, ch);
                        app.cursor += 1;
                        return Ok(true);
                    }
                }
                _ => {}
            }
        }
        Event::Mouse(mouse) => match mouse.kind {
            MouseEventKind::ScrollUp => {
                app.stick_to_bottom = false;
                app.scroll = app.scroll.saturating_sub(1);
                return Ok(true);
            }
            MouseEventKind::ScrollDown => {
                app.stick_to_bottom = false;
                app.scroll = app.scroll.saturating_add(1);
                return Ok(true);
            }
            _ => {}
        },
        Event::Resize(_, _) => {
            return Ok(true);
        }
        _ => {}
    }
    Ok(false)
}

async fn handle_agent_event(
    event: AgentEvent,
    app: &mut App,
    store: &std::sync::Arc<dyn SessionStore>,
) {
    match event {
        AgentEvent::AssistantDelta { content } => {
            app.append_stream_delta(&content);
        }
        AgentEvent::AssistantMessage { content } => {
            app.finalize_stream();
            app.push_markdown_block(&content);
            if !content.trim().is_empty() {
                app.last_copyable_output = Some(content);
            }
        }
        AgentEvent::ToolCallStarted { name, input } => {
            app.finalize_stream();
            let args = one_line(&input);
            let label = format_tool_label(&name, &args, app.viewport_width);
            app.last_tool_label = Some(label.clone());
            app.status = Status::Tool(label);
        }
        AgentEvent::ToolCallFinished { output, ok, .. } => {
            let color = if ok { DotColor::Green } else { DotColor::Red };
            let label = app.last_tool_label.clone();
            app.push_tool_output(color, label.as_deref(), output.trim(), app.viewport_width);
            app.last_tool_label = None;
            app.status = Status::Thinking;
            app.blink_on = true;
            app.last_blink = Instant::now();
            refresh_session_state(app, store).await;
        }
        AgentEvent::Usage { usage } => {
            app.usage = Some(usage);
        }
        AgentEvent::Error { message } => {
            app.finalize_stream();
            app.status = Status::Error(message.clone());
            app.push_block(DotColor::Red, &message);
        }
        AgentEvent::Done => {
            app.finalize_stream();
            app.status = Status::Idle;
            refresh_session_state(app, store).await;
        }
        _ => {}
    }
}

async fn refresh_session_state(app: &mut App, store: &std::sync::Arc<dyn SessionStore>) {
    if let Ok(todos) = store.get_todos(&app.session_id).await {
        app.todos = todos;
    }
    if let Ok(stack) = store.get_skill_stack(&app.session_id).await {
        app.skills = stack;
    }
}

fn update_blink(app: &mut App) -> bool {
    if !matches!(app.status, Status::Thinking) {
        if !app.blink_on {
            app.blink_on = true;
            return true;
        }
        return false;
    }
    let now = Instant::now();
    if now.duration_since(app.last_blink) >= Duration::from_millis(500) {
        app.blink_on = !app.blink_on;
        app.last_blink = now;
        return true;
    }
    false
}

fn draw(frame: &mut Frame, app: &mut App) {
    let size = frame.size();
    let info_lines = app.info_lines();
    let info_height = info_lines.len().max(1) as u16 + 2;

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(info_height),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(size);

    let output_area = chunks[0];
    let info_area = chunks[2];
    let input_area = chunks[3];
    let status_area = chunks[4];

    app.viewport_width = output_area.width;

    let display_lines = app.display_lines();
    let total_lines = count_wrapped_lines(&display_lines, output_area.width);
    let visible_height = output_area.height as usize;
    let max_scroll = total_lines.saturating_sub(visible_height);
    if app.stick_to_bottom {
        app.scroll = max_scroll as u16;
    } else if (app.scroll as usize) >= max_scroll {
        app.scroll = max_scroll as u16;
        app.stick_to_bottom = true;
    }

    let output_widget = Paragraph::new(Text::from(display_lines))
        .wrap(Wrap { trim: false })
        .scroll((app.scroll, 0));
    frame.render_widget(output_widget, output_area);

    let info_widget = Paragraph::new(Text::from(info_lines))
        .block(
            Block::default()
                .title("会话信息")
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(BORDER_COLOR)),
        )
        .style(Style::default().fg(Color::White));
    frame.render_widget(info_widget, info_area);

    let input_block = Block::default().borders(Borders::TOP | Borders::BOTTOM);
    let input_line = Line::from(vec![
        Span::styled(">", Style::default().fg(Color::Cyan)),
        Span::raw(" "),
        Span::raw(app.input.clone()),
    ]);
    let input_widget = Paragraph::new(Text::from(input_line)).block(input_block);
    frame.render_widget(input_widget, input_area);

    let status_text = build_status_bar(app);
    let status_widget = Paragraph::new(Text::from(Line::from(Span::raw(status_text))))
        .style(Style::default().fg(Color::White).bg(Color::DarkGray));
    frame.render_widget(status_widget, status_area);

    if let Some(prompt) = &app.permission_prompt {
        render_permission_prompt(frame, prompt);
    }

    let inner = Rect {
        x: input_area.x,
        y: input_area.y + 1,
        width: input_area.width,
        height: 1,
    };
    let cursor_offset = UnicodeWidthStr::width(app.input.chars().take(app.cursor).collect::<String>().as_str()) as u16;
    let cursor_x = inner.x.saturating_add(2 + cursor_offset);
    let cursor_x = cursor_x.min(inner.x.saturating_add(inner.width.saturating_sub(1)));
    frame.set_cursor(cursor_x, inner.y);
}

fn render_permission_prompt(frame: &mut Frame, prompt: &PermissionPrompt) {
    let area = centered_rect(60, 20, frame.size());
    frame.render_widget(Clear, area);
    let lines = prompt
        .options
        .iter()
        .enumerate()
        .map(|(idx, opt)| {
            let prefix = if idx == prompt.selected { "> " } else { "  " };
            Line::from(Span::raw(format!("{prefix}{opt}")))
        })
        .collect::<Vec<_>>();
    let block = Block::default()
        .title(prompt.title.clone())
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(BORDER_COLOR));
    let widget = Paragraph::new(Text::from(lines)).block(block);
    frame.render_widget(widget, area);
}

fn build_status_bar(app: &App) -> String {
    let usage = app.usage.as_ref();
    let input = usage.and_then(|u| u.input_tokens).map(|v| v.to_string()).unwrap_or_else(|| "-".to_string());
    let output = usage.and_then(|u| u.output_tokens).map(|v| v.to_string()).unwrap_or_else(|| "-".to_string());
    let total = usage.and_then(|u| u.total_tokens).map(|v| v.to_string()).unwrap_or_else(|| "-".to_string());
    let commands = app.command_hint();
    let mut parts = vec![
        format!("Session: {}", app.session_id),
        format!("{} / {}", app.provider_id, app.model),
        format!("Tokens: {input}/{output}/{total}"),
    ];
    if !commands.is_empty() {
        parts.push(format!("Commands: {commands}"));
    }
    parts.join(" | ")
}

fn user_input_line(text: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(">", Style::default().fg(Color::Cyan)),
        Span::raw(" "),
        Span::raw(text.to_string()),
    ])
}

fn copy_text_to_clipboard(text: &str) -> Result<(), String> {
    if std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some() {
        return copy_via_osc52(text);
    }

    match arboard::Clipboard::new() {
        Ok(mut clipboard) => match clipboard.set_text(text.to_string()) {
            Ok(()) => Ok(()),
            Err(err) => copy_via_osc52(text).map_err(|_| format!("clipboard unavailable: {err}")),
        },
        Err(err) => copy_via_osc52(text).map_err(|_| format!("clipboard unavailable: {err}")),
    }
}

fn copy_via_osc52(text: &str) -> Result<(), String> {
    let sequence = osc52_sequence(text, std::env::var_os("TMUX").is_some());
    #[cfg(unix)]
    {
        use std::fs::OpenOptions;
        use std::io::Write;
        let mut tty = OpenOptions::new()
            .write(true)
            .open("/dev/tty")
            .map_err(|e| format!("clipboard unavailable: failed to open /dev/tty: {e}"))?;
        tty.write_all(sequence.as_bytes())
            .map_err(|e| format!("clipboard unavailable: failed to write OSC 52: {e}"))?;
        tty.flush()
            .map_err(|e| format!("clipboard unavailable: failed to flush OSC 52: {e}"))?;
        return Ok(());
    }
    #[cfg(windows)]
    {
        use std::io::Write;
        let mut out = std::io::stdout();
        out.write_all(sequence.as_bytes())
            .map_err(|e| format!("clipboard unavailable: failed to write OSC 52: {e}"))?;
        out.flush()
            .map_err(|e| format!("clipboard unavailable: failed to flush OSC 52: {e}"))?;
        Ok(())
    }
}

fn osc52_sequence(text: &str, tmux: bool) -> String {
    let data = base64::engine::general_purpose::STANDARD.encode(text);
    if tmux {
        format!("\x1bPtmux;\x1b]52;c;{}\x07\x1b\\", data)
    } else {
        format!("\x1b]52;c;{}\x07", data)
    }
}

fn build_welcome_lines(
    version: &str,
    provider: &str,
    model: &str,
    cwd: &str,
    term_width: usize,
) -> Vec<Line<'static>> {
    let logo = [
        "███████╗███████╗██████╗  ██████╗ ██████╗  ██████╗ ████████╗",
        "╚══███╔╝██╔════╝██╔══██╗██╔═══██╗██╔══██╗██╔═══██╗╚══██╔══╝",
        "  ███╔╝ █████╗  ██████╔╝██║   ██║██████╔╝██║   ██║   ██║   ",
        " ███╔╝  ██╔══╝  ██╔══██╗██║   ██║██╔══██╗██║   ██║   ██║   ",
        "███████╗███████╗██║  ██║╚██████╔╝██████╔╝╚██████╔╝   ██║   ",
        "╚══════╝╚══════╝╚═╝  ╚═╝ ╚═════╝ ╚═════╝  ╚═════╝    ╚═╝   ",
    ];
    let title = format!(">_ zerobot (v{version})");
    let meta = format!("{provider} | {model}");
    let help = "输入 /help 查看命令";
    let box_lines = [title, meta, cwd.to_string(), help.to_string()];
    let logo_width = logo.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    let box_width = box_lines
        .iter()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(0)
        + 4;
    let min_width = logo_width + 2 + box_width;

    let mut out = Vec::new();
    if term_width >= min_width {
        let inner = box_width.saturating_sub(2);
        let top = format!("╭{}╮", "─".repeat(inner));
        let bottom = format!("╰{}╯", "─".repeat(inner));
        let mut box_rendered: Vec<(String, bool)> = Vec::new();
        box_rendered.push((top, true));
        for line in &box_lines {
            let pad = inner.saturating_sub(UnicodeWidthStr::width(line.as_str()));
            box_rendered.push((format!("│{}{}│", line, " ".repeat(pad)), false));
        }
        box_rendered.push((bottom, true));

        let rows = logo.len().max(box_rendered.len());
        for i in 0..rows {
            let left = *logo.get(i).unwrap_or(&"");
            let left_pad = logo_width.saturating_sub(left.chars().count());
            let right = box_rendered.get(i).map(|(s, _)| s.as_str()).unwrap_or("");
            let right_is_border = box_rendered.get(i).map(|(_, b)| *b).unwrap_or(false);
            let mut spans = Vec::new();
            spans.push(Span::styled(left.to_string(), Style::default().fg(LOGO_COLOR)));
            spans.push(Span::raw(" ".repeat(left_pad)));
            spans.push(Span::raw("  "));
            if right_is_border {
                spans.push(Span::styled(right.to_string(), Style::default().fg(BORDER_COLOR)));
            } else {
                // right line has borders; color only the borders to keep text bright.
                let mut chars = right.chars();
                let left_border = chars.next().unwrap_or('│').to_string();
                let right_border = right.chars().last().unwrap_or('│').to_string();
                let middle: String = right.chars().skip(1).take(right.chars().count().saturating_sub(2)).collect();
                spans.push(Span::styled(left_border, Style::default().fg(BORDER_COLOR)));
                spans.push(Span::raw(middle));
                spans.push(Span::styled(right_border, Style::default().fg(BORDER_COLOR)));
            }
            out.push(Line::from(spans));
        }
    } else {
        // Fallback: only render the info box when terminal is narrow.
        let inner = box_width.saturating_sub(2).max(10);
        out.push(Line::from(Span::styled(
            format!("╭{}╮", "─".repeat(inner)),
            Style::default().fg(BORDER_COLOR),
        )));
        for line in &box_lines {
            let pad = inner.saturating_sub(line.chars().count());
            let mut spans = Vec::new();
            spans.push(Span::styled("│", Style::default().fg(BORDER_COLOR)));
            spans.push(Span::raw(format!("{}{}", line, " ".repeat(pad))));
            spans.push(Span::styled("│", Style::default().fg(BORDER_COLOR)));
            out.push(Line::from(spans));
        }
        out.push(Line::from(Span::styled(
            format!("╰{}╯", "─".repeat(inner)),
            Style::default().fg(BORDER_COLOR),
        )));
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EnableAlternateScroll;

impl Command for EnableAlternateScroll {
    fn write_ansi(&self, f: &mut impl std::fmt::Write) -> std::fmt::Result {
        write!(f, "\x1b[?1007h")
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> crossterm::Result<()> {
        Err(std::io::Error::other(
            "tried to execute EnableAlternateScroll using WinAPI; use ANSI instead",
        ))
    }

    #[cfg(windows)]
    fn is_ansi_code_supported(&self) -> bool {
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DisableAlternateScroll;

impl Command for DisableAlternateScroll {
    fn write_ansi(&self, f: &mut impl std::fmt::Write) -> std::fmt::Result {
        write!(f, "\x1b[?1007l")
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> crossterm::Result<()> {
        Err(std::io::Error::other(
            "tried to execute DisableAlternateScroll using WinAPI; use ANSI instead",
        ))
    }

    #[cfg(windows)]
    fn is_ansi_code_supported(&self) -> bool {
        true
    }
}

fn format_block_lines(color: DotColor, text: &str) -> Vec<Line<'static>> {
    let cleaned = text.trim_end_matches('\n');
    if cleaned.trim().is_empty() {
        return vec![Line::from(vec![dot_span(color), Span::raw(" ")])];
    }
    let mut lines = Vec::new();
    for (idx, line) in cleaned.lines().enumerate() {
        if idx == 0 {
            lines.push(Line::from(vec![
                dot_span(color),
                Span::raw(" "),
                Span::raw(line.to_string()),
            ]));
        } else {
            lines.push(Line::from(Span::raw(format!("  {line}"))));
        }
    }
    lines
}

fn format_markdown_lines(text: &str) -> Vec<Line<'static>> {
    let mut lines = markdown_to_lines(text);
    if lines.is_empty() {
        return vec![Line::from(vec![dot_span(DotColor::White), Span::raw(" ")])];
    }
    let mut out = Vec::new();
    for (idx, mut line) in lines.drain(..).enumerate() {
        let mut spans = Vec::new();
        if idx == 0 {
            spans.push(dot_span(DotColor::White));
            spans.push(Span::raw(" "));
        } else {
            spans.push(Span::raw("  "));
        }
        spans.extend(line.spans.drain(..));
        out.push(Line::from(spans));
    }
    out
}

fn markdown_to_lines(text: &str) -> Vec<Line<'static>> {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TABLES);
    let parser = Parser::new_ext(text, opts);

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current: Vec<Span<'static>> = Vec::new();
    let mut style_stack: Vec<Style> = vec![Style::default()];
    let mut in_code_block = false;
    let mut list_stack: Vec<(bool, usize, usize)> = Vec::new(); // (ordered, index, indent_len)
    let mut pending_prefix: Option<String> = None;
    let mut current_prefix: Option<String> = None;
    let mut blockquote_depth = 0usize;
    let mut table_align: Vec<Alignment> = Vec::new();
    let mut table_rows: Vec<Vec<String>> = Vec::new();
    let mut current_row: Vec<String> = Vec::new();
    let mut current_cell = String::new();
    let mut in_table_cell = false;
    let mut table_header_rows = 0usize;
    let mut in_table_head = false;

    let flush_line = |lines: &mut Vec<Line<'static>>, current: &mut Vec<Span<'static>>| {
        if !current.is_empty() {
            lines.push(Line::from(std::mem::take(current)));
        }
    };

    let ensure_prefix = |current: &mut Vec<Span<'static>>,
                             pending_prefix: &mut Option<String>,
                             current_prefix: &Option<String>,
                             list_stack: &Vec<(bool, usize, usize)>,
                             blockquote_depth: usize| {
        if !current.is_empty() {
            return;
        }
        if blockquote_depth > 0 {
            let prefix = "│ ".repeat(blockquote_depth);
            current.push(Span::styled(prefix, Style::default().fg(Color::DarkGray)));
        }
        let mut prefix = String::new();
        if !list_stack.is_empty() {
            if let Some(p) = pending_prefix.take() {
                prefix.push_str(&p);
            } else if let Some(indent) = current_prefix {
                prefix.push_str(indent);
            }
        }
        if !prefix.is_empty() {
            current.push(Span::raw(prefix));
        }
    };

    for event in parser {
        match event {
            MdEvent::Start(tag) => match tag {
                Tag::Emphasis => {
                    let style = style_stack.last().copied().unwrap_or_default().add_modifier(Modifier::ITALIC);
                    style_stack.push(style);
                }
                Tag::Strong => {
                    let style = style_stack.last().copied().unwrap_or_default().add_modifier(Modifier::BOLD);
                    style_stack.push(style);
                }
                Tag::Strikethrough => {
                    let style = style_stack.last().copied().unwrap_or_default().add_modifier(Modifier::CROSSED_OUT);
                    style_stack.push(style);
                }
                Tag::Heading { .. } => {
                    let style = style_stack.last().copied().unwrap_or_default().add_modifier(Modifier::BOLD);
                    style_stack.push(style);
                }
                Tag::CodeBlock(_) => {
                    flush_line(&mut lines, &mut current);
                    in_code_block = true;
                }
                Tag::BlockQuote(_) => {
                    flush_line(&mut lines, &mut current);
                    blockquote_depth = blockquote_depth.saturating_add(1);
                }
                Tag::List(start) => {
                    let ordered = start.is_some();
                    let index = start.unwrap_or(1);
                    let indent = if ordered { index.to_string().len() + 2 } else { 2 };
                    list_stack.push((ordered, index as usize, indent));
                }
                Tag::Item => {
                    if let Some((ordered, index, indent)) = list_stack.last_mut() {
                        let prefix = if *ordered {
                            let p = format!("{}. ", *index);
                            *index += 1;
                            p
                        } else {
                            "• ".to_string()
                        };
                        pending_prefix = Some(prefix);
                        current_prefix = Some(" ".repeat(*indent));
                    }
                }
                Tag::Link { .. } => {
                    let style = style_stack
                        .last()
                        .copied()
                        .unwrap_or_default()
                        .fg(Color::LightBlue)
                        .add_modifier(Modifier::UNDERLINED);
                    style_stack.push(style);
                }
                Tag::Table(align) => {
                    flush_line(&mut lines, &mut current);
                    table_align = align;
                    table_rows.clear();
                    current_row.clear();
                    current_cell.clear();
                    in_table_cell = false;
                    table_header_rows = 0;
                    in_table_head = false;
                }
                Tag::TableHead => {
                    in_table_head = true;
                }
                Tag::TableRow => {
                    current_row.clear();
                }
                Tag::TableCell => {
                    in_table_cell = true;
                    current_cell.clear();
                }
                _ => {}
            },
            MdEvent::End(tag) => match tag {
                TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough | TagEnd::Heading(_) => {
                    if style_stack.len() > 1 {
                        style_stack.pop();
                    }
                    if matches!(tag, TagEnd::Heading(_)) {
                        flush_line(&mut lines, &mut current);
                        lines.push(Line::from(Span::raw("")));
                    }
                }
                TagEnd::Paragraph => {
                    flush_line(&mut lines, &mut current);
                    lines.push(Line::from(Span::raw("")));
                }
                TagEnd::CodeBlock => {
                    in_code_block = false;
                    lines.push(Line::from(Span::raw("")));
                }
                TagEnd::BlockQuote => {
                    flush_line(&mut lines, &mut current);
                    blockquote_depth = blockquote_depth.saturating_sub(1);
                    lines.push(Line::from(Span::raw("")));
                }
                TagEnd::List(_) => {
                    list_stack.pop();
                    pending_prefix = None;
                    current_prefix = None;
                    lines.push(Line::from(Span::raw("")));
                }
                TagEnd::Item => {
                    flush_line(&mut lines, &mut current);
                }
                TagEnd::Link => {
                    if style_stack.len() > 1 {
                        style_stack.pop();
                    }
                }
                TagEnd::TableCell => {
                    current_row.push(current_cell.trim().to_string());
                    current_cell.clear();
                    in_table_cell = false;
                }
                TagEnd::TableRow => {
                    if !current_row.is_empty() {
                        table_rows.push(current_row.clone());
                        if in_table_head {
                            table_header_rows += 1;
                        }
                    }
                    current_row.clear();
                }
                TagEnd::TableHead => {
                    if !current_row.is_empty() {
                        table_rows.push(current_row.clone());
                        table_header_rows += 1;
                        current_row.clear();
                    }
                    in_table_head = false;
                }
                TagEnd::Table => {
                    if !table_rows.is_empty() {
                        render_table_lines(&table_rows, &table_align, table_header_rows, &mut lines);
                        lines.push(Line::from(Span::raw("")));
                    }
                }
                _ => {}
            },
            MdEvent::Text(text) => {
                if in_code_block {
                    let code_style = Style::default().fg(Color::LightYellow).bg(Color::DarkGray);
                    for (i, line) in text.split('\n').enumerate() {
                        if i > 0 {
                            lines.push(Line::from(Span::styled("", code_style)));
                        }
                        lines.push(Line::from(Span::styled(line.to_string(), code_style)));
                    }
                } else if in_table_cell {
                    current_cell.push_str(&text);
                } else {
                    ensure_prefix(
                        &mut current,
                        &mut pending_prefix,
                        &current_prefix,
                        &list_stack,
                        blockquote_depth,
                    );
                    let style = *style_stack.last().unwrap_or(&Style::default());
                    current.push(Span::styled(text.to_string(), style));
                }
            }
            MdEvent::Code(code) => {
                if in_table_cell {
                    current_cell.push_str(&code);
                    continue;
                }
                ensure_prefix(
                    &mut current,
                    &mut pending_prefix,
                    &current_prefix,
                    &list_stack,
                    blockquote_depth,
                );
                let style = style_stack
                    .last()
                    .copied()
                    .unwrap_or_default()
                    .fg(Color::LightYellow)
                    .bg(Color::DarkGray);
                current.push(Span::styled(code.to_string(), style));
            }
            MdEvent::SoftBreak | MdEvent::HardBreak => {
                if in_table_cell {
                    current_cell.push(' ');
                } else {
                    flush_line(&mut lines, &mut current);
                }
            }
            MdEvent::Rule => {
                flush_line(&mut lines, &mut current);
                lines.push(Line::from(Span::raw("—".repeat(20))));
                lines.push(Line::from(Span::raw("")));
            }
            _ => {}
        }
    }

    if !current.is_empty() {
        lines.push(Line::from(current));
    }
    // Trim trailing empty lines
    while matches!(lines.last(), Some(line) if line.to_string().trim().is_empty()) {
        lines.pop();
    }
    lines
}

fn render_table_lines(
    rows: &[Vec<String>],
    align: &[Alignment],
    header_rows: usize,
    lines: &mut Vec<Line<'static>>,
) {
    let mut col_count = 0usize;
    for row in rows {
        col_count = col_count.max(row.len());
    }
    if col_count == 0 {
        return;
    }
    let mut widths = vec![3usize; col_count];
    for row in rows {
        for (idx, cell) in row.iter().enumerate() {
            let w = UnicodeWidthStr::width(cell.as_str());
            widths[idx] = widths[idx].max(w);
        }
    }

    let make_border = |left: char, mid: char, right: char| {
        let mut line = String::new();
        line.push(left);
        for col in 0..col_count {
            let segment = "─".repeat(widths[col] + 2);
            line.push_str(&segment);
            if col + 1 < col_count {
                line.push(mid);
            }
        }
        line.push(right);
        line
    };

    let top = make_border('╭', '┬', '╮');
    lines.push(Line::from(Span::raw(top)));

    for (row_idx, row) in rows.iter().enumerate() {
        let mut line = String::new();
        for col in 0..col_count {
            let cell = row.get(col).map(|s| s.as_str()).unwrap_or("");
            let width = widths[col];
            let cell_width = UnicodeWidthStr::width(cell);
            let pad = width.saturating_sub(cell_width);
            let (pad_left, pad_right) = match align.get(col).copied().unwrap_or(Alignment::Left) {
                Alignment::Right => (pad, 0),
                Alignment::Center => (pad / 2, pad - pad / 2),
                _ => (0, pad),
            };
            line.push('│');
            line.push(' ');
            line.push_str(&" ".repeat(pad_left));
            line.push_str(cell);
            line.push_str(&" ".repeat(pad_right));
            line.push(' ');
        }
        line.push('│');
        if row_idx < header_rows {
            lines.push(Line::from(Span::styled(
                line,
                Style::default().add_modifier(Modifier::BOLD),
            )));
        } else {
            lines.push(Line::from(Span::raw(line)));
        }

        if row_idx + 1 == header_rows {
            let sep = make_border('├', '┼', '┤');
            lines.push(Line::from(Span::raw(sep)));
        } else if row_idx + 1 < rows.len() {
            let sep = make_border('├', '┼', '┤');
            lines.push(Line::from(Span::raw(sep)));
        }
    }

    let bottom = make_border('╰', '┴', '╯');
    lines.push(Line::from(Span::raw(bottom)));
}

fn dot_span(color: DotColor) -> Span<'static> {
    let fg = match color {
        DotColor::White => Color::White,
        DotColor::Green => Color::Green,
        DotColor::Red => Color::Red,
    };
    Span::styled("●", Style::default().fg(fg))
}

fn tool_dot_span(color: DotColor) -> Span<'static> {
    let fg = match color {
        DotColor::White => Color::White,
        DotColor::Green => Color::Green,
        DotColor::Red => Color::Red,
    };
    Span::styled("⏺", Style::default().fg(fg))
}

fn format_tool_box_lines(lines: &[String], width: u16) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let mut box_width = width.saturating_sub(6) as usize;
    if box_width < 10 {
        box_width = 10;
    }
    let inner = box_width.saturating_sub(2);
    let top = format!("  ╰╭{}╮", "─".repeat(inner));
    out.push(Line::from(Span::styled(top, Style::default().fg(BORDER_COLOR))));
    for (idx, line) in lines.iter().enumerate() {
        let trimmed = truncate_to_width(line, inner);
        let pad = inner.saturating_sub(UnicodeWidthStr::width(trimmed.as_str()));
        let prefix = if idx == 0 { "   │" } else { "   │" };
        let mut spans = Vec::new();
        spans.push(Span::styled(prefix.to_string(), Style::default().fg(BORDER_COLOR)));
        spans.push(Span::raw(format!("{}{}", trimmed, " ".repeat(pad))));
        spans.push(Span::styled("│", Style::default().fg(BORDER_COLOR)));
        out.push(Line::from(spans));
    }
    let bottom = format!("   ╰{}╯", "─".repeat(inner));
    out.push(Line::from(Span::styled(bottom, Style::default().fg(BORDER_COLOR))));
    out
}

fn truncate_to_width(text: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_string();
    }
    if max_width <= 3 {
        return text.chars().take(max_width).collect();
    }
    let mut out = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let w = UnicodeWidthStr::width(ch.to_string().as_str());
        if used + w > max_width - 3 {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push_str("...");
    out
}

fn line_is_blank(line: &Line<'static>) -> bool {
    line.to_string().trim().is_empty()
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

fn format_tool_label(name: &str, args: &str, width: u16) -> String {
    let base = name.to_string();
    if args.is_empty() {
        return base;
    }
    let max_label = if width == 0 { 160 } else { width as usize }.saturating_sub(2);
    let mut full = format!("{base} {args}");
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

fn char_to_byte_idx(text: &str, char_idx: usize) -> usize {
    if char_idx == 0 {
        return 0;
    }
    text.char_indices()
        .nth(char_idx)
        .map(|(idx, _)| idx)
        .unwrap_or_else(|| text.len())
}

fn count_wrapped_lines(lines: &[Line<'static>], width: u16) -> usize {
    if width == 0 {
        return 0;
    }
    lines
        .iter()
        .map(|line| {
            let text = line.to_string();
            let w = UnicodeWidthStr::width(text.as_str());
            let width = width as usize;
            let used = if w == 0 { 1 } else { (w + width - 1) / width };
            used.max(1)
        })
        .sum()
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints(
            [
                Constraint::Percentage((100 - percent_y) / 2),
                Constraint::Percentage(percent_y),
                Constraint::Percentage((100 - percent_y) / 2),
            ]
            .as_ref(),
        )
        .split(r);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints(
            [
                Constraint::Percentage((100 - percent_x) / 2),
                Constraint::Percentage(percent_x),
                Constraint::Percentage((100 - percent_x) / 2),
            ]
            .as_ref(),
        )
        .split(popup_layout[1])[1]
}
