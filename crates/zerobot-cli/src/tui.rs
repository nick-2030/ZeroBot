use anyhow::Result;
use base64::Engine as _;
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::{execute, Command};
use pulldown_cmark::{Alignment, CodeBlockKind, Event as MdEvent, Options, Parser, Tag, TagEnd};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap, Clear, BorderType};
use ratatui::{Frame, Terminal};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, OnceLock, RwLock as StdRwLock};
use std::time::Instant;
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Theme, ThemeSet};
use syntect::parsing::SyntaxSet;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::sync::RwLock as TokioRwLock;
use tokio::time::{self, Duration};
use tokio_stream::StreamExt;
use unicode_width::UnicodeWidthStr;
use zerobot_core::agent::Agent;
use zerobot_core::config::Settings;
use zerobot_core::events::AgentEvent;
use zerobot_core::hooks::HookManager;
use zerobot_core::interaction::{
    InteractionHandler,
    ToolApprovalDecision,
    ToolApprovalRequest,
    ToolApprovalResponse,
    UserInputAnswer,
    UserInputRequest,
    UserInputResponse,
};
use zerobot_core::provider::{ProviderFactory, TokenUsage};
use zerobot_core::session::{create_session_with_hooks, SessionKind, SessionStore, TodoItem, TodoStatus};
use zerobot_core::skills::SkillStackEntry;
use zerobot_core::tool::ToolRegistry;
use zerobot_core::ZeroBotError;
use zerobot_core::init_prompt;
use crate::slash::{SlashMatch, SlashRegistry};

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
    WaitingUserInput,
    WaitingApproval,
}

struct PermissionPrompt {
    title: String,
    options: Vec<String>,
    selected: usize,
}

enum InfoOverlay {
    UserInput(UserInputOverlay),
    ToolApproval(ToolApprovalOverlay),
}

struct UserInputOverlay {
    request: UserInputRequest,
    current: usize,
    selected: usize,
    focus: UserInputFocus,
    notes: HashMap<(String, Option<String>), String>,
    answers: HashMap<String, UserInputAnswer>,
    respond_to: Option<oneshot::Sender<UserInputResponse>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum UserInputFocus {
    Options,
    Input,
}

struct ToolApprovalOverlay {
    request: ToolApprovalRequest,
    selected: usize,
    respond_to: Option<oneshot::Sender<ToolApprovalResponse>>,
}

enum OverlayAction<T> {
    None,
    Updated,
    Complete(T),
}

enum UiRequest {
    UserInput {
        request: UserInputRequest,
        respond_to: oneshot::Sender<UserInputResponse>,
    },
    ToolApproval {
        request: ToolApprovalRequest,
        respond_to: oneshot::Sender<ToolApprovalResponse>,
    },
}

struct UiInteractionHandler {
    tx: mpsc::UnboundedSender<UiRequest>,
}

#[async_trait::async_trait]
impl InteractionHandler for UiInteractionHandler {
    async fn request_user_input(
        &self,
        request: UserInputRequest,
    ) -> Result<UserInputResponse, ZeroBotError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(UiRequest::UserInput {
                request,
                respond_to: tx,
            })
            .map_err(|_| ZeroBotError::Tool("无法发送用户输入请求".to_string()))?;
        rx.await
            .map_err(|_| ZeroBotError::Tool("等待用户输入失败".to_string()))
    }

    async fn request_tool_approval(
        &self,
        request: ToolApprovalRequest,
    ) -> Result<ToolApprovalResponse, ZeroBotError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(UiRequest::ToolApproval {
                request,
                respond_to: tx,
            })
            .map_err(|_| ZeroBotError::Tool("无法发送授权请求".to_string()))?;
        rx.await
            .map_err(|_| ZeroBotError::Tool("等待授权失败".to_string()))
    }
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
    context_used: Option<usize>,
    context_limit: Option<u32>,
    permission_prompt: Option<PermissionPrompt>,
    info_overlay: Option<InfoOverlay>,
    overlay_queue: VecDeque<InfoOverlay>,
    overlay_prev_status: Option<Status>,
    viewport_width: u16,
    blink_on: bool,
    last_blink: Instant,
    last_copyable_output: Option<String>,
    slash_query: Option<String>,
    slash_matches: Vec<SlashMatch>,
    slash_selected: usize,
    slash_page: usize,
    slash_hint: String,
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
            context_used: None,
            context_limit: None,
            permission_prompt: None,
            info_overlay: None,
            overlay_queue: VecDeque::new(),
            overlay_prev_status: None,
            viewport_width: 0,
            blink_on: true,
            last_blink: Instant::now(),
            last_copyable_output: None,
            slash_query: None,
            slash_matches: Vec::new(),
            slash_selected: 0,
            slash_page: 0,
            slash_hint: String::new(),
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
        self.output.extend(format_markdown_lines(text, self.viewport_width));
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
        self.output
            .extend(format_markdown_lines(&content, self.viewport_width));
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

    fn refresh_slash(&mut self, registry: &SlashRegistry) {
        let next_query = slash_query(&self.input);
        if next_query != self.slash_query {
            self.slash_selected = 0;
            self.slash_page = 0;
        }
        self.slash_query = next_query;
        if let Some(query) = &self.slash_query {
            self.slash_matches = registry.matches(query);
            clamp_slash_selection(self);
        } else {
            self.slash_matches.clear();
            self.slash_selected = 0;
            self.slash_page = 0;
        }
    }

    fn slash_active(&self) -> bool {
        self.slash_query.is_some()
    }

    fn enqueue_overlay(&mut self, overlay: InfoOverlay) {
        if self.info_overlay.is_some() {
            self.overlay_queue.push_back(overlay);
        } else {
            self.activate_overlay(overlay);
        }
    }

    fn activate_overlay(&mut self, overlay: InfoOverlay) {
        if self.overlay_prev_status.is_none() {
            self.overlay_prev_status = Some(self.status.clone());
        }
        self.status = overlay.status();
        self.info_overlay = Some(overlay);
    }

    fn dismiss_overlay(&mut self) {
        if let Some(next) = self.overlay_queue.pop_front() {
            self.activate_overlay(next);
            return;
        }
        self.info_overlay = None;
        if let Some(prev) = self.overlay_prev_status.take() {
            self.status = prev;
        }
    }

    fn info_lines(&self) -> Vec<Line<'static>> {
        if let Some(overlay) = &self.info_overlay {
            return overlay.lines();
        }
        let mut lines = Vec::new();
        let status_line = match &self.status {
            Status::Idle => Line::from(Span::raw("状态: 空闲")),
            Status::Thinking => {
                let dot = if self.blink_on { "●" } else { " " };
                Line::from(Span::raw(format!("状态: {dot} 努力工作中")))
            }
            Status::Tool(name) => Line::from(Span::raw(format!("状态: 工具执行中: {name}"))),
            Status::Error(message) => Line::from(Span::raw(format!("状态: 错误: {message}"))),
            Status::WaitingUserInput => Line::from(Span::raw("状态: 等待用户输入")),
            Status::WaitingApproval => Line::from(Span::raw("状态: 等待授权")),
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
        if self.slash_active() {
            lines.push(Line::from(Span::styled(
                "Commands:",
                Style::default().add_modifier(Modifier::BOLD),
            )));
            if self.slash_matches.is_empty() {
                lines.push(Line::from(Span::raw("  （无匹配）")));
            } else {
                let page_size = slash_page_size();
                let (page, pages) = slash_page_info(self.slash_matches.len(), self.slash_page);
                let start = page * page_size;
                let end = (start + page_size).min(self.slash_matches.len());
                for (idx, cmd) in self.slash_matches[start..end].iter().enumerate() {
                    let absolute_idx = start + idx;
                    let prefix = if absolute_idx == self.slash_selected {
                        "> "
                    } else {
                        "  "
                    };
                    let line = format!("{prefix}/{} - {}", cmd.name, cmd.description);
                    lines.push(Line::from(Span::raw(line)));
                }
                if pages > 1 {
                    lines.push(Line::from(Span::raw(format!(
                        "  Page {}/{}",
                        page + 1,
                        pages
                    ))));
                }
            }
            lines.push(Line::from(Span::raw("↑/↓ 选择  ←/→ 翻页  Enter/Tab 补全")));
        }
        lines
    }

    fn command_hint(&self) -> String {
        if self.input.trim_start().starts_with('/') {
            self.slash_hint.clone()
        } else {
            String::new()
        }
    }
}

impl InfoOverlay {
    fn status(&self) -> Status {
        match self {
            InfoOverlay::UserInput(_) => Status::WaitingUserInput,
            InfoOverlay::ToolApproval(_) => Status::WaitingApproval,
        }
    }

    fn lines(&self) -> Vec<Line<'static>> {
        match self {
            InfoOverlay::UserInput(overlay) => overlay.lines(),
            InfoOverlay::ToolApproval(overlay) => overlay.lines(),
        }
    }

    fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> OverlayAction<()> {
        match self {
            InfoOverlay::UserInput(overlay) => overlay.handle_key(key),
            InfoOverlay::ToolApproval(overlay) => overlay.handle_key(key),
        }
    }
}

impl UserInputOverlay {
    fn new(request: UserInputRequest, respond_to: oneshot::Sender<UserInputResponse>) -> Self {
        let focus = if request
            .questions
            .get(0)
            .and_then(|q| q.options.as_ref())
            .is_some()
        {
            UserInputFocus::Options
        } else {
            UserInputFocus::Input
        };
        Self {
            request,
            current: 0,
            selected: 0,
            focus,
            notes: HashMap::new(),
            answers: HashMap::new(),
            respond_to: Some(respond_to),
        }
    }

    fn current_question(&self) -> Option<&zerobot_core::interaction::UserInputQuestion> {
        self.request.questions.get(self.current)
    }

    fn current_option_id(&self) -> Option<String> {
        let question = self.current_question()?;
        let options = question.options.as_ref()?;
        options.get(self.selected).map(|opt| opt.id.clone())
    }

    fn reset_focus_for_current(&mut self) {
        let has_options = self
            .current_question()
            .and_then(|q| q.options.as_ref())
            .is_some();
        self.focus = if has_options {
            UserInputFocus::Options
        } else {
            UserInputFocus::Input
        };
    }

    fn note_key(&self) -> Option<(String, Option<String>)> {
        let question = self.current_question()?;
        Some((question.id.clone(), self.current_option_id()))
    }

    fn current_note_mut(&mut self) -> Option<&mut String> {
        let key = self.note_key()?;
        Some(self.notes.entry(key).or_default())
    }

    fn current_note(&self) -> String {
        let key = match self.note_key() {
            Some(key) => key,
            None => return String::new(),
        };
        self.notes.get(&key).cloned().unwrap_or_default()
    }

    fn commit_current_answer(&mut self) {
        let Some(question) = self.current_question() else {
            return;
        };
        let note = self.current_note();
        let note = if note.trim().is_empty() { None } else { Some(note) };
        let answer = UserInputAnswer {
            option_id: self.current_option_id(),
            note,
        };
        self.answers.insert(question.id.clone(), answer);
    }

    fn finish(&mut self, cancelled: bool) -> Option<UserInputResponse> {
        let respond_to = self.respond_to.take()?;
        let response = UserInputResponse {
            answers: self.answers.clone(),
            cancelled,
        };
        let _ = respond_to.send(response);
        None
    }

    fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> OverlayAction<()> {
        match key.code {
            KeyCode::Up => {
                if self.focus == UserInputFocus::Options {
                    if self.selected > 0 {
                        self.selected -= 1;
                        return OverlayAction::Updated;
                    }
                }
            }
            KeyCode::Down => {
                if self.focus == UserInputFocus::Options {
                    if let Some(question) = self.current_question() {
                        if let Some(options) = &question.options {
                            if self.selected + 1 < options.len() {
                                self.selected += 1;
                                return OverlayAction::Updated;
                            }
                        }
                    }
                }
            }
            KeyCode::Backspace => {
                if self.focus == UserInputFocus::Input {
                    if let Some(note) = self.current_note_mut() {
                        note.pop();
                        return OverlayAction::Updated;
                    }
                }
            }
            KeyCode::Tab => {
                self.focus = if self.focus == UserInputFocus::Options {
                    UserInputFocus::Input
                } else {
                    UserInputFocus::Options
                };
                return OverlayAction::Updated;
            }
            KeyCode::Left => {
                if self.current > 0 {
                    self.commit_current_answer();
                    self.current -= 1;
                    self.selected = 0;
                    self.reset_focus_for_current();
                    return OverlayAction::Updated;
                }
            }
            KeyCode::Right => {
                if self.current + 1 < self.request.questions.len() {
                    self.commit_current_answer();
                    self.current += 1;
                    self.selected = 0;
                    self.reset_focus_for_current();
                    return OverlayAction::Updated;
                }
            }
            KeyCode::Enter => {
                self.commit_current_answer();
                if self.current + 1 >= self.request.questions.len() {
                    self.finish(false);
                    return OverlayAction::Complete(());
                }
                self.current += 1;
                self.selected = 0;
                self.reset_focus_for_current();
                return OverlayAction::Updated;
            }
            KeyCode::Esc => {
                self.finish(true);
                return OverlayAction::Complete(());
            }
            KeyCode::Char(ch) => {
                if !key.modifiers.contains(KeyModifiers::CONTROL) {
                    self.focus = UserInputFocus::Input;
                    if let Some(note) = self.current_note_mut() {
                        note.push(ch);
                        return OverlayAction::Updated;
                    }
                }
            }
            _ => {}
        }
        OverlayAction::None
    }

    fn lines(&self) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let title = self
            .request
            .title
            .clone()
            .unwrap_or_else(|| "需要用户输入".to_string());
        lines.push(Line::from(Span::styled(
            title,
            Style::default().add_modifier(Modifier::BOLD),
        )));
        if self.request.questions.len() > 1 {
            let mut spans = Vec::new();
            spans.push(Span::raw("问题: "));
            for (idx, q) in self.request.questions.iter().enumerate() {
                let label = truncate_chars(&q.prompt, 12);
                let text = format!("{}{} ", idx + 1, label);
                if idx == self.current {
                    spans.push(Span::styled(
                        text,
                        Style::default().add_modifier(Modifier::BOLD),
                    ));
                } else {
                    spans.push(Span::raw(text));
                }
            }
            lines.push(Line::from(spans));
        }
        if let Some(question) = self.current_question() {
            lines.push(Line::from(Span::raw(format!(
                "问题 {}/{}: {}",
                self.current + 1,
                self.request.questions.len(),
                question.prompt
            ))));
            if let Some(options) = &question.options {
                for (idx, opt) in options.iter().enumerate() {
                    let prefix = if idx == self.selected && self.focus == UserInputFocus::Options {
                        "> "
                    } else {
                        "  "
                    };
                    lines.push(Line::from(Span::raw(format!("{prefix}{}", opt.label))));
                }
            } else {
                lines.push(Line::from(Span::raw("（无选项，直接输入）")));
            }
            let note = self.current_note();
            let prefix = if self.focus == UserInputFocus::Input { "> " } else { "  " };
            lines.push(Line::from(Span::raw(format!("{prefix}输入内容: {note}"))));
        }
        lines.push(Line::from(Span::raw(
            "↑/↓ 选择  ←/→ 切换  Tab 切换输入  Enter 下一项/提交  Esc 取消",
        )));
        lines
    }
}

impl ToolApprovalOverlay {
    fn new(request: ToolApprovalRequest, respond_to: oneshot::Sender<ToolApprovalResponse>) -> Self {
        Self {
            request,
            selected: 0,
            respond_to: Some(respond_to),
        }
    }

    fn finish(&mut self, decision: ToolApprovalDecision) -> Option<ToolApprovalResponse> {
        let respond_to = self.respond_to.take()?;
        let response = ToolApprovalResponse { decision };
        let _ = respond_to.send(response);
        None
    }

    fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> OverlayAction<()> {
        match key.code {
            KeyCode::Up => {
                if self.selected > 0 {
                    self.selected -= 1;
                    return OverlayAction::Updated;
                }
            }
            KeyCode::Down => {
                if self.selected + 1 < 3 {
                    self.selected += 1;
                    return OverlayAction::Updated;
                }
            }
            KeyCode::Enter => {
                let decision = match self.selected {
                    0 => ToolApprovalDecision::AllowOnce,
                    1 => ToolApprovalDecision::AllowSession,
                    _ => ToolApprovalDecision::Deny,
                };
                self.finish(decision);
                return OverlayAction::Complete(());
            }
            KeyCode::Esc => {
                self.finish(ToolApprovalDecision::Deny);
                return OverlayAction::Complete(());
            }
            _ => {}
        }
        OverlayAction::None
    }

    fn lines(&self) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        lines.push(Line::from(Span::styled(
            "需要工具授权".to_string(),
            Style::default().add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::raw(format!(
            "工具: {}",
            self.request.tool_name
        ))));
        if let Some(reason) = &self.request.reason {
            if !reason.trim().is_empty() {
                lines.push(Line::from(Span::raw(format!("原因: {reason}"))));
            }
        }
        if let Ok(args) = serde_json::to_string(&self.request.arguments) {
            let args = one_line(&args);
            if !args.is_empty() {
                lines.push(Line::from(Span::raw(format!("参数: {args}"))));
            }
        }
        let options = ["仅本次允许", "本会话允许", "拒绝"];
        for (idx, opt) in options.iter().enumerate() {
            let prefix = if idx == self.selected { "> " } else { "  " };
            lines.push(Line::from(Span::raw(format!("{prefix}{opt}"))));
        }
        lines.push(Line::from(Span::raw("↑/↓ 选择  Enter 确认  Esc 取消")));
        lines
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
    provider_state: Arc<StdRwLock<String>>,
    tool_approvals: Arc<TokioRwLock<HashSet<String>>>,
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
        provider_state,
        tool_approvals,
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
    provider_state: Arc<StdRwLock<String>>,
    tool_approvals: Arc<TokioRwLock<HashSet<String>>>,
) -> Result<()> {
    let slash = SlashRegistry::extended();
    let mut app = App::new(session_id.clone(), provider_id.clone(), model.clone());
    app.slash_hint = slash.hint(6);
    app.refresh_slash(&slash);
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
    let (ui_tx, mut ui_rx) = mpsc::unbounded_channel::<UiRequest>();
    let interaction: Arc<dyn InteractionHandler> = Arc::new(UiInteractionHandler { tx: ui_tx });
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
                        if handle_event(
                            event,
                            &mut app,
                            &mut runner,
                            &settings,
                            &cwd,
                            &store,
                            &tools,
                            &provider_factory,
                            &slash,
                            &hooks,
                            &interaction,
                            &provider_state,
                            &tool_approvals,
                            &tx,
                            &mut should_quit,
                        )
                        .await?
                        {
                            dirty = true;
                        }
                    }
                }
                Some(event) = rx.recv() => {
                    handle_agent_event(event, &mut app, &store).await;
                    dirty = true;
                }
                Some(req) = ui_rx.recv() => {
                    handle_ui_request(req, &mut app);
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
                        if handle_event(
                            event,
                            &mut app,
                            &mut runner,
                            &settings,
                            &cwd,
                            &store,
                            &tools,
                            &provider_factory,
                            &slash,
                            &hooks,
                            &interaction,
                            &provider_state,
                            &tool_approvals,
                            &tx,
                            &mut should_quit,
                        )
                        .await?
                        {
                            dirty = true;
                        }
                    }
                }
                Some(event) = rx.recv() => {
                    handle_agent_event(event, &mut app, &store).await;
                    dirty = true;
                }
                Some(req) = ui_rx.recv() => {
                    handle_ui_request(req, &mut app);
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
    slash: &SlashRegistry,
    hooks: &HookManager,
    interaction: &Arc<dyn InteractionHandler>,
    provider_state: &Arc<StdRwLock<String>>,
    tool_approvals: &Arc<TokioRwLock<HashSet<String>>>,
    tx: &mpsc::UnboundedSender<AgentEvent>,
    should_quit: &mut bool,
) -> Result<bool> {
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => {
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                *should_quit = true;
                return Ok(true);
            }
            if app.info_overlay.is_some() {
                if handle_overlay_key(key, app) {
                    return Ok(true);
                }
                return Ok(false);
            }
            match key.code {
                KeyCode::Enter => {
                    if runner.is_some() {
                        return Ok(false);
                    }
                    let raw_input = app.input.trim().to_string();
                    if raw_input.is_empty() {
                        return Ok(false);
                    }
                    if app.slash_active() && !app.slash_matches.is_empty() {
                        let trimmed = raw_input.trim_start();
                        let after_slash = trimmed.strip_prefix('/').unwrap_or("");
                        if !after_slash.chars().any(|c| c.is_whitespace()) {
                            let chosen = app.slash_matches[app.slash_selected].name.clone();
                            app.input = format!("/{chosen} ");
                            app.cursor = app.input.chars().count();
                            app.refresh_slash(slash);
                            return Ok(true);
                        }
                    }
                    let slash_input = if raw_input == "exit" {
                        "/exit".to_string()
                    } else {
                        raw_input.clone()
                    };
                    if slash_input.starts_with('/') {
                        let (command, args) = parse_slash_command(&slash_input);
                        if command.is_empty() {
                            app.push_block(DotColor::Red, "请输入命令，使用 /help 查看可用命令");
                            return Ok(true);
                        }
                        let Some(spec) = slash.find(&command) else {
                            app.push_block(
                                DotColor::Red,
                                &format!("未知命令: /{command}（输入 /help 查看）"),
                            );
                            return Ok(true);
                        };
                        match spec.name {
                            "clear" => {
                                app.output.clear();
                                app.stream_buffer.clear();
                                app.streaming = false;
                                app.scroll = 0;
                                app.stick_to_bottom = true;
                                app.input.clear();
                                app.cursor = 0;
                                app.refresh_slash(slash);
                                return Ok(true);
                            }
                            "exit" => {
                                app.push_line(user_input_line(&raw_input));
                                *should_quit = true;
                                return Ok(true);
                            }
                            _ => {
                                app.push_line(user_input_line(&raw_input));
                            }
                        }

                        match spec.name {
                            "help" => {
                                let target = args.split_whitespace().next().unwrap_or("");
                                let message = if target.is_empty() {
                                    let mut lines = Vec::new();
                                    lines.push("可用命令:".to_string());
                                    for cmd in slash.commands() {
                                        lines.push(format!("  {} - {}", cmd.usage, cmd.description));
                                    }
                                    lines.push("输入 /help <命令> 查看用法".to_string());
                                    lines.join("\n")
                                } else if let Some(cmd) = slash.find(target) {
                                    format!("{} - {}", cmd.usage, cmd.description)
                                } else {
                                    format!("未知命令: /{target}（输入 /help 查看）")
                                };
                                let color = if slash.find(target).is_some() || target.is_empty() {
                                    DotColor::White
                                } else {
                                    DotColor::Red
                                };
                                app.push_block(color, &message);
                            }
                            "copy" => {
                                let message = match app.last_copyable_output.as_deref() {
                                    Some(text) => match copy_text_to_clipboard(text) {
                                        Ok(()) => "已复制最新回复到剪贴板".to_string(),
                                        Err(err) => format!("复制失败: {err}"),
                                    },
                                    None => "暂无可复制内容".to_string(),
                                };
                                app.push_block(DotColor::White, &message);
                            }
                            "tools" => {
                                let mut names = tools.names();
                                names.sort();
                                let enabled = if settings.tools.enabled.is_empty() {
                                    "（无）".to_string()
                                } else {
                                    settings.tools.enabled.join(", ")
                                };
                                let registered = if names.is_empty() {
                                    "（无）".to_string()
                                } else {
                                    names.join(", ")
                                };
                                let message = format!("启用工具: {enabled}\n已注册工具: {registered}");
                                app.push_block(DotColor::White, &message);
                            }
                            "init" => {
                                let prompt = init_prompt(cwd, &args);
                                app.input.clear();
                                app.cursor = 0;
                                app.refresh_slash(slash);
                                app.status = Status::Thinking;
                                app.blink_on = true;
                                app.last_blink = Instant::now();

                                let provider = (provider_factory)()?;
                                let agent = Agent::new(
                                    provider,
                                    app.model.clone(),
                                    settings.clone(),
                                    store.clone(),
                                    tools.clone(),
                                    cwd.clone(),
                                    hooks.clone(),
                                    Some(interaction.clone()),
                                    tool_approvals.clone(),
                                );
                                let session_id = app.session_id.clone();
                                let tx_clone = tx.clone();
                                *runner = Some(tokio::spawn(async move {
                                    agent.run_turn(&session_id, &prompt, Some(tx_clone)).await
                                }));
                                return Ok(true);
                            }
                            "model" => {
                                let args_lower = args.to_lowercase();
                                if args.is_empty() || args_lower == "list" {
                                    let mut lines = Vec::new();
                                    lines.push(format!("当前模型: {}", app.model));
                                    if let Some(info) = settings.providers.get(&app.provider_id) {
                                        if let Some(model) = &info.model {
                                            lines.push(format!("提供商默认模型: {model}"));
                                        }
                                    }
                                    if let Some(model) = &settings.default_model {
                                        lines.push(format!("全局默认模型: {model}"));
                                    }
                                    lines.push("使用 /model <name> 设置模型".to_string());
                                    app.push_block(DotColor::White, &lines.join("\n"));
                                } else {
                                    app.model = args.clone();
                                    app.push_block(
                                        DotColor::White,
                                        &format!("已切换模型: {}", app.model),
                                    );
                                }
                            }
                            "provider" => {
                                let args_lower = args.to_lowercase();
                                if args.is_empty() || args_lower == "list" {
                                    let mut lines = Vec::new();
                                    lines.push(format!("当前提供商: {}", app.provider_id));
                                    if settings.providers.is_empty() {
                                        lines.push("未配置 providers".to_string());
                                    } else {
                                        let mut items = settings
                                            .providers
                                            .iter()
                                            .map(|(id, info)| (id.clone(), info.kind.clone(), info.model.clone()))
                                            .collect::<Vec<_>>();
                                        items.sort_by(|a, b| a.0.cmp(&b.0));
                                        for (id, kind, model) in items {
                                            let suffix = if id == app.provider_id { " *" } else { "" };
                                            let model = model.map(|m| format!(", model={m}")).unwrap_or_default();
                                            lines.push(format!("  {id} ({kind}{model}){suffix}"));
                                        }
                                    }
                                    app.push_block(DotColor::White, &lines.join("\n"));
                                } else {
                                    let target = args_lower;
                                    let exists = settings.providers.contains_key(&target)
                                        || matches!(target.as_str(), "openai" | "anthropic");
                                    if !exists {
                                        app.push_block(DotColor::Red, "未知提供商（输入 /provider list 查看）");
                                    } else {
                                        app.provider_id = target.clone();
                                        if let Ok(mut guard) = provider_state.write() {
                                            *guard = target.clone();
                                        }
                                        if let Some(info) = settings.providers.get(&app.provider_id) {
                                            if let Some(model) = &info.model {
                                                app.model = model.clone();
                                            }
                                        }
                                        app.push_block(
                                            DotColor::White,
                                            &format!("已切换提供商: {}", app.provider_id),
                                        );
                                    }
                                }
                            }
                            "config" => {
                                let args_lower = args.to_lowercase();
                                if args_lower == "show" || args.is_empty() {
                                    let yaml = masked_settings_yaml(settings);
                                    app.push_block(DotColor::White, &yaml);
                                } else {
                                    app.push_block(DotColor::Red, "用法: /config show");
                                }
                            }
                            "session" => {
                                let mut parts = args.split_whitespace();
                                let action = parts.next().unwrap_or("").to_lowercase();
                                match action.as_str() {
                                    "list" => {
                                        match store.list_sessions().await {
                                            Ok(sessions) => {
                                                let mut lines = Vec::new();
                                                lines.push("会话列表:".to_string());
                                                for session in sessions {
                                                    lines.push(format!("  {}\t{}", session.id, session.title));
                                                }
                                                app.push_block(DotColor::White, &lines.join("\n"));
                                            }
                                            Err(err) => {
                                                app.push_block(DotColor::Red, &format!("读取会话失败: {err}"));
                                            }
                                        }
                                    }
                                    "new" => {
                                        let title = parts.collect::<Vec<_>>().join(" ");
                                        let title = if title.trim().is_empty() {
                                            "新会话".to_string()
                                        } else {
                                            title
                                        };
                                        match create_session_with_hooks(
                                            store.as_ref(),
                                            hooks,
                                            title,
                                            None,
                                            SessionKind::Main,
                                        )
                                        .await
                                        {
                                            Ok(session) => {
                                                app.session_id = session.id;
                                                app.output.clear();
                                                app.stream_buffer.clear();
                                                app.streaming = false;
                                                app.scroll = 0;
                                                app.stick_to_bottom = true;
                                                app.status = Status::Idle;
                                                app.context_used = None;
                                                app.context_limit = None;
                                                refresh_session_state(app, store).await;
                                                app.push_block(DotColor::White, "已创建并切换到新会话");
                                            }
                                            Err(err) => {
                                                app.push_block(DotColor::Red, &format!("创建会话失败: {err}"));
                                            }
                                        }
                                    }
                                    "show" => {
                                        let id = parts.next().unwrap_or("");
                                        if id.is_empty() {
                                            app.push_block(DotColor::Red, "用法: /session show <id>");
                                        } else {
                                            match store.list_messages(id).await {
                                                Ok(messages) => {
                                                    app.output.clear();
                                                    app.stream_buffer.clear();
                                                    app.streaming = false;
                                                    app.scroll = 0;
                                                    app.stick_to_bottom = true;
                                                    let mut lines = Vec::new();
                                                    if id == app.session_id {
                                                        lines.push(format!("会话 {id} 内容:"));
                                                    } else {
                                                        lines.push(format!(
                                                            "会话 {id} 内容（当前会话: {}）:",
                                                            app.session_id
                                                        ));
                                                    }
                                                    for message in messages {
                                                        let content = one_line(&message.content);
                                                        lines.push(format!(
                                                            "  [{}] {}",
                                                            message.role.to_string(),
                                                            content
                                                        ));
                                                    }
                                                    app.push_block(DotColor::White, &lines.join("\n"));
                                                }
                                                Err(err) => {
                                                    app.push_block(DotColor::Red, &format!("读取消息失败: {err}"));
                                                }
                                            }
                                        }
                                    }
                                    _ => {
                                        app.push_block(
                                            DotColor::Red,
                                            "用法: /session list|new [title]|show <id>",
                                        );
                                    }
                                }
                            }
                            "compact" => {
                                app.push_block(DotColor::White, "开始压缩上下文...");
                                app.status = Status::Thinking;
                                app.blink_on = true;
                                app.last_blink = Instant::now();
                                let provider_factory = provider_factory.clone();
                                let settings = settings.clone();
                                let store = store.clone();
                                let tools = tools.clone();
                                let cwd = cwd.clone();
                                let hooks = hooks.clone();
                                let model = app.model.clone();
                                let session_id = app.session_id.clone();
                                let interaction = interaction.clone();
                                let tool_approvals = tool_approvals.clone();
                                let tx_clone = tx.clone();
                                tokio::spawn(async move {
                                    let result = (|| async {
                                        let provider = (provider_factory)()?;
                                        let agent = Agent::new(
                                            provider,
                                            model,
                                            settings,
                                            store,
                                            tools,
                                            cwd,
                                            hooks,
                                            Some(interaction),
                                            tool_approvals,
                                        );
                                        agent.compact_now(&session_id).await
                                    })()
                                    .await;
                                    match result {
                                        Ok(()) => {
                                            let _ = tx_clone.send(AgentEvent::AssistantMessage {
                                                content: "上下文压缩完成".to_string(),
                                            });
                                            let _ = tx_clone.send(AgentEvent::Done);
                                        }
                                        Err(err) => {
                                            let _ = tx_clone.send(AgentEvent::Error {
                                                message: format!("压缩失败: {err}"),
                                            });
                                        }
                                    }
                                });
                            }
                            _ => {}
                        }

                        app.input.clear();
                        app.cursor = 0;
                        app.refresh_slash(slash);
                        return Ok(true);
                    }

                    app.push_line(user_input_line(&raw_input));
                    app.input.clear();
                    app.cursor = 0;
                    app.refresh_slash(slash);
                    app.status = Status::Thinking;
                    app.blink_on = true;
                    app.last_blink = Instant::now();

                    let provider = (provider_factory)()?;
                    let agent = Agent::new(
                        provider,
                        app.model.clone(),
                        settings.clone(),
                        store.clone(),
                        tools.clone(),
                        cwd.clone(),
                        hooks.clone(),
                        Some(interaction.clone()),
                        tool_approvals.clone(),
                    );
                    let session_id = app.session_id.clone();
                    let input_clone = raw_input.clone();
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
                        app.refresh_slash(slash);
                        return Ok(true);
                    }
                }
                KeyCode::Delete => {
                    if app.cursor < app.input.chars().count() {
                        let idx = char_to_byte_idx(&app.input, app.cursor);
                        app.input.remove(idx);
                        app.refresh_slash(slash);
                        return Ok(true);
                    }
                }
                KeyCode::Left => {
                    if app.slash_active() && slash_page_prev(app) {
                        return Ok(true);
                    }
                    if app.cursor > 0 {
                        app.cursor -= 1;
                        return Ok(true);
                    }
                }
                KeyCode::Right => {
                    if app.slash_active() && slash_page_next(app) {
                        return Ok(true);
                    }
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
                    if app.slash_active() {
                        if slash_move_selection(app, -1) {
                            return Ok(true);
                        }
                        return Ok(false);
                    }
                    app.stick_to_bottom = false;
                    app.scroll = app.scroll.saturating_sub(1);
                    return Ok(true);
                }
                KeyCode::Down => {
                    if app.slash_active() {
                        if slash_move_selection(app, 1) {
                            return Ok(true);
                        }
                        return Ok(false);
                    }
                    app.stick_to_bottom = false;
                    app.scroll = app.scroll.saturating_add(1);
                    return Ok(true);
                }
                KeyCode::Tab => {
                    if app.slash_active() && !app.slash_matches.is_empty() {
                        let chosen = app.slash_matches[app.slash_selected].name.clone();
                        app.input = format!("/{chosen} ");
                        app.cursor = app.input.chars().count();
                        app.refresh_slash(slash);
                        return Ok(true);
                    }
                }
                KeyCode::Char(ch) => {
                    if !key.modifiers.contains(KeyModifiers::CONTROL) {
                        let idx = char_to_byte_idx(&app.input, app.cursor);
                        app.input.insert(idx, ch);
                        app.cursor += 1;
                        app.refresh_slash(slash);
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

fn handle_ui_request(req: UiRequest, app: &mut App) {
    match req {
        UiRequest::UserInput { request, respond_to } => {
            app.enqueue_overlay(InfoOverlay::UserInput(UserInputOverlay::new(
                request, respond_to,
            )));
        }
        UiRequest::ToolApproval { request, respond_to } => {
            app.enqueue_overlay(InfoOverlay::ToolApproval(ToolApprovalOverlay::new(
                request, respond_to,
            )));
        }
    }
}

fn handle_overlay_key(key: crossterm::event::KeyEvent, app: &mut App) -> bool {
    let Some(overlay) = app.info_overlay.as_mut() else {
        return false;
    };
    match overlay.handle_key(key) {
        OverlayAction::None => false,
        OverlayAction::Updated => true,
        OverlayAction::Complete(_) => {
            app.dismiss_overlay();
            true
        }
    }
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
        AgentEvent::ContextUsage { used, limit } => {
            app.context_used = Some(used);
            app.context_limit = limit;
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
    let used = app
        .context_used
        .map(|v| v.to_string())
        .unwrap_or_else(|| "-".to_string());
    let limit = app
        .context_limit
        .map(|v| v.to_string())
        .unwrap_or_else(|| "-".to_string());
    let percent = match (app.context_used, app.context_limit) {
        (Some(used), Some(limit)) if limit > 0 => format!("{:.1}%", (used as f64 / limit as f64) * 100.0),
        _ => "-".to_string(),
    };
    let commands = app.command_hint();
    let mut parts = vec![
        format!("Session: {}", app.session_id),
        format!("{} / {}", app.provider_id, app.model),
        format!("Tokens: {used}/{limit}/{percent}"),
    ];
    if !commands.is_empty() {
        parts.push(format!("Commands: {commands}"));
    }
    parts.join(" | ")
}

fn parse_slash_command(input: &str) -> (String, String) {
    let trimmed = input.trim();
    let without = trimmed.trim_start_matches('/');
    let mut parts = without.splitn(2, |c: char| c.is_whitespace());
    let command = parts.next().unwrap_or("").trim().to_lowercase();
    let args = parts.next().unwrap_or("").trim().to_string();
    (command, args)
}

fn slash_query(input: &str) -> Option<String> {
    let trimmed = input.trim_start();
    if !trimmed.starts_with('/') {
        return None;
    }
    let rest = &trimmed[1..];
    if rest.chars().any(|c| c.is_whitespace()) {
        return None;
    }
    Some(rest.to_string())
}

fn masked_settings_yaml(settings: &Settings) -> String {
    let mut safe = settings.clone();
    for info in safe.providers.values_mut() {
        if info.api_key.is_some() {
            info.api_key = Some("*****".to_string());
        }
        if info.api_key_env.is_some() {
            info.api_key_env = Some("*****".to_string());
        }
    }
    serde_yaml::to_string(&safe).unwrap_or_else(|_| "配置序列化失败".to_string())
}


fn slash_page_size() -> usize {
    6
}

fn slash_page_info(total: usize, page: usize) -> (usize, usize) {
    if total == 0 {
        return (0, 0);
    }
    let page_size = slash_page_size();
    let pages = (total + page_size - 1) / page_size;
    let page = page.min(pages.saturating_sub(1));
    (page, pages)
}

fn clamp_slash_selection(app: &mut App) {
    if app.slash_matches.is_empty() {
        app.slash_selected = 0;
        app.slash_page = 0;
        return;
    }
    if app.slash_selected >= app.slash_matches.len() {
        app.slash_selected = 0;
    }
    let page_size = slash_page_size();
    app.slash_page = app.slash_selected / page_size;
}

fn slash_move_selection(app: &mut App, delta: isize) -> bool {
    if app.slash_matches.is_empty() {
        return false;
    }
    let page_size = slash_page_size();
    let (page, _pages) = slash_page_info(app.slash_matches.len(), app.slash_page);
    let start = page * page_size;
    let end = (start + page_size).min(app.slash_matches.len());
    let mut idx = app.slash_selected;
    if idx < start || idx >= end {
        idx = start;
    }
    if delta < 0 && idx > start {
        app.slash_selected = idx - 1;
        return true;
    }
    if delta > 0 && idx + 1 < end {
        app.slash_selected = idx + 1;
        return true;
    }
    false
}

fn slash_page_prev(app: &mut App) -> bool {
    if app.slash_matches.is_empty() {
        return false;
    }
    let (_page, pages) = slash_page_info(app.slash_matches.len(), app.slash_page);
    if pages <= 1 || app.slash_page == 0 {
        return false;
    }
    app.slash_page -= 1;
    app.slash_selected = app.slash_page * slash_page_size();
    true
}

fn slash_page_next(app: &mut App) -> bool {
    if app.slash_matches.is_empty() {
        return false;
    }
    let (page, pages) = slash_page_info(app.slash_matches.len(), app.slash_page);
    if pages <= 1 || page + 1 >= pages {
        return false;
    }
    app.slash_page = page + 1;
    app.slash_selected = app.slash_page * slash_page_size();
    true
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

fn format_markdown_lines(text: &str, width: u16) -> Vec<Line<'static>> {
    let mut lines = markdown_to_lines(text, width);
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

fn markdown_to_lines(text: &str, width: u16) -> Vec<Line<'static>> {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TABLES);
    let parser = Parser::new_ext(text, opts);

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current: Vec<Span<'static>> = Vec::new();
    let mut style_stack: Vec<Style> = vec![Style::default()];
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
    let mut code_block_lines: Vec<String> = Vec::new();
    let mut code_block_lang: Option<String> = None;
    let mut in_code_block = false;

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
                Tag::CodeBlock(kind) => {
                    flush_line(&mut lines, &mut current);
                    in_code_block = true;
                    code_block_lines.clear();
                    code_block_lang = match kind {
                        CodeBlockKind::Fenced(info) => info
                            .split_whitespace()
                            .next()
                            .map(|s| s.to_string())
                            .filter(|s| !s.is_empty()),
                        _ => None,
                    };
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
                    if code_block_lines.is_empty() {
                        code_block_lines.push(String::new());
                    }
                    lines.extend(render_code_block_lines(
                        &code_block_lines,
                        code_block_lang.as_deref(),
                        width,
                    ));
                    code_block_lines.clear();
                    code_block_lang = None;
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
                    if code_block_lines.is_empty() {
                        code_block_lines.push(String::new());
                    }
                    for (idx, chunk) in text.split('\n').enumerate() {
                        if idx > 0 {
                            code_block_lines.push(String::new());
                        }
                        if let Some(last) = code_block_lines.last_mut() {
                            last.push_str(chunk);
                        }
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
                if in_code_block {
                    if code_block_lines.is_empty() {
                        code_block_lines.push(String::new());
                    }
                    if let Some(last) = code_block_lines.last_mut() {
                        last.push_str(&code);
                    }
                    continue;
                } else if in_table_cell {
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
                if in_code_block {
                    code_block_lines.push(String::new());
                } else if in_table_cell {
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

fn render_code_block_lines(
    lines: &[String],
    lang: Option<&str>,
    width: u16,
) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let mut content_width = 1usize;
    for line in lines {
        content_width = content_width.max(UnicodeWidthStr::width(line.as_str()));
    }
    let label = lang
        .map(|l| format!(" {l} "))
        .filter(|s| !s.trim().is_empty());
    let available_width = width.saturating_sub(2) as usize;
    let outer_width = if available_width > 0 {
        available_width.max(6)
    } else {
        0
    };
    let inner_width = if outer_width > 2 {
        outer_width - 2
    } else {
        content_width.saturating_add(2)
    };
    let content_width = if inner_width > 2 {
        inner_width - 2
    } else {
        content_width
    };

    let border_style = Style::default().fg(Color::DarkGray);
    let (syntax_set, theme) = syntect_assets();
    let syntax = lang
        .map(|l| l.to_lowercase())
        .and_then(|l| syntax_set.find_syntax_by_token(&l))
        .unwrap_or_else(|| syntax_set.find_syntax_plain_text());
    let mut highlighter = HighlightLines::new(syntax, theme);
    if let Some(label_text) = label {
        let mut label_text = label_text;
        let mut label_width = UnicodeWidthStr::width(label_text.as_str());
        if label_width > inner_width {
            label_text = truncate_to_width(&label_text, inner_width);
            label_width = UnicodeWidthStr::width(label_text.as_str());
        }
        let dash_count = inner_width.saturating_sub(label_width);
        let mut spans = Vec::new();
        spans.push(Span::styled("╭", border_style));
        spans.push(Span::styled("─".repeat(dash_count), border_style));
        spans.push(Span::raw(label_text));
        spans.push(Span::styled("╮", border_style));
        out.push(Line::from(spans));
    } else {
        out.push(Line::from(Span::styled(
            format!("╭{}╮", "─".repeat(inner_width)),
            border_style,
        )));
    }

    for line in lines {
        let trimmed = truncate_to_width(line, content_width);
        let trimmed_width = UnicodeWidthStr::width(trimmed.as_str());
        let pad = content_width.saturating_sub(trimmed_width);
        let regions = highlighter
            .highlight_line(&trimmed, syntax_set)
            .unwrap_or_default();
        let mut spans = Vec::new();
        spans.push(Span::styled("│", border_style));
        spans.push(Span::raw(" "));
        if regions.is_empty() {
            spans.push(Span::raw(trimmed.clone()));
        } else {
            for (style, text) in regions {
                let mut span_style =
                    Style::default().fg(Color::Rgb(style.foreground.r, style.foreground.g, style.foreground.b));
                if style.font_style.contains(FontStyle::BOLD) {
                    span_style = span_style.add_modifier(Modifier::BOLD);
                }
                if style.font_style.contains(FontStyle::ITALIC) {
                    span_style = span_style.add_modifier(Modifier::ITALIC);
                }
                if style.font_style.contains(FontStyle::UNDERLINE) {
                    span_style = span_style.add_modifier(Modifier::UNDERLINED);
                }
                spans.push(Span::styled(text.to_string(), span_style));
            }
        }
        spans.push(Span::raw(" ".repeat(pad)));
        spans.push(Span::raw(" "));
        spans.push(Span::styled("│", border_style));
        out.push(Line::from(spans));
    }

    out.push(Line::from(Span::styled(
        format!("╰{}╯", "─".repeat(inner_width)),
        border_style,
    )));
    out
}

fn syntect_assets() -> (&'static SyntaxSet, &'static Theme) {
    static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();
    static THEME_SET: OnceLock<ThemeSet> = OnceLock::new();
    static THEME_NAME: &str = "base16-ocean.dark";
    let syntax_set = SYNTAX_SET.get_or_init(SyntaxSet::load_defaults_newlines);
    let theme_set = THEME_SET.get_or_init(ThemeSet::load_defaults);
    let theme = theme_set
        .themes
        .get(THEME_NAME)
        .or_else(|| theme_set.themes.values().next())
        .expect("syntect theme set is empty");
    (syntax_set, theme)
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
