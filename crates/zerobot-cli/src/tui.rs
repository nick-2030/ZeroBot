use crate::slash::{SlashMatch, SlashRegistry};
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
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
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
    InteractionHandler, ToolApprovalDecision, ToolApprovalRequest, ToolApprovalResponse,
    UserInputAnswer, UserInputOption, UserInputQuestion, UserInputRequest, UserInputResponse,
};
use zerobot_core::plugin::PluginManager;
use zerobot_core::provider::{ProviderFactory, TokenUsage};
use zerobot_core::session::{
    create_session_with_hooks, MessageRole, Session, SessionKind, SessionStore, TodoItem,
    TodoStatus,
};
use zerobot_core::tool::ToolRegistry;
use zerobot_core::ZeroBotError;
use zerobot_core::{discover_template_commands, init_prompt, render_template_prompt};

#[derive(Copy, Clone)]
enum DotColor {
    White,
    Green,
    Yellow,
    Red,
}

const COLOR_PANEL_BG: Color = Color::Rgb(32, 36, 44);
const COLOR_PANEL_BORDER: Color = Color::Rgb(70, 76, 88);
const COLOR_TEXT: Color = Color::Rgb(220, 224, 232);
const COLOR_MUTED: Color = Color::Rgb(136, 142, 156);
const COLOR_ACCENT: Color = Color::Rgb(186, 148, 255);
const COLOR_ACCENT_DIM: Color = Color::Rgb(132, 112, 190);
const COLOR_SELECTED_BG: Color = Color::Rgb(48, 52, 64);
const COLOR_SUCCESS: Color = Color::Rgb(124, 216, 168);
const COLOR_ERROR: Color = Color::Rgb(236, 112, 104);
const COLOR_WARN: Color = Color::Rgb(234, 196, 118);
const LOGO_COLOR: Color = COLOR_ACCENT;
const BORDER_COLOR: Color = COLOR_PANEL_BORDER;
const DOUBLE_PRESS_WINDOW_MS: u64 = 900;

#[derive(Clone)]
enum Status {
    Idle,
    Thinking,
    Tool(String),
    Hook(String),
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
    ResumeSelected {
        session_id: String,
    },
    RewindSelected {
        message_id: String,
        input: String,
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
    output: Vec<OutputItem>,
    stream_buffer: String,
    streaming: bool,
    last_tool_label: Option<String>,
    input: String,
    cursor: usize,
    scroll: u16,
    stick_to_bottom: bool,
    todos: Vec<TodoItem>,
    usage: Option<TokenUsage>,
    context_used: Option<usize>,
    context_limit: Option<u32>,
    session_input_tokens: u64,
    session_output_tokens: u64,
    session_cache_creation_tokens: u64,
    session_cache_read_tokens: u64,
    session_turn_count: u32,
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
    show_full_tool_output: bool,
    running_tool_output_idx: Option<usize>,
    running_hook_output_idx: Option<usize>,
    active_hooks: Vec<String>,
    last_idle_esc: Option<Instant>,
    last_idle_ctrl_c: Option<Instant>,
    status_notice: Option<String>,
}

#[derive(Clone)]
enum OutputItem {
    Lines(Vec<Line<'static>>),
    Block {
        color: DotColor,
        text: String,
    },
    Markdown(String),
    ToolRunning {
        label: String,
    },
    ToolOutput {
        color: DotColor,
        tool_name: String,
        label: Option<String>,
        output: String,
    },
    HookRunning {
        label: String,
    },
    HookOutput {
        ok: bool,
        label: String,
    },
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
            usage: None,
            context_used: None,
            context_limit: None,
            session_input_tokens: 0,
            session_output_tokens: 0,
            session_cache_creation_tokens: 0,
            session_cache_read_tokens: 0,
            session_turn_count: 0,
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
            show_full_tool_output: false,
            running_tool_output_idx: None,
            running_hook_output_idx: None,
            active_hooks: Vec::new(),
            last_idle_esc: None,
            last_idle_ctrl_c: None,
            status_notice: None,
        }
    }

    fn push_line(&mut self, line: Line<'static>) {
        self.output.push(OutputItem::Lines(vec![line]));
        self.stick_to_bottom = true;
    }

    fn push_lines(&mut self, lines: Vec<Line<'static>>) {
        if lines.is_empty() {
            return;
        }
        self.output.push(OutputItem::Lines(lines));
        self.stick_to_bottom = true;
    }

    fn push_block(&mut self, color: DotColor, text: &str) {
        self.output.push(OutputItem::Block {
            color,
            text: text.to_string(),
        });
        self.stick_to_bottom = true;
    }

    fn push_markdown_block(&mut self, text: &str) {
        self.output.push(OutputItem::Markdown(text.to_string()));
        self.stick_to_bottom = true;
    }

    fn push_tool_output(
        &mut self,
        color: DotColor,
        tool_name: &str,
        label: Option<&str>,
        output: &str,
    ) {
        self.output.push(OutputItem::ToolOutput {
            color,
            tool_name: tool_name.to_string(),
            label: label.map(|s| s.to_string()),
            output: output.to_string(),
        });
        self.stick_to_bottom = true;
    }

    fn push_running_hook(&mut self, label: &str) {
        self.output.push(OutputItem::HookRunning {
            label: label.to_string(),
        });
        self.running_hook_output_idx = Some(self.output.len().saturating_sub(1));
        self.stick_to_bottom = true;
    }

    fn complete_running_hook(&mut self, ok: bool, label: &str) {
        let item = OutputItem::HookOutput {
            ok,
            label: label.to_string(),
        };
        if let Some(idx) = self.running_hook_output_idx.take() {
            if idx < self.output.len() {
                self.output[idx] = item;
                self.stick_to_bottom = true;
                return;
            }
        }
        self.output.push(item);
        self.stick_to_bottom = true;
    }

    fn push_running_tool(&mut self, label: &str) {
        self.output.push(OutputItem::ToolRunning {
            label: label.to_string(),
        });
        self.running_tool_output_idx = Some(self.output.len().saturating_sub(1));
        self.stick_to_bottom = true;
    }

    fn complete_running_tool(&mut self, name: &str, output: &str, ok: bool) {
        let color = if ok { DotColor::Green } else { DotColor::Red };
        let label = self.last_tool_label.clone();
        let item = OutputItem::ToolOutput {
            color,
            tool_name: name.to_string(),
            label: label.clone(),
            output: output.to_string(),
        };
        if let Some(idx) = self.running_tool_output_idx.take() {
            if idx < self.output.len() {
                self.output[idx] = item;
                self.stick_to_bottom = true;
                return;
            }
        }
        self.push_tool_output(color, name, label.as_deref(), output);
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
        self.stick_to_bottom = true;
    }

    fn finalize_stream(&mut self) {
        if !self.streaming {
            return;
        }
        let content = self.stream_buffer.clone();
        self.output.push(OutputItem::Markdown(content.clone()));
        if !content.trim().is_empty() {
            self.last_copyable_output = Some(content);
        }
        self.stream_buffer.clear();
        self.streaming = false;
        self.stick_to_bottom = true;
    }

    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut out = Vec::new();
        for item in &self.output {
            let mut lines = match item {
                OutputItem::Lines(lines) => lines.clone(),
                OutputItem::Block { color, text } => format_block_lines(*color, text),
                OutputItem::Markdown(text) => format_markdown_lines(text, width),
                OutputItem::ToolRunning { label } => {
                    vec![format_running_tool_line(label, self.blink_on)]
                }
                OutputItem::ToolOutput {
                    color,
                    tool_name,
                    label,
                    output,
                } => {
                    let always_full = is_full_output_tool(tool_name);
                    format_tool_output_lines(
                        *color,
                        label.as_deref(),
                        output,
                        width,
                        self.show_full_tool_output || always_full,
                        always_full,
                    )
                }
                OutputItem::HookRunning { label } => {
                    vec![format_running_hook_line(label, self.blink_on)]
                }
                OutputItem::HookOutput { ok, label } => {
                    vec![format_hook_output_line(*ok, label)]
                }
            };
            if lines.is_empty() {
                continue;
            }
            if !out.is_empty() {
                out.push(Line::from(Span::raw("")));
            }
            out.append(&mut lines);
        }
        if self.streaming {
            if !out.is_empty() {
                out.push(Line::from(Span::raw("")));
            }
            out.extend(format_block_lines(DotColor::White, &self.stream_buffer));
        }
        out
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
            Status::Idle => {
                Line::from(Span::styled("状态: 空闲", Style::default().fg(COLOR_MUTED)))
            }
            Status::Thinking => {
                let dot = if self.blink_on { "●" } else { " " };
                Line::from(Span::styled(
                    format!("状态: {dot} 努力工作中"),
                    Style::default().fg(COLOR_ACCENT),
                ))
            }
            Status::Tool(name) => Line::from(Span::styled(
                format!("状态: 工具执行中: {name}"),
                Style::default().fg(COLOR_ACCENT),
            )),
            Status::Hook(name) => Line::from(Span::styled(
                format!("状态: Hook 执行中: {name}"),
                Style::default().fg(COLOR_WARN),
            )),
            Status::Error(message) => Line::from(Span::styled(
                format!("状态: 错误: {message}"),
                Style::default().fg(COLOR_ERROR),
            )),
            Status::WaitingUserInput => Line::from(Span::styled(
                "状态: 等待用户输入",
                Style::default().fg(COLOR_WARN),
            )),
            Status::WaitingApproval => Line::from(Span::styled(
                "状态: 等待授权",
                Style::default().fg(COLOR_WARN),
            )),
        };
        lines.push(status_line);
        if !self.active_hooks.is_empty() {
            let hook_count = self.active_hooks.len();
            let label = if hook_count == 1 {
                format!("Hooks: {} running", self.active_hooks[0])
            } else {
                format!("Hooks: {} active", hook_count)
            };
            lines.push(Line::from(Span::styled(
                label,
                Style::default().fg(COLOR_WARN),
            )));
        }
        if !self.todos.is_empty() {
            let total = self.todos.len();
            let done = self
                .todos
                .iter()
                .filter(|t| {
                    matches!(t.status, TodoStatus::Completed | TodoStatus::Cancelled)
                })
                .count();
            let in_progress = self
                .todos
                .iter()
                .filter(|t| matches!(t.status, TodoStatus::InProgress))
                .count();
            let pending = total - done - in_progress;
            let header = if done == total {
                format!("Tasks: {done}/{total} done")
            } else {
                format!(
                    "Tasks: {done}/{total} done, {in_progress} active, {pending} pending"
                )
            };
            lines.push(Line::from(Span::styled(
                header,
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .fg(COLOR_ACCENT),
            )));
            // Show up to 8 items: in_progress first, then pending, then recently completed
            let max_display: usize = 8;
            let mut display_items: Vec<&TodoItem> = Vec::new();
            // In-progress items first
            display_items.extend(
                self.todos
                    .iter()
                    .filter(|t| matches!(t.status, TodoStatus::InProgress)),
            );
            // Then pending
            display_items.extend(
                self.todos
                    .iter()
                    .filter(|t| matches!(t.status, TodoStatus::Pending)),
            );
            // Then completed/cancelled (up to remaining slots)
            let remaining = max_display.saturating_sub(display_items.len());
            display_items.extend(
                self.todos
                    .iter()
                    .filter(|t| {
                        matches!(t.status, TodoStatus::Completed | TodoStatus::Cancelled)
                    })
                    .take(remaining),
            );
            let hidden = total.saturating_sub(display_items.len());
            for item in display_items {
                let (icon, style) = match item.status {
                    TodoStatus::InProgress => (
                        "\u{25A0}", // ■
                        Style::default()
                            .fg(COLOR_ACCENT)
                            .add_modifier(Modifier::BOLD),
                    ),
                    TodoStatus::Pending => {
                        ("\u{25A1}", Style::default().fg(COLOR_TEXT)) // □
                    }
                    TodoStatus::Completed => (
                        "\u{2713}", // ✓
                        Style::default()
                            .fg(COLOR_SUCCESS)
                            .add_modifier(Modifier::CROSSED_OUT)
                            .add_modifier(Modifier::DIM),
                    ),
                    TodoStatus::Cancelled => (
                        "\u{2717}", // ✗
                        Style::default()
                            .fg(COLOR_MUTED)
                            .add_modifier(Modifier::CROSSED_OUT)
                            .add_modifier(Modifier::DIM),
                    ),
                };
                let display_text = if matches!(item.status, TodoStatus::InProgress) {
                    item.active_form.as_deref().unwrap_or(&item.content)
                } else {
                    &item.content
                };
                lines.push(Line::from(vec![
                    Span::styled(format!("  {icon} "), style),
                    Span::styled(display_text.to_string(), style),
                ]));
            }
            if hidden > 0 {
                lines.push(Line::from(Span::styled(
                    format!("  ... +{hidden} more"),
                    Style::default().fg(COLOR_MUTED),
                )));
            }
        }
        if self.slash_active() {
            lines.push(Line::from(Span::styled(
                "Commands:",
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .fg(COLOR_ACCENT),
            )));
            if self.slash_matches.is_empty() {
                lines.push(Line::from(Span::styled(
                    "  （无匹配）",
                    Style::default().fg(COLOR_MUTED),
                )));
            } else {
                let page_size = slash_page_size();
                let (page, pages) = slash_page_info(self.slash_matches.len(), self.slash_page);
                let start = page * page_size;
                let end = (start + page_size).min(self.slash_matches.len());
                for (idx, cmd) in self.slash_matches[start..end].iter().enumerate() {
                    let absolute_idx = start + idx;
                    let selected = absolute_idx == self.slash_selected;
                    let prefix = if selected { "▸ " } else { "  " };
                    let mut spans = Vec::new();
                    let style = if selected {
                        Style::default().fg(COLOR_TEXT).bg(COLOR_SELECTED_BG)
                    } else {
                        Style::default().fg(COLOR_MUTED)
                    };
                    spans.push(Span::styled(prefix, style));
                    spans.push(Span::styled(
                        format!("/{}", cmd.name),
                        style.fg(COLOR_ACCENT),
                    ));
                    spans.push(Span::styled("  ", style));
                    spans.push(Span::styled(cmd.description.clone(), style));
                    lines.push(Line::from(spans));
                }
                if pages > 1 {
                    lines.push(Line::from(Span::styled(
                        format!("  Page {}/{}", page + 1, pages),
                        Style::default().fg(COLOR_ACCENT_DIM),
                    )));
                }
            }
            lines.push(Line::from(Span::styled(
                "↑/↓ 选择  ←/→ 翻页  Enter/Tab 补全",
                Style::default().fg(COLOR_MUTED),
            )));
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

    fn clear_key_arms(&mut self) {
        self.last_idle_esc = None;
        self.last_idle_ctrl_c = None;
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
        let note = if note.trim().is_empty() {
            None
        } else {
            Some(note)
        };
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
            Style::default()
                .add_modifier(Modifier::BOLD)
                .fg(COLOR_ACCENT),
        )));
        if self.request.questions.len() > 1 {
            let mut spans = Vec::new();
            spans.push(Span::styled("问题: ", Style::default().fg(COLOR_MUTED)));
            for (idx, q) in self.request.questions.iter().enumerate() {
                let label = truncate_chars(&q.prompt, 12);
                let text = format!("{}{} ", idx + 1, label);
                if idx == self.current {
                    spans.push(Span::styled(
                        text,
                        Style::default().add_modifier(Modifier::BOLD).fg(COLOR_TEXT),
                    ));
                } else {
                    spans.push(Span::styled(text, Style::default().fg(COLOR_MUTED)));
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
                    let selected = idx == self.selected && self.focus == UserInputFocus::Options;
                    let prefix = if selected { "▸ " } else { "  " };
                    let style = if selected {
                        Style::default().fg(COLOR_TEXT).bg(COLOR_SELECTED_BG)
                    } else {
                        Style::default().fg(COLOR_MUTED)
                    };
                    lines.push(Line::from(Span::styled(
                        format!("{prefix}{}", opt.label),
                        style,
                    )));
                }
            } else {
                lines.push(Line::from(Span::styled(
                    "（无选项，直接输入）",
                    Style::default().fg(COLOR_MUTED),
                )));
            }
            let note = self.current_note();
            let selected = self.focus == UserInputFocus::Input;
            let prefix = if selected { "▸ " } else { "  " };
            let style = if selected {
                Style::default().fg(COLOR_TEXT).bg(COLOR_SELECTED_BG)
            } else {
                Style::default().fg(COLOR_MUTED)
            };
            lines.push(Line::from(Span::styled(
                format!("{prefix}输入内容: {note}"),
                style,
            )));
        }
        lines.push(Line::from(Span::styled(
            "↑/↓ 选择  ←/→ 切换  Tab 切换输入  Enter 下一项/提交  Esc 取消",
            Style::default().fg(COLOR_MUTED),
        )));
        lines
    }
}

impl ToolApprovalOverlay {
    fn new(
        request: ToolApprovalRequest,
        respond_to: oneshot::Sender<ToolApprovalResponse>,
    ) -> Self {
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
                if self.selected + 1 < 4 {
                    self.selected += 1;
                    return OverlayAction::Updated;
                }
            }
            KeyCode::Enter => {
                let decision = match self.selected {
                    0 => ToolApprovalDecision::AllowOnce,
                    1 => ToolApprovalDecision::AllowSession,
                    2 => ToolApprovalDecision::AllowWorkspace,
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
            Style::default()
                .add_modifier(Modifier::BOLD)
                .fg(COLOR_ACCENT),
        )));
        lines.push(Line::from(Span::styled(
            format!("工具: {}", self.request.tool_name),
            Style::default().fg(COLOR_TEXT),
        )));
        if let Some(reason) = &self.request.reason {
            if !reason.trim().is_empty() {
                lines.push(Line::from(Span::styled(
                    format!("原因: {reason}"),
                    Style::default().fg(COLOR_MUTED),
                )));
            }
        }
        if let Ok(args) = serde_json::to_string(&self.request.arguments) {
            let args = one_line(&args);
            if !args.is_empty() {
                lines.push(Line::from(Span::styled(
                    format!("参数: {args}"),
                    Style::default().fg(COLOR_MUTED),
                )));
            }
        }
        let options = ["仅本次允许", "本会话允许", "本工作区允许", "拒绝"];
        for (idx, opt) in options.iter().enumerate() {
            let selected = idx == self.selected;
            let prefix = if selected { "▸ " } else { "  " };
            let style = if selected {
                Style::default().fg(COLOR_TEXT).bg(COLOR_SELECTED_BG)
            } else {
                Style::default().fg(COLOR_MUTED)
            };
            lines.push(Line::from(Span::styled(format!("{prefix}{opt}"), style)));
        }
        lines.push(Line::from(Span::styled(
            "↑/↓ 选择  Enter 确认  Esc 取消",
            Style::default().fg(COLOR_MUTED),
        )));
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
    resume: bool,
    use_alt_screen: bool,
    provider_state: Arc<StdRwLock<String>>,
    plugins: Option<Arc<PluginManager>>,
    tool_approvals: Arc<TokioRwLock<HashSet<String>>>,
) -> Result<String> {
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
        resume,
        provider_state,
        plugins,
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
    resume: bool,
    provider_state: Arc<StdRwLock<String>>,
    plugins: Option<Arc<PluginManager>>,
    tool_approvals: Arc<TokioRwLock<HashSet<String>>>,
) -> Result<String> {
    let plugin_assets = plugins
        .as_ref()
        .map(|manager| manager.asset_roots())
        .unwrap_or_default();
    let (dynamic_commands, slash_warning) =
        match discover_template_commands(&settings, &cwd, &plugin_assets) {
            Ok(list) => (list, None),
            Err(err) => (
                Vec::new(),
                Some(format!("加载动态斜杠命令失败，已降级为内置命令: {err}")),
            ),
        };
    let slash = SlashRegistry::extended(dynamic_commands);
    let mut app = App::new(session_id.clone(), provider_id.clone(), model.clone());
    if let Some(message) = slash_warning {
        app.push_block(DotColor::Yellow, &message);
    }
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
    app.push_lines(welcome);
    if resume {
        resume_session(&mut app, &store, &session_id).await;
    } else {
        refresh_session_state(&mut app, &store).await;
    }

    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
    hooks.set_event_sender(tx.clone());
    let (ui_tx, mut ui_rx) = mpsc::unbounded_channel::<UiRequest>();
    let interaction: Arc<dyn InteractionHandler> =
        Arc::new(UiInteractionHandler { tx: ui_tx.clone() });
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
                            &plugins,
                            &tool_approvals,
                            &ui_tx,
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
                    handle_ui_request(req, &mut app, &store, &slash).await;
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
                            &plugins,
                            &tool_approvals,
                            &ui_tx,
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
                    handle_ui_request(req, &mut app, &store, &slash).await;
                    dirty = true;
                }
            }
        }
    }

    Ok(app.session_id.clone())
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
    plugins: &Option<Arc<PluginManager>>,
    tool_approvals: &Arc<TokioRwLock<HashSet<String>>>,
    ui_tx: &mpsc::UnboundedSender<UiRequest>,
    tx: &mpsc::UnboundedSender<AgentEvent>,
    should_quit: &mut bool,
) -> Result<bool> {
    fn is_busy(
        app: &App,
        runner: &Option<tokio::task::JoinHandle<zerobot_core::error::ZeroBotResult<String>>>,
    ) -> bool {
        if runner.is_some() {
            return true;
        }
        matches!(
            app.status,
            Status::Thinking | Status::Tool(_) | Status::WaitingApproval | Status::WaitingUserInput
        )
    }

    fn interrupt_active_turn(
        app: &mut App,
        runner: &mut Option<tokio::task::JoinHandle<zerobot_core::error::ZeroBotResult<String>>>,
    ) -> bool {
        let mut interrupted = false;
        if let Some(handle) = runner.take() {
            handle.abort();
            interrupted = true;
        }
        app.finalize_stream();
        if let Some(idx) = app.running_tool_output_idx.take() {
            if idx < app.output.len() {
                if let OutputItem::ToolRunning { label } = app.output[idx].clone() {
                    app.output[idx] = OutputItem::ToolOutput {
                        color: DotColor::Yellow,
                        tool_name: "interrupted".to_string(),
                        label: Some(label),
                        output: "已中断".to_string(),
                    };
                }
            }
        }
        if let Some(overlay) = app.info_overlay.as_mut() {
            match overlay {
                InfoOverlay::UserInput(overlay) => {
                    overlay.finish(true);
                }
                InfoOverlay::ToolApproval(overlay) => {
                    overlay.finish(ToolApprovalDecision::Deny);
                }
            }
            app.dismiss_overlay();
        }
        if interrupted || !matches!(app.status, Status::Idle | Status::Error(_)) {
            app.last_tool_label = None;
            app.status = Status::Idle;
            app.push_block(DotColor::Yellow, "已中断当前执行");
            app.clear_key_arms();
            app.status_notice = None;
            return true;
        }
        false
    }

    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => {
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('o') {
                app.show_full_tool_output = !app.show_full_tool_output;
                return Ok(true);
            }
            let now = Instant::now();
            let double_window = Duration::from_millis(DOUBLE_PRESS_WINDOW_MS);
            let ctrl_c =
                key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c');
            let esc = key.code == KeyCode::Esc;
            let busy = is_busy(app, runner);
            let idle = !busy && app.info_overlay.is_none() && matches!(app.status, Status::Idle);

            if ctrl_c {
                if busy {
                    return Ok(interrupt_active_turn(app, runner));
                }
                if idle {
                    if let Some(prev) = app.last_idle_ctrl_c {
                        if now.duration_since(prev) <= double_window {
                            app.clear_key_arms();
                            app.status_notice = None;
                            *should_quit = true;
                            return Ok(true);
                        }
                    }
                    app.last_idle_ctrl_c = Some(now);
                    app.last_idle_esc = None;
                    app.status_notice = Some("再按一次ctrl+c退出程序".to_string());
                    return Ok(true);
                }
                app.clear_key_arms();
                app.status_notice = None;
            } else if esc {
                if busy {
                    return Ok(interrupt_active_turn(app, runner));
                }
                if idle {
                    if let Some(prev) = app.last_idle_esc {
                        if now.duration_since(prev) <= double_window {
                            app.clear_key_arms();
                            app.status_notice = None;

                            let messages = match store.list_messages(&app.session_id).await {
                                Ok(messages) => messages,
                                Err(err) => {
                                    app.push_block(
                                        DotColor::Red,
                                        &format!("读取会话消息失败: {err}"),
                                    );
                                    return Ok(true);
                                }
                            };
                            let mut options = Vec::new();
                            let mut input_by_id = HashMap::<String, String>::new();
                            let mut index = 1usize;
                            for message in messages
                                .into_iter()
                                .filter(|msg| matches!(msg.role, MessageRole::User))
                            {
                                let summary = one_line(&message.content);
                                let summary = truncate_chars(&summary, 80);
                                options.push(UserInputOption {
                                    id: message.id.clone(),
                                    label: format!("{index:>3}. {summary}"),
                                });
                                input_by_id.insert(message.id.clone(), message.content.clone());
                                index += 1;
                            }
                            if options.is_empty() {
                                app.push_block(DotColor::White, "当前会话还没有可回退的用户输入");
                                return Ok(true);
                            }

                            let request = UserInputRequest {
                                id: "rewind".to_string(),
                                title: Some("选择要回退的用户输入".to_string()),
                                questions: vec![UserInputQuestion {
                                    id: "message".to_string(),
                                    prompt: "请选择一条用户输入（将回退到该消息之前）".to_string(),
                                    options: Some(options),
                                }],
                            };
                            let (resp_tx, resp_rx) = oneshot::channel();
                            if ui_tx
                                .send(UiRequest::UserInput {
                                    request,
                                    respond_to: resp_tx,
                                })
                                .is_err()
                            {
                                app.push_block(DotColor::Red, "无法打开回退选择列表");
                                return Ok(true);
                            }
                            let ui_tx = ui_tx.clone();
                            tokio::spawn(async move {
                                if let Ok(resp) = resp_rx.await {
                                    if resp.cancelled {
                                        return;
                                    }
                                    let selected = resp
                                        .answers
                                        .get("message")
                                        .and_then(|ans| ans.option_id.clone());
                                    if let Some(message_id) = selected {
                                        if let Some(input) = input_by_id.get(&message_id).cloned() {
                                            let _ = ui_tx.send(UiRequest::RewindSelected {
                                                message_id,
                                                input,
                                            });
                                        }
                                    }
                                }
                            });
                            return Ok(true);
                        }
                    }
                    app.last_idle_esc = Some(now);
                    app.last_idle_ctrl_c = None;
                    app.status_notice = None;
                    return Ok(true);
                }
                app.clear_key_arms();
                app.status_notice = None;
            } else {
                app.clear_key_arms();
                app.status_notice = None;
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
                        let Some(spec) = slash.find(&command).cloned() else {
                            app.push_block(
                                DotColor::Red,
                                &format!("未知命令: /{command}（输入 /help 查看）"),
                            );
                            return Ok(true);
                        };
                        if spec.is_builtin("clear") {
                            app.output.clear();
                            app.stream_buffer.clear();
                            app.streaming = false;
                            app.last_tool_label = None;
                            app.running_tool_output_idx = None;
                            app.scroll = 0;
                            app.stick_to_bottom = true;
                            app.input.clear();
                            app.cursor = 0;
                            app.refresh_slash(slash);
                            return Ok(true);
                        }
                        if spec.is_builtin("exit") {
                            app.push_line(user_input_line(&raw_input));
                            *should_quit = true;
                            return Ok(true);
                        }

                        app.push_line(user_input_line(&raw_input));

                        if let Some(template_command) = spec.template().cloned() {
                            let prompt = match render_template_prompt(&template_command, &args, cwd)
                                .await
                            {
                                Ok(prompt) => prompt,
                                Err(err) => {
                                    app.push_block(DotColor::Red, &format!("命令执行失败: {err}"));
                                    app.input.clear();
                                    app.cursor = 0;
                                    app.refresh_slash(slash);
                                    return Ok(true);
                                }
                            };

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
                                plugins.clone(),
                                tool_approvals.clone(),
                                None,
                                None,
                            );
                            let session_id = app.session_id.clone();
                            let tx_clone = tx.clone();
                            *runner = Some(tokio::spawn(async move {
                                agent.run_turn(&session_id, &prompt, Some(tx_clone)).await
                            }));
                            return Ok(true);
                        }

                        match spec.name.as_str() {
                            "help" => {
                                let target = args.split_whitespace().next().unwrap_or("");
                                let message = if target.is_empty() {
                                    let mut lines = Vec::new();
                                    lines.push("可用命令:".to_string());
                                    for cmd in slash.commands() {
                                        lines.push(format!(
                                            "  {} [{}] - {}",
                                            cmd.usage,
                                            cmd.source_tag(),
                                            cmd.description
                                        ));
                                    }
                                    lines.push("输入 /help <命令> 查看用法".to_string());
                                    lines.join("\n")
                                } else if let Some(cmd) = slash.find(target) {
                                    format!(
                                        "{} [{}] - {}",
                                        cmd.usage,
                                        cmd.source_tag(),
                                        cmd.description
                                    )
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
                                let message =
                                    format!("启用工具: {enabled}\n已注册工具: {registered}");
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
                                    plugins.clone(),
                                    tool_approvals.clone(),
                                    None,
                                    None,
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
                                            .map(|(id, info)| {
                                                (id.clone(), info.kind.clone(), info.model.clone())
                                            })
                                            .collect::<Vec<_>>();
                                        items.sort_by(|a, b| a.0.cmp(&b.0));
                                        for (id, kind, model) in items {
                                            let suffix =
                                                if id == app.provider_id { " *" } else { "" };
                                            let model = model
                                                .map(|m| format!(", model={m}"))
                                                .unwrap_or_default();
                                            lines.push(format!("  {id} ({kind}{model}){suffix}"));
                                        }
                                    }
                                    app.push_block(DotColor::White, &lines.join("\n"));
                                } else {
                                    let target = args_lower;
                                    let exists = settings.providers.contains_key(&target)
                                        || matches!(target.as_str(), "openai" | "anthropic");
                                    if !exists {
                                        app.push_block(
                                            DotColor::Red,
                                            "未知提供商（输入 /provider list 查看）",
                                        );
                                    } else {
                                        app.provider_id = target.clone();
                                        if let Ok(mut guard) = provider_state.write() {
                                            *guard = target.clone();
                                        }
                                        if let Some(info) = settings.providers.get(&app.provider_id)
                                        {
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
                                    "list" => match store.list_sessions().await {
                                        Ok(sessions) => {
                                            let mut lines = Vec::new();
                                            lines.push("会话列表:".to_string());
                                            for session in sessions {
                                                let summary =
                                                    session_summary_for_display(store, &session)
                                                        .await;
                                                lines.push(format_session_line(&session, &summary));
                                            }
                                            app.push_block(DotColor::White, &lines.join("\n"));
                                        }
                                        Err(err) => {
                                            app.push_block(
                                                DotColor::Red,
                                                &format!("读取会话失败: {err}"),
                                            );
                                        }
                                    },
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
                                                app.push_block(
                                                    DotColor::White,
                                                    "已创建并切换到新会话",
                                                );
                                            }
                                            Err(err) => {
                                                app.push_block(
                                                    DotColor::Red,
                                                    &format!("创建会话失败: {err}"),
                                                );
                                            }
                                        }
                                    }
                                    "show" => {
                                        let id = parts.next().unwrap_or("");
                                        if id.is_empty() {
                                            app.push_block(
                                                DotColor::Red,
                                                "用法: /session show <id>",
                                            );
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
                                                    app.push_block(
                                                        DotColor::White,
                                                        &lines.join("\n"),
                                                    );
                                                }
                                                Err(err) => {
                                                    app.push_block(
                                                        DotColor::Red,
                                                        &format!("读取消息失败: {err}"),
                                                    );
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
                            "resume" => {
                                let target = args.split_whitespace().next().unwrap_or("");
                                if !target.is_empty() {
                                    resume_session(app, store, target).await;
                                    return Ok(true);
                                }

                                let sessions = match store.list_sessions().await {
                                    Ok(sessions) => sessions,
                                    Err(err) => {
                                        app.push_block(
                                            DotColor::Red,
                                            &format!("读取会话失败: {err}"),
                                        );
                                        return Ok(true);
                                    }
                                };
                                if sessions.is_empty() {
                                    app.push_block(DotColor::White, "暂无可恢复的会话");
                                    return Ok(true);
                                }

                                let mut options = Vec::new();
                                for session in &sessions {
                                    let summary = session_summary_for_display(store, session).await;
                                    options.push(UserInputOption {
                                        id: session.id.clone(),
                                        label: format_session_option(session, &summary),
                                    });
                                }
                                let request = UserInputRequest {
                                    id: "resume".to_string(),
                                    title: Some("选择要恢复的会话".to_string()),
                                    questions: vec![UserInputQuestion {
                                        id: "session".to_string(),
                                        prompt: "请选择会话".to_string(),
                                        options: Some(options),
                                    }],
                                };
                                let (resp_tx, resp_rx) = oneshot::channel();
                                if ui_tx
                                    .send(UiRequest::UserInput {
                                        request,
                                        respond_to: resp_tx,
                                    })
                                    .is_err()
                                {
                                    app.push_block(DotColor::Red, "无法发起恢复选择");
                                    return Ok(true);
                                }
                                let ui_tx = ui_tx.clone();
                                tokio::spawn(async move {
                                    if let Ok(resp) = resp_rx.await {
                                        if resp.cancelled {
                                            return;
                                        }
                                        let selected = resp
                                            .answers
                                            .get("session")
                                            .and_then(|ans| ans.option_id.clone());
                                        if let Some(session_id) = selected {
                                            let _ = ui_tx
                                                .send(UiRequest::ResumeSelected { session_id });
                                        }
                                    }
                                });
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
                                let plugins = plugins.clone();
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
                                            plugins,
                                            tool_approvals,
                                            None,
                                            None,
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
                        plugins.clone(),
                        tool_approvals.clone(),
                        None,
                        None,
                    );
                    let session_id = app.session_id.clone();
                    let input_clone = raw_input.clone();
                    let tx_clone = tx.clone();
                    *runner = Some(tokio::spawn(async move {
                        agent
                            .run_turn(&session_id, &input_clone, Some(tx_clone))
                            .await
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

async fn handle_ui_request(
    req: UiRequest,
    app: &mut App,
    store: &std::sync::Arc<dyn SessionStore>,
    slash: &SlashRegistry,
) {
    match req {
        UiRequest::UserInput {
            request,
            respond_to,
        } => {
            app.enqueue_overlay(InfoOverlay::UserInput(UserInputOverlay::new(
                request, respond_to,
            )));
        }
        UiRequest::ToolApproval {
            request,
            respond_to,
        } => {
            app.enqueue_overlay(InfoOverlay::ToolApproval(ToolApprovalOverlay::new(
                request, respond_to,
            )));
        }
        UiRequest::ResumeSelected { session_id } => {
            resume_session(app, store, &session_id).await;
        }
        UiRequest::RewindSelected { message_id, input } => {
            match store
                .rewind_to_before_message(&app.session_id, &message_id)
                .await
            {
                Ok(()) => {
                    let session_id = app.session_id.clone();
                    load_session_into_output(app, store, &session_id).await;
                    app.status = Status::Idle;
                    app.input = input;
                    app.cursor = app.input.chars().count();
                    app.refresh_slash(slash);
                    app.push_block(
                        DotColor::White,
                        "已回退到所选用户输入之前，原输入已恢复到输入框",
                    );
                }
                Err(err) => {
                    app.push_block(DotColor::Red, &format!("回退会话失败: {err}"));
                }
            }
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
        AgentEvent::ToolCallStarted {
            tool_call_id: _,
            name,
            input,
        } => {
            app.finalize_stream();
            let args = one_line(&input);
            let label = format_tool_label(&name, &args, app.viewport_width);
            app.last_tool_label = Some(label.clone());
            app.push_running_tool(&label);
            app.status = Status::Tool(label);
            app.blink_on = true;
            app.last_blink = Instant::now();
        }
        AgentEvent::ToolCallFinished {
            tool_call_id: _,
            name,
            output,
            ok,
        } => {
            app.complete_running_tool(&name, output.trim(), ok);
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
        AgentEvent::PluginWarning {
            plugin,
            hook,
            message,
            degraded,
        } => {
            let text = if degraded {
                format!("插件降级继续: plugin={plugin}, hook={hook}, message={message}")
            } else {
                format!("插件告警: plugin={plugin}, hook={hook}, message={message}")
            };
            app.push_block(DotColor::Yellow, &text);
        }
        AgentEvent::HookStarted {
            event,
            hook_name,
            status_message,
        } => {
            let label = status_message
                .as_deref()
                .unwrap_or(&hook_name)
                .to_string();
            app.finalize_stream();
            app.push_running_hook(&label);
            app.active_hooks.push(hook_name.clone());
            app.status = Status::Hook(format!("{}: {}", event, hook_name));
            app.blink_on = true;
            app.last_blink = Instant::now();
        }
        AgentEvent::HookFinished {
            event: _,
            hook_name,
            ok,
            message,
        } => {
            let label = message.as_deref().unwrap_or(&hook_name).to_string();
            app.complete_running_hook(ok, &label);
            app.active_hooks.retain(|h| h != &hook_name);
            if app.active_hooks.is_empty() {
                app.status = Status::Thinking;
            }
            app.blink_on = true;
            app.last_blink = Instant::now();
        }
        AgentEvent::SessionCost {
            input_tokens,
            output_tokens,
            cache_creation_tokens,
            cache_read_tokens,
            turn_count,
        } => {
            app.session_input_tokens = input_tokens;
            app.session_output_tokens = output_tokens;
            app.session_cache_creation_tokens = cache_creation_tokens;
            app.session_cache_read_tokens = cache_read_tokens;
            app.session_turn_count = turn_count;
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
    // Auto-clear completed todos when agent is idle (like Claude Code's auto-hide)
    if !app.todos.is_empty()
        && matches!(app.status, Status::Idle)
        && app
            .todos
            .iter()
            .all(|t| matches!(t.status, TodoStatus::Completed | TodoStatus::Cancelled))
    {
        let _ = store.set_todos(&app.session_id, &[]).await;
        app.todos.clear();
    }
}

async fn load_session_into_output(
    app: &mut App,
    store: &std::sync::Arc<dyn SessionStore>,
    session_id: &str,
) {
    app.session_id = session_id.to_string();
    app.output.clear();
    app.stream_buffer.clear();
    app.streaming = false;
    app.scroll = 0;
    app.stick_to_bottom = true;
    app.status = Status::Idle;
    app.context_used = None;
    app.context_limit = None;
    app.last_tool_label = None;
    app.running_tool_output_idx = None;
    app.running_hook_output_idx = None;
    app.active_hooks.clear();
    app.last_copyable_output = None;
    app.clear_key_arms();
    app.status_notice = None;

    if let Ok(messages) = store.list_messages(session_id).await {
        for message in messages {
            match message.role {
                MessageRole::User => {
                    app.push_line(user_input_line(&message.content));
                }
                MessageRole::Assistant => {
                    app.push_markdown_block(&message.content);
                    if !message.content.trim().is_empty() {
                        app.last_copyable_output = Some(message.content.clone());
                    }
                }
                MessageRole::Tool => {
                    app.push_block(DotColor::White, &message.content);
                }
                MessageRole::System => {
                    app.push_block(DotColor::White, &message.content);
                }
            }
        }
    }

    refresh_session_state(app, store).await;
}

async fn resume_session(app: &mut App, store: &std::sync::Arc<dyn SessionStore>, session_id: &str) {
    match store.get_session(session_id).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            app.push_block(DotColor::Red, &format!("会话不存在: {session_id}"));
            return;
        }
        Err(err) => {
            app.push_block(DotColor::Red, &format!("读取会话失败: {err}"));
            return;
        }
    }

    load_session_into_output(app, store, session_id).await;
    app.push_block(DotColor::White, &format!("已恢复会话: {session_id}"));
}

async fn session_summary_for_display(
    store: &std::sync::Arc<dyn SessionStore>,
    session: &Session,
) -> String {
    if let Ok(messages) = store.list_messages(&session.id).await {
        if let Some(first) = messages
            .into_iter()
            .find(|msg| matches!(msg.role, MessageRole::User) && !msg.content.trim().is_empty())
        {
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

fn format_session_line(session: &Session, summary: &str) -> String {
    if summary.is_empty() {
        format!("  {}  {}", session.id, session.title)
    } else {
        format!("  {}  {}  {}", session.id, session.title, summary)
    }
}

fn format_session_option(session: &Session, summary: &str) -> String {
    if summary.is_empty() {
        format!("{}  {}", session.id, session.title)
    } else {
        format!("{}  {}  {}", session.id, session.title, summary)
    }
}

fn update_blink(app: &mut App) -> bool {
    if !matches!(app.status, Status::Thinking | Status::Tool(_) | Status::Hook(_)) {
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

    let display_lines = app.display_lines(output_area.width);
    let output_style = Style::default().fg(COLOR_TEXT);
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
        .scroll((app.scroll, 0))
        .style(output_style);
    frame.render_widget(output_widget, output_area);

    let info_widget = Paragraph::new(Text::from(info_lines))
        .block(
            Block::default()
                .title(Span::styled("会话信息", Style::default().fg(COLOR_ACCENT)))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(BORDER_COLOR)),
        )
        .style(Style::default().fg(COLOR_TEXT));
    frame.render_widget(info_widget, info_area);

    let input_block = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(Style::default().fg(BORDER_COLOR));
    let input_line = Line::from(vec![
        Span::styled(">", Style::default().fg(COLOR_ACCENT)),
        Span::raw(" "),
        Span::styled(app.input.clone(), Style::default().fg(COLOR_TEXT)),
    ]);
    let input_widget = Paragraph::new(Text::from(input_line))
        .block(input_block)
        .style(Style::default().fg(COLOR_TEXT));
    frame.render_widget(input_widget, input_area);

    let status_text = build_status_bar(app);
    let status_widget = Paragraph::new(Text::from(Line::from(Span::raw(status_text))))
        .style(Style::default().fg(COLOR_TEXT));
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
    let cursor_offset = UnicodeWidthStr::width(
        app.input
            .chars()
            .take(app.cursor)
            .collect::<String>()
            .as_str(),
    ) as u16;
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
            let selected = idx == prompt.selected;
            let prefix = if selected { "▸ " } else { "  " };
            let style = if selected {
                Style::default().fg(COLOR_TEXT).bg(COLOR_SELECTED_BG)
            } else {
                Style::default().fg(COLOR_MUTED)
            };
            Line::from(Span::styled(format!("{prefix}{opt}"), style))
        })
        .collect::<Vec<_>>();
    let block = Block::default()
        .title(prompt.title.clone())
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(BORDER_COLOR));
    let widget = Paragraph::new(Text::from(lines))
        .block(block)
        .style(Style::default().fg(COLOR_TEXT));
    frame.render_widget(widget, area);
}

fn format_token_count(count: u64) -> String {
    if count >= 1_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else if count >= 1_000 {
        format!("{:.1}K", count as f64 / 1_000.0)
    } else {
        count.to_string()
    }
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
        (Some(used), Some(limit)) if limit > 0 => {
            format!("{:.1}%", (used as f64 / limit as f64) * 100.0)
        }
        _ => "-".to_string(),
    };
    let commands = app.command_hint();
    let mut parts = vec![
        format!("Session: {}", app.session_id),
        format!("{} / {}", app.provider_id, app.model),
        format!("Tokens: {used}/{limit}/{percent}"),
    ];
    if app.session_turn_count > 0 {
        let total = app.session_input_tokens + app.session_output_tokens;
        let cache_total = app.session_cache_creation_tokens + app.session_cache_read_tokens;
        let cache_rate = if total > 0 {
            format!("{:.0}%", (app.session_cache_read_tokens as f64 / total as f64) * 100.0)
        } else {
            "-".to_string()
        };
        parts.push(format!(
            "Cost: {}tok (cache: {}, hit: {})",
            format_token_count(total),
            format_token_count(cache_total),
            cache_rate,
        ));
    }
    if !commands.is_empty() {
        parts.push(format!("Commands: {commands}"));
    }
    if let Some(notice) = &app.status_notice {
        parts.push(notice.clone());
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
        Span::styled(">", Style::default().fg(COLOR_ACCENT)),
        Span::raw(" "),
        Span::styled(text.to_string(), Style::default().fg(COLOR_TEXT)),
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
            spans.push(Span::styled(
                left.to_string(),
                Style::default().fg(LOGO_COLOR),
            ));
            spans.push(Span::raw(" ".repeat(left_pad)));
            spans.push(Span::raw("  "));
            if right_is_border {
                spans.push(Span::styled(
                    right.to_string(),
                    Style::default().fg(BORDER_COLOR),
                ));
            } else {
                // right line has borders; color only the borders to keep text bright.
                let mut chars = right.chars();
                let left_border = chars.next().unwrap_or('│').to_string();
                let right_border = right.chars().last().unwrap_or('│').to_string();
                let middle: String = right
                    .chars()
                    .skip(1)
                    .take(right.chars().count().saturating_sub(2))
                    .collect();
                spans.push(Span::styled(left_border, Style::default().fg(BORDER_COLOR)));
                spans.push(Span::raw(middle));
                spans.push(Span::styled(
                    right_border,
                    Style::default().fg(BORDER_COLOR),
                ));
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
    let mut lines = markdown_to_lines_with_thinking(text, width);
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum ThinkingSegmentKind {
    Normal,
    Thinking,
}

struct ThinkingSegment {
    kind: ThinkingSegmentKind,
    content: String,
}

fn normalize_thinking_fences(text: &str) -> String {
    let mut out = String::new();
    let text = text
        .replace("<think>", "<thinking>")
        .replace("</think>", "</thinking>");
    let mut in_thinking = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if !in_thinking {
            if trimmed.starts_with("```thinking") || trimmed.starts_with("```analysis") {
                in_thinking = true;
                out.push_str("<thinking>\n");
                continue;
            }
            out.push_str(line);
            out.push('\n');
        } else if trimmed.starts_with("```") {
            in_thinking = false;
            out.push_str("</thinking>\n");
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    if in_thinking {
        out.push_str("</thinking>\n");
    }
    out
}

fn split_thinking_blocks(text: &str) -> Vec<ThinkingSegment> {
    const OPEN: &str = "<thinking>";
    const CLOSE: &str = "</thinking>";
    let mut out = Vec::new();
    let mut rest = text;
    loop {
        let Some(start) = rest.find(OPEN) else {
            if !rest.is_empty() {
                out.push(ThinkingSegment {
                    kind: ThinkingSegmentKind::Normal,
                    content: rest.to_string(),
                });
            }
            break;
        };
        let before = &rest[..start];
        if !before.is_empty() {
            out.push(ThinkingSegment {
                kind: ThinkingSegmentKind::Normal,
                content: before.to_string(),
            });
        }
        let after_open = &rest[start + OPEN.len()..];
        let Some(end) = after_open.find(CLOSE) else {
            out.push(ThinkingSegment {
                kind: ThinkingSegmentKind::Normal,
                content: rest.to_string(),
            });
            break;
        };
        let content = &after_open[..end];
        out.push(ThinkingSegment {
            kind: ThinkingSegmentKind::Thinking,
            content: content.to_string(),
        });
        rest = &after_open[end + CLOSE.len()..];
    }
    out
}

fn markdown_to_lines_with_thinking(text: &str, width: u16) -> Vec<Line<'static>> {
    let normalized = normalize_thinking_fences(text);
    let segments = split_thinking_blocks(&normalized);
    if segments.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for seg in segments {
        match seg.kind {
            ThinkingSegmentKind::Normal => {
                out.extend(markdown_to_lines(&seg.content, width));
            }
            ThinkingSegmentKind::Thinking => {
                out.extend(format_thinking_block_lines(&seg.content, width));
            }
        }
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
            current.push(Span::styled(prefix, Style::default().fg(COLOR_MUTED)));
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
                    let style = style_stack
                        .last()
                        .copied()
                        .unwrap_or_default()
                        .add_modifier(Modifier::ITALIC);
                    style_stack.push(style);
                }
                Tag::Strong => {
                    let style = style_stack
                        .last()
                        .copied()
                        .unwrap_or_default()
                        .add_modifier(Modifier::BOLD);
                    style_stack.push(style);
                }
                Tag::Strikethrough => {
                    let style = style_stack
                        .last()
                        .copied()
                        .unwrap_or_default()
                        .add_modifier(Modifier::CROSSED_OUT);
                    style_stack.push(style);
                }
                Tag::Heading { .. } => {
                    let style = style_stack
                        .last()
                        .copied()
                        .unwrap_or_default()
                        .add_modifier(Modifier::BOLD);
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
                    let indent = if ordered {
                        index.to_string().len() + 2
                    } else {
                        2
                    };
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
                        .fg(COLOR_ACCENT)
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
                        render_table_lines(
                            &table_rows,
                            &table_align,
                            table_header_rows,
                            &mut lines,
                        );
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
                    .fg(COLOR_WARN);
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

fn format_thinking_block_lines(text: &str, width: u16) -> Vec<Line<'static>> {
    let mut box_width = width.saturating_sub(6) as usize;
    if box_width < 12 {
        box_width = 12;
    }
    let inner_width = box_width.saturating_sub(2);
    let content_limit = inner_width.saturating_sub(2);
    let mut content_lines: Vec<String> = text.lines().map(|l| l.trim_end().to_string()).collect();
    while content_lines.first().is_some_and(|s| s.trim().is_empty()) {
        content_lines.remove(0);
    }
    while content_lines.last().is_some_and(|s| s.trim().is_empty()) {
        content_lines.pop();
    }
    if content_lines.is_empty() {
        content_lines.push("（无思考内容）".to_string());
    }
    let mut wrapped_lines = Vec::new();
    for line in content_lines {
        wrapped_lines.extend(wrap_text_to_width(&line, content_limit.max(1)));
    }

    let border_style = Style::default().fg(BORDER_COLOR);
    let title_style = Style::default()
        .fg(COLOR_ACCENT_DIM)
        .add_modifier(Modifier::BOLD);
    let text_style = Style::default().fg(COLOR_MUTED);

    let label = " 思考 ";
    let mut label_text = label.to_string();
    let mut label_width = UnicodeWidthStr::width(label_text.as_str());
    if label_width > inner_width {
        label_text = truncate_to_width(&label_text, inner_width);
        label_width = UnicodeWidthStr::width(label_text.as_str());
    }
    let dash_total = inner_width.saturating_sub(label_width);
    let left_dash = dash_total / 2;
    let right_dash = dash_total - left_dash;

    let mut out = Vec::new();
    let mut top = Vec::new();
    top.push(Span::styled("╭", border_style));
    top.push(Span::styled("─".repeat(left_dash), border_style));
    top.push(Span::styled(label_text, title_style));
    top.push(Span::styled("─".repeat(right_dash), border_style));
    top.push(Span::styled("╮", border_style));
    out.push(Line::from(top));

    for line in wrapped_lines {
        let line_width = UnicodeWidthStr::width(line.as_str());
        let pad = content_limit.saturating_sub(line_width);
        let mut spans = Vec::new();
        spans.push(Span::styled("│", border_style));
        spans.push(Span::styled(" ", border_style));
        spans.push(Span::styled(line, text_style));
        spans.push(Span::styled(" ".repeat(pad), text_style));
        spans.push(Span::styled(" ", border_style));
        spans.push(Span::styled("│", border_style));
        out.push(Line::from(spans));
    }

    out.push(Line::from(Span::styled(
        format!("╰{}╯", "─".repeat(inner_width)),
        border_style,
    )));
    out
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

fn render_code_block_lines(lines: &[String], lang: Option<&str>, width: u16) -> Vec<Line<'static>> {
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

    let border_style = Style::default().fg(BORDER_COLOR);
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
            spans.push(Span::styled(
                trimmed.clone(),
                Style::default().fg(COLOR_TEXT),
            ));
        } else {
            for (style, text) in regions {
                let mut span_style = Style::default().fg(Color::Rgb(
                    style.foreground.r,
                    style.foreground.g,
                    style.foreground.b,
                ));
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
        DotColor::White => COLOR_ACCENT,
        DotColor::Green => COLOR_SUCCESS,
        DotColor::Yellow => COLOR_WARN,
        DotColor::Red => COLOR_ERROR,
    };
    Span::styled("●", Style::default().fg(fg))
}

fn tool_dot_span(color: DotColor) -> Span<'static> {
    let fg = match color {
        DotColor::White => COLOR_ACCENT,
        DotColor::Green => COLOR_SUCCESS,
        DotColor::Yellow => COLOR_WARN,
        DotColor::Red => COLOR_ERROR,
    };
    Span::styled("⏺", Style::default().fg(fg))
}

fn running_tool_dot_span(blink_on: bool) -> Span<'static> {
    if blink_on {
        tool_dot_span(DotColor::White)
    } else {
        Span::styled(" ", Style::default().fg(COLOR_ACCENT))
    }
}

fn format_running_tool_line(label: &str, blink_on: bool) -> Line<'static> {
    let mut line = Vec::new();
    line.push(running_tool_dot_span(blink_on));
    line.push(Span::raw(" "));
    line.push(Span::styled(
        label.to_string(),
        Style::default().fg(COLOR_TEXT),
    ));
    Line::from(line)
}

fn format_running_hook_line(label: &str, blink_on: bool) -> Line<'static> {
    let icon = if blink_on { "⚡" } else { " " };
    Line::from(vec![
        Span::styled(icon, Style::default().fg(COLOR_WARN)),
        Span::styled(" ", Style::default()),
        Span::styled(
            format!("Hook: {label}"),
            Style::default().fg(COLOR_WARN).add_modifier(Modifier::DIM),
        ),
    ])
}

fn format_hook_output_line(ok: bool, label: &str) -> Line<'static> {
    let (icon, color) = if ok {
        ("✓", COLOR_SUCCESS)
    } else {
        ("✗", COLOR_ERROR)
    };
    Line::from(vec![
        Span::styled(icon, Style::default().fg(color)),
        Span::styled(" ", Style::default()),
        Span::styled(
            format!("Hook: {label}"),
            Style::default().fg(color).add_modifier(Modifier::DIM),
        ),
    ])
}

fn format_tool_box_lines(lines: &[String], width: u16) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let mut box_width = width.saturating_sub(6) as usize;
    if box_width < 10 {
        box_width = 10;
    }
    let inner = box_width.saturating_sub(2);
    let border_style = Style::default().fg(BORDER_COLOR);
    let text_style = Style::default().fg(COLOR_TEXT);
    let top = format!("  ╭{}╮", "─".repeat(inner));
    out.push(Line::from(Span::styled(top, border_style)));
    let content_width = inner.saturating_sub(2);
    for line in lines.iter() {
        let wrapped = wrap_text_to_width(line, content_width.max(1));
        for piece in wrapped {
            let pad = content_width.saturating_sub(UnicodeWidthStr::width(piece.as_str()));
            let mut spans = Vec::new();
            spans.push(Span::styled("  │ ", border_style));
            spans.push(Span::styled(piece, text_style));
            spans.push(Span::styled(" ".repeat(pad), text_style));
            spans.push(Span::styled(" │", border_style));
            out.push(Line::from(spans));
        }
    }
    let bottom = format!("  ╰{}╯", "─".repeat(inner));
    out.push(Line::from(Span::styled(bottom, border_style)));
    out
}

fn format_tool_output_lines(
    color: DotColor,
    label: Option<&str>,
    output: &str,
    width: u16,
    show_full: bool,
    always_full: bool,
) -> Vec<Line<'static>> {
    let lines: Vec<String> = output.lines().map(|s| s.to_string()).collect();
    let (lines, omitted) = if show_full || always_full {
        (lines, 0)
    } else {
        truncate_lines(output, 3)
    };
    let mut out = Vec::new();
    if let Some(label) = label {
        let mut line = Vec::new();
        line.push(tool_dot_span(color));
        line.push(Span::raw(" "));
        line.push(Span::styled(
            label.to_string(),
            Style::default().fg(COLOR_TEXT),
        ));
        out.push(Line::from(line));
    }
    let mut content_lines = Vec::new();
    content_lines.extend(lines);
    if omitted > 0 {
        content_lines.push(format!("... 已省略 {} 行", omitted));
    }
    out.extend(format_tool_box_lines(&content_lines, width));
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

fn wrap_text_to_width(text: &str, max_width: usize) -> Vec<String> {
    if max_width == 0 {
        return vec![String::new()];
    }
    let mut out = Vec::new();
    let mut current = String::new();
    let mut width = 0usize;
    for ch in text.chars() {
        let ch_width = UnicodeWidthStr::width(ch.to_string().as_str());
        if width + ch_width > max_width && !current.is_empty() {
            out.push(current);
            current = String::new();
            width = 0;
        }
        current.push(ch);
        width += ch_width;
    }
    if !current.is_empty() || out.is_empty() {
        out.push(current);
    }
    out
}

fn is_full_output_tool(name: &str) -> bool {
    matches!(name, "write" | "apply_patch" | "edit")
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
    let max = width as usize;
    let mut total = 0usize;
    for line in lines {
        total += count_wrapped_line(line.to_string().as_str(), max);
    }
    total
}

fn count_wrapped_line(text: &str, max_width: usize) -> usize {
    if max_width == 0 {
        return 0;
    }
    if text.is_empty() {
        return 1;
    }
    let mut count = 0usize;
    let mut line_width = 0usize;
    let mut iter = text.chars().peekable();

    while let Some(ch) = iter.next() {
        let is_space = ch.is_whitespace() && ch != '\u{00a0}';
        let mut token = String::new();
        token.push(ch);
        while let Some(&next) = iter.peek() {
            let next_is_space = next.is_whitespace() && next != '\u{00a0}';
            if next_is_space == is_space {
                token.push(next);
                iter.next();
            } else {
                break;
            }
        }
        let mut token_width = UnicodeWidthStr::width(token.as_str());
        if token_width == 0 {
            continue;
        }

        if !is_space && token_width > max_width {
            if line_width > 0 {
                count += 1;
            }
            let full = token_width / max_width;
            if full > 0 {
                count += full;
                token_width %= max_width;
            }
            line_width = token_width;
            continue;
        }

        if line_width + token_width > max_width {
            count += 1;
            line_width = 0;
        }
        if token_width > max_width {
            let full = token_width / max_width;
            count += full;
            line_width = token_width % max_width;
        } else {
            line_width += token_width;
        }
    }

    if line_width > 0 || count == 0 {
        count += 1;
    }
    count
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
