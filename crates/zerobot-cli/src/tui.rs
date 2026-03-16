use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, event::EnableMouseCapture, event::DisableMouseCapture};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap, Clear};
use ratatui::{Frame, Terminal};
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
        }
    }

    fn push_line(&mut self, line: Line<'static>) {
        self.output.push(line);
    }

    fn push_block(&mut self, color: DotColor, text: &str) {
        self.output.extend(format_block_lines(color, text));
    }

    fn push_tool_output(&mut self, color: DotColor, label: Option<&str>, output: &str) {
        let (lines, omitted) = truncate_lines(output, 3);
        if lines.is_empty() {
            if let Some(label) = label {
                self.push_block(color, label);
            } else {
                self.push_block(color, "");
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
        self.push_block(color, &joined);
    }

    fn append_stream_delta(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if !self.streaming {
            self.streaming = true;
            self.stream_buffer.clear();
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
        self.output.extend(format_block_lines(DotColor::White, &content));
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

    fn status_line(&self) -> String {
        match &self.status {
            Status::Idle => "空闲".to_string(),
            Status::Thinking => "思考中".to_string(),
            Status::Tool(name) => format!("工具执行中: {name}"),
            Status::Error(message) => format!("错误: {message}"),
        }
    }

    fn info_lines(&self) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        lines.push(Line::from(Span::raw(format!("状态: {}", self.status_line()))));
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
            "/exit /help /clear".to_string()
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
) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
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
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
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
    let mut app = App::new(session_id.clone(), provider_id, model.clone());
    app.push_line(Line::from(Span::styled(
        "ZeroBot",
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
    )));
    app.push_line(Line::from(Span::styled(
        format!("会话已启动: {}", session_id),
        Style::default().fg(Color::Green),
    )));
    app.push_line(Line::from(Span::raw("输入 /exit 退出")));

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
                    if input == "/help" {
                        app.push_block(DotColor::White, "可用命令: /exit /help /clear");
                        app.input.clear();
                        app.cursor = 0;
                        return Ok(true);
                    }

                    app.input.clear();
                    app.cursor = 0;
                    app.status = Status::Thinking;

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
                    app.scroll = app.scroll.saturating_add(5);
                    return Ok(true);
                }
                KeyCode::PageDown => {
                    app.scroll = app.scroll.saturating_sub(5);
                    if app.scroll == 0 {
                        app.stick_to_bottom = true;
                    }
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
                app.scroll = app.scroll.saturating_add(1);
                return Ok(true);
            }
            MouseEventKind::ScrollDown => {
                app.scroll = app.scroll.saturating_sub(1);
                if app.scroll == 0 {
                    app.stick_to_bottom = true;
                }
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
            app.push_block(DotColor::White, &content);
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
            app.push_tool_output(color, label.as_deref(), output.trim());
            app.last_tool_label = None;
            app.status = Status::Thinking;
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

fn draw(frame: &mut Frame, app: &mut App) {
    let size = frame.size();
    let info_lines = app.info_lines();
    let info_height = info_lines.len().max(1) as u16;

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(info_height),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(size);

    let output_area = chunks[0];
    let info_area = chunks[1];
    let input_area = chunks[2];
    let status_area = chunks[3];

    app.viewport_width = output_area.width;

    let display_lines = app.display_lines();
    let total_lines = count_wrapped_lines(&display_lines, output_area.width);
    let visible_height = output_area.height as usize;
    let max_scroll = total_lines.saturating_sub(visible_height);
    if app.stick_to_bottom {
        app.scroll = max_scroll as u16;
    } else if (app.scroll as usize) > max_scroll {
        app.scroll = max_scroll as u16;
    }

    let output_widget = Paragraph::new(Text::from(display_lines))
        .wrap(Wrap { trim: false })
        .scroll((app.scroll, 0));
    frame.render_widget(output_widget, output_area);

    let info_widget = Paragraph::new(Text::from(info_lines));
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
        .borders(Borders::ALL);
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

fn dot_span(color: DotColor) -> Span<'static> {
    let fg = match color {
        DotColor::White => Color::White,
        DotColor::Green => Color::Green,
        DotColor::Red => Color::Red,
    };
    Span::styled("●", Style::default().fg(fg))
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
