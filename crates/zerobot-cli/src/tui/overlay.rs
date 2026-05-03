//! Overlay system for modal dialogs and popups.
//!
//! Each overlay type is a self-contained widget that knows how to render itself
//! and handle key events.  `OverlayType` carries the data for each variant so
//! that `AppState` can hold a single `Option<OverlayType>`.

use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;
use ratatui::buffer::Buffer;

use zerobot_core::interaction::{
    ToolApprovalDecision, ToolApprovalRequest, ToolApprovalResponse,
    UserInputAnswer, UserInputRequest, UserInputResponse,
};

use crate::tui::message::Message;
use crate::tui::theme::Theme;

// ---------------------------------------------------------------------------
// OverlayType — the single enum that holds all overlay state.
// ---------------------------------------------------------------------------

/// Identifies which overlay is currently displayed (or queued) and carries its
/// mutable widget state.
#[derive(Debug)]
pub enum OverlayType {
    ToolApproval(ToolApprovalOverlay),
    UserInput(UserInputOverlay),
    HistorySearch(HistorySearchOverlay),
    Help(HelpOverlay),
    MessageSelector(MessageSelectorOverlay),
    TurnCost(TurnCostOverlay),
}

// ---------------------------------------------------------------------------
// OverlayComponent trait
// ---------------------------------------------------------------------------

/// Trait for overlay widgets.
///
/// Each overlay implements `render` to draw into a buffer and `handle_key` to
/// produce `Message` values.  `height_needed` reports how many rows the overlay
/// needs at a given width.
pub trait OverlayComponent {
    fn render(&self, area: Rect, buf: &mut Buffer, theme: &Theme);
    fn handle_key(&mut self, key: KeyEvent) -> Option<Message>;
    fn height_needed(&self, width: u16) -> u16;
}

// ---------------------------------------------------------------------------
// ToolApprovalOverlay
// ---------------------------------------------------------------------------

/// Overlay for tool approval (permission) prompts.
///
/// Shows the tool name, arguments, and a list of options the user can select.
#[derive(Debug)]
pub struct ToolApprovalOverlay {
    pub request: ToolApprovalRequest,
    pub selected: usize,
    pub respond_to: Option<tokio::sync::oneshot::Sender<ToolApprovalResponse>>,
}

impl ToolApprovalOverlay {
    pub fn new(
        request: ToolApprovalRequest,
        respond_to: tokio::sync::oneshot::Sender<ToolApprovalResponse>,
    ) -> Self {
        Self {
            request,
            selected: 0,
            respond_to: Some(respond_to),
        }
    }

    /// Consume the overlay and send the response.
    pub fn finish(&mut self, decision: ToolApprovalDecision) {
        if let Some(respond_to) = self.respond_to.take() {
            let response = ToolApprovalResponse {
                decision,
                reason: None,
            };
            let _ = respond_to.send(response);
        }
    }

    fn lines(&self, theme: &Theme) -> Vec<Line<'static>> {
        let mut lines = Vec::new();

        // Title: tool name in bold
        lines.push(Line::from(Span::styled(
            self.request.tool_name.clone(),
            Style::default()
                .add_modifier(Modifier::BOLD)
                .fg(theme.accent),
        )));

        // Key parameter (command for bash, path for file tools, etc.)
        let detail_lines = tool_approval_detail_lines(&self.request, theme);
        if !detail_lines.is_empty() {
            lines.extend(detail_lines);
        } else if let Ok(args) = serde_json::to_string(&self.request.arguments) {
            let args = one_line(&args);
            if !args.is_empty() {
                lines.push(Line::from(Span::styled(args, Style::default().fg(theme.text))));
            }
        }

        // Question
        lines.push(Line::from(Span::styled(
            "是否允许执行？",
            Style::default().fg(theme.text_muted),
        )));

        // Options
        let options = ["仅本次允许", "本会话允许", "本工作区允许", "拒绝"];
        for (idx, opt) in options.iter().enumerate() {
            let selected = idx == self.selected;
            let prefix = if selected { "▸ " } else { "  " };
            let style = if selected {
                Style::default().fg(theme.text).bg(theme.selected_bg)
            } else {
                Style::default().fg(theme.text_muted)
            };
            lines.push(Line::from(Span::styled(format!("{prefix}{opt}"), style)));
        }
        lines
    }
}

impl OverlayComponent for ToolApprovalOverlay {
    fn render(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        render_top_border(buf, area, theme);
        let content = Rect::new(area.x, area.y + 1, area.width, area.height.saturating_sub(1));
        render_overlay_content(buf, content, &self.lines(theme), theme);
    }

    fn handle_key(&mut self, key: KeyEvent) -> Option<Message> {
        match key.code {
            KeyCode::Up => {
                if self.selected > 0 {
                    self.selected -= 1;
                }
                None
            }
            KeyCode::Down => {
                if self.selected + 1 < 4 {
                    self.selected += 1;
                }
                None
            }
            KeyCode::Enter => {
                let decision = match self.selected {
                    0 => ToolApprovalDecision::AllowOnce,
                    1 => ToolApprovalDecision::AllowSession,
                    2 => ToolApprovalDecision::AllowWorkspace,
                    _ => ToolApprovalDecision::Deny,
                };
                self.finish(decision);
                Some(Message::CloseOverlay)
            }
            KeyCode::Esc => {
                self.finish(ToolApprovalDecision::Deny);
                Some(Message::CloseOverlay)
            }
            _ => None,
        }
    }

    fn height_needed(&self, _width: u16) -> u16 {
        // Top border + title + detail + question + 4 options
        let mut h = 1; // top border
        h += 1; // title (tool name)
        // detail lines (bash command, file path, etc.)
        h += 1; // at least one detail/fallback line
        // destructive warning for bash
        if self.request.tool_name == "bash" || self.request.tool_name == "shell" {
            if let Some(cmd) = self.request.arguments.get("command").and_then(|v| v.as_str()) {
                if cmd.contains("rm ") || cmd.contains("rm -") || cmd.contains("rmdir")
                    || cmd.contains("mkfs") || cmd.contains("dd ") || cmd.contains("> /dev/")
                    || cmd.contains("chmod -R") || cmd.contains("chown -R")
                {
                    h += 1;
                }
            }
        }
        h += 1; // "是否允许执行？"
        h += 4; // options
        h
    }
}

// ---------------------------------------------------------------------------
// UserInputOverlay
// ---------------------------------------------------------------------------

/// Focus state for the user input overlay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserInputFocus {
    Options,
    Input,
}

/// Overlay for multi-question user input prompts.
#[derive(Debug)]
pub struct UserInputOverlay {
    pub request: UserInputRequest,
    pub current: usize,
    pub selected: usize,
    pub focus: UserInputFocus,
    pub notes: HashMap<(String, Option<String>), String>,
    pub answers: HashMap<String, UserInputAnswer>,
    pub respond_to: Option<tokio::sync::oneshot::Sender<UserInputResponse>>,
}

impl UserInputOverlay {
    pub fn new(
        request: UserInputRequest,
        respond_to: tokio::sync::oneshot::Sender<UserInputResponse>,
    ) -> Self {
        let focus = if request
            .questions
            .first()
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

    fn finish(&mut self, cancelled: bool) {
        if let Some(respond_to) = self.respond_to.take() {
            let response = UserInputResponse {
                answers: self.answers.clone(),
                cancelled,
            };
            let _ = respond_to.send(response);
        }
    }

    fn lines(&self, theme: &Theme) -> Vec<Line<'static>> {
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
                .fg(theme.accent),
        )));
        if self.request.questions.len() > 1 {
            let mut spans = Vec::new();
            spans.push(Span::styled("问题: ", Style::default().fg(theme.text_muted)));
            for (idx, q) in self.request.questions.iter().enumerate() {
                let label = truncate_chars(&q.prompt, 12);
                let text = format!("{}{} ", idx + 1, label);
                if idx == self.current {
                    spans.push(Span::styled(
                        text,
                        Style::default().add_modifier(Modifier::BOLD).fg(theme.text),
                    ));
                } else {
                    spans.push(Span::styled(text, Style::default().fg(theme.text_muted)));
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
                        Style::default().fg(theme.text).bg(theme.selected_bg)
                    } else {
                        Style::default().fg(theme.text_muted)
                    };
                    lines.push(Line::from(Span::styled(
                        format!("{prefix}{}", opt.label),
                        style,
                    )));
                }
            } else {
                lines.push(Line::from(Span::styled(
                    "（无选项，直接输入）",
                    Style::default().fg(theme.text_muted),
                )));
            }
            let note = self.current_note();
            let selected = self.focus == UserInputFocus::Input;
            let prefix = if selected { "▸ " } else { "  " };
            let style = if selected {
                Style::default().fg(theme.text).bg(theme.selected_bg)
            } else {
                Style::default().fg(theme.text_muted)
            };
            lines.push(Line::from(Span::styled(
                format!("{prefix}输入内容: {note}"),
                style,
            )));
        }
        lines.push(Line::from(Span::styled(
            "↑/↓ 选择  ←/→ 切换  Tab 切换输入  Enter 下一项/提交  Esc 取消",
            Style::default().fg(theme.text_muted),
        )));
        lines
    }
}

impl OverlayComponent for UserInputOverlay {
    fn render(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        render_top_border(buf, area, theme);
        let content = Rect::new(area.x, area.y + 1, area.width, area.height.saturating_sub(1));
        render_overlay_content(buf, content, &self.lines(theme), theme);
    }

    fn handle_key(&mut self, key: KeyEvent) -> Option<Message> {
        match key.code {
            KeyCode::Up => {
                if self.focus == UserInputFocus::Options {
                    if self.selected > 0 {
                        self.selected -= 1;
                    }
                }
                None
            }
            KeyCode::Down => {
                if self.focus == UserInputFocus::Options {
                    if let Some(question) = self.current_question() {
                        if let Some(options) = &question.options {
                            if self.selected + 1 < options.len() {
                                self.selected += 1;
                            }
                        }
                    }
                }
                None
            }
            KeyCode::Backspace => {
                if self.focus == UserInputFocus::Input {
                    if let Some(note) = self.current_note_mut() {
                        note.pop();
                    }
                }
                None
            }
            KeyCode::Tab => {
                self.focus = if self.focus == UserInputFocus::Options {
                    UserInputFocus::Input
                } else {
                    UserInputFocus::Options
                };
                None
            }
            KeyCode::Left => {
                if self.current > 0 {
                    self.commit_current_answer();
                    self.current -= 1;
                    self.selected = 0;
                    self.reset_focus_for_current();
                }
                None
            }
            KeyCode::Right => {
                if self.current + 1 < self.request.questions.len() {
                    self.commit_current_answer();
                    self.current += 1;
                    self.selected = 0;
                    self.reset_focus_for_current();
                }
                None
            }
            KeyCode::Enter => {
                self.commit_current_answer();
                if self.current + 1 >= self.request.questions.len() {
                    self.finish(false);
                    return Some(Message::CloseOverlay);
                }
                self.current += 1;
                self.selected = 0;
                self.reset_focus_for_current();
                None
            }
            KeyCode::Esc => {
                self.finish(true);
                Some(Message::CloseOverlay)
            }
            KeyCode::Char(ch) => {
                if !key.modifiers.contains(KeyModifiers::CONTROL) {
                    self.focus = UserInputFocus::Input;
                    if let Some(note) = self.current_note_mut() {
                        note.push(ch);
                    }
                }
                None
            }
            _ => None,
        }
    }

    fn height_needed(&self, _width: u16) -> u16 {
        let mut h = 1; // top border
        h += 1; // title
        if self.request.questions.len() > 1 {
            h += 1; // question tabs
        }
        h += 1; // question text
        if let Some(question) = self.current_question() {
            if let Some(options) = &question.options {
                h += options.len() as u16;
            } else {
                h += 1;
            }
            h += 1; // note input
        }
        h += 1; // help
        h
    }
}

// ---------------------------------------------------------------------------
// HistorySearchOverlay
// ---------------------------------------------------------------------------

/// A single search result for history search.
#[derive(Clone, Debug)]
pub struct SearchResult {
    pub message_id: String,
    pub role: String,
    pub preview: String,
}

/// Overlay for searching conversation history.
#[derive(Clone, Debug)]
pub struct HistorySearchOverlay {
    pub query: String,
    pub cursor: usize,
    pub results: Vec<SearchResult>,
    pub selected: usize,
}

impl HistorySearchOverlay {
    pub fn new() -> Self {
        Self {
            query: String::new(),
            cursor: 0,
            results: Vec::new(),
            selected: 0,
        }
    }

    /// Return the message ID of the currently selected search result.
    pub fn selected_message_id(&self) -> Option<&str> {
        self.results.get(self.selected).map(|r| r.message_id.as_str())
    }

    fn lines(&self, theme: &Theme) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        lines.push(Line::from(Span::styled(
            "历史搜索",
            Style::default()
                .add_modifier(Modifier::BOLD)
                .fg(theme.accent),
        )));
        let query_display = if self.query.is_empty() {
            "输入关键词搜索...".to_string()
        } else {
            self.query.clone()
        };
        let query_style = if self.query.is_empty() {
            Style::default().fg(theme.text_muted)
        } else {
            Style::default().fg(theme.text)
        };
        lines.push(Line::from(vec![
            Span::styled("搜索: ", Style::default().fg(theme.text_muted)),
            Span::styled(query_display, query_style),
        ]));
        if self.query.is_empty() {
            lines.push(Line::from(Span::styled(
                "输入关键词开始搜索",
                Style::default().fg(theme.text_muted),
            )));
        } else if self.results.is_empty() {
            lines.push(Line::from(Span::styled(
                "未找到匹配结果",
                Style::default().fg(theme.text_muted),
            )));
        } else {
            let max_display = 10;
            for (i, result) in self.results.iter().take(max_display).enumerate() {
                let selected = i == self.selected;
                let prefix = if selected { "▸ " } else { "  " };
                let role_icon = match result.role.as_str() {
                    "user" => "👤",
                    "assistant" => "🤖",
                    "tool" => "🔧",
                    _ => "📝",
                };
                let preview = if result.preview.len() > 60 {
                    format!("{}...", &result.preview[..57])
                } else {
                    result.preview.clone()
                };
                let style = if selected {
                    Style::default().fg(theme.text).bg(theme.selected_bg)
                } else {
                    Style::default().fg(theme.text_muted)
                };
                lines.push(Line::from(vec![
                    Span::styled(format!("{prefix}{role_icon} "), style),
                    Span::styled(preview, style),
                ]));
            }
            if self.results.len() > max_display {
                lines.push(Line::from(Span::styled(
                    format!("  ... 还有 {} 条结果", self.results.len() - max_display),
                    Style::default().fg(theme.text_muted),
                )));
            }
        }
        lines.push(Line::from(Span::styled(
            "↑/↓ 导航  Enter 跳转  Esc 关闭",
            Style::default().fg(theme.text_muted),
        )));
        lines
    }
}

impl Default for HistorySearchOverlay {
    fn default() -> Self {
        Self::new()
    }
}

impl OverlayComponent for HistorySearchOverlay {
    fn render(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        render_top_border(buf, area, theme);
        let content = Rect::new(area.x, area.y + 1, area.width, area.height.saturating_sub(1));
        render_overlay_content(buf, content, &self.lines(theme), theme);
    }

    fn handle_key(&mut self, key: KeyEvent) -> Option<Message> {
        match key.code {
            KeyCode::Esc => Some(Message::CloseOverlay),
            KeyCode::Up => {
                if self.selected > 0 {
                    self.selected -= 1;
                }
                None
            }
            KeyCode::Down => {
                if self.selected + 1 < self.results.len() {
                    self.selected += 1;
                }
                None
            }
            KeyCode::Enter => {
                // The caller checks `selected_message_id()` after receiving the message.
                Some(Message::CloseOverlay)
            }
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    let prev = self.query[..self.cursor]
                        .char_indices()
                        .last()
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                    self.query.drain(prev..self.cursor);
                    self.cursor = prev;
                    self.selected = 0;
                }
                None
            }
            KeyCode::Delete => {
                if self.cursor < self.query.len() {
                    let next = self.query[self.cursor..]
                        .char_indices()
                        .nth(1)
                        .map(|(i, _)| self.cursor + i)
                        .unwrap_or(self.query.len());
                    self.query.drain(self.cursor..next);
                }
                None
            }
            KeyCode::Left => {
                if self.cursor > 0 {
                    self.cursor = self.query[..self.cursor]
                        .char_indices()
                        .last()
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                }
                None
            }
            KeyCode::Right => {
                if self.cursor < self.query.len() {
                    self.cursor = self.query[self.cursor..]
                        .char_indices()
                        .nth(1)
                        .map(|(i, _)| self.cursor + i)
                        .unwrap_or(self.query.len());
                }
                None
            }
            KeyCode::Home => {
                self.cursor = 0;
                None
            }
            KeyCode::End => {
                self.cursor = self.query.len();
                None
            }
            KeyCode::Char(c) => {
                self.query.insert(self.cursor, c);
                self.cursor += c.len_utf8();
                self.selected = 0;
                None
            }
            _ => None,
        }
    }

    fn height_needed(&self, _width: u16) -> u16 {
        let mut h = 1; // top border
        h += 2; // title + query line
        if self.query.is_empty() {
            h += 1;
        } else if self.results.is_empty() {
            h += 1;
        } else {
            h += self.results.len().min(10) as u16;
            if self.results.len() > 10 {
                h += 1;
            }
        }
        h += 1; // help
        h
    }
}

// ---------------------------------------------------------------------------
// HelpOverlay (empty placeholder)
// ---------------------------------------------------------------------------

/// Overlay for displaying keybinding help.
#[derive(Clone, Debug)]
pub struct HelpOverlay;

impl OverlayComponent for HelpOverlay {
    fn render(&self, _area: Rect, _buf: &mut Buffer, _theme: &Theme) {
        // TODO: Implement help rendering in a future task.
    }

    fn handle_key(&mut self, _key: KeyEvent) -> Option<Message> {
        Some(Message::CloseOverlay)
    }

    fn height_needed(&self, _width: u16) -> u16 {
        0
    }
}

// ---------------------------------------------------------------------------
// MessageSelectorOverlay (empty placeholder)
// ---------------------------------------------------------------------------

/// Overlay for selecting a specific message in the conversation.
#[derive(Clone, Debug)]
pub struct MessageSelectorOverlay;

impl OverlayComponent for MessageSelectorOverlay {
    fn render(&self, _area: Rect, _buf: &mut Buffer, _theme: &Theme) {
        // TODO: Implement message selector rendering in a future task.
    }

    fn handle_key(&mut self, _key: KeyEvent) -> Option<Message> {
        Some(Message::CloseOverlay)
    }

    fn height_needed(&self, _width: u16) -> u16 {
        0
    }
}

// ---------------------------------------------------------------------------
// TurnCostOverlay (empty placeholder)
// ---------------------------------------------------------------------------

/// Overlay for displaying per-turn token cost breakdown.
#[derive(Clone, Debug)]
pub struct TurnCostOverlay;

impl OverlayComponent for TurnCostOverlay {
    fn render(&self, _area: Rect, _buf: &mut Buffer, _theme: &Theme) {
        // TODO: Implement turn cost rendering in a future task.
    }

    fn handle_key(&mut self, _key: KeyEvent) -> Option<Message> {
        Some(Message::CloseOverlay)
    }

    fn height_needed(&self, _width: u16) -> u16 {
        0
    }
}

// ---------------------------------------------------------------------------
// Helper: per-tool detail lines for ToolApprovalOverlay
// ---------------------------------------------------------------------------

fn tool_approval_detail_lines(
    request: &ToolApprovalRequest,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let tool_name = request.tool_name.as_str();

    match tool_name {
        "bash" | "shell" => {
            if let Some(cmd) = request.arguments.get("command").and_then(|v| v.as_str()) {
                let is_destructive = cmd.contains("rm ")
                    || cmd.contains("rm -")
                    || cmd.contains("rmdir")
                    || cmd.contains("mkfs")
                    || cmd.contains("dd ")
                    || cmd.contains("> /dev/")
                    || cmd.contains("chmod -R")
                    || cmd.contains("chown -R");
                let cmd_style = if is_destructive {
                    Style::default().fg(theme.error).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.text)
                };
                lines.push(Line::from(Span::styled(cmd.to_string(), cmd_style)));
                if is_destructive {
                    lines.push(Line::from(Span::styled(
                        "⚠ 检测到潜在破坏性操作",
                        Style::default().fg(theme.error).add_modifier(Modifier::BOLD),
                    )));
                }
            }
        }
        "write" | "edit" | "apply_patch" | "patch" => {
            if let Some(path) = request.arguments.get("file_path")
                .or_else(|| request.arguments.get("path"))
                .and_then(|v| v.as_str())
            {
                lines.push(Line::from(Span::styled(
                    path.to_string(),
                    Style::default().fg(theme.text),
                )));
            }
        }
        "skill" => {
            if let Some(name) = request.arguments.get("name").and_then(|v| v.as_str()) {
                lines.push(Line::from(Span::styled(
                    name.to_string(),
                    Style::default().fg(theme.text),
                )));
            }
        }
        _ => {}
    }
    lines
}

// ---------------------------------------------------------------------------
// Shared overlay rendering helpers
// ---------------------------------------------------------------------------

/// Render a rounded top border line (`╭────╮`) above overlay content.
fn render_top_border(buf: &mut Buffer, area: Rect, theme: &Theme) {
    if area.height == 0 || area.width < 3 {
        return;
    }
    let style = Style::default().fg(theme.panel_border).bg(theme.panel_bg);
    buf.set_string(area.x, area.y, "\u{256D}", style); // ╭
    let inner_w = area.width.saturating_sub(2) as usize;
    let hline: String = "\u{2500}".repeat(inner_w); // ─
    buf.set_string(area.x + 1, area.y, &hline, style);
    buf.set_string(area.x + area.width - 1, area.y, "\u{256E}", style); // ╮
    for x in area.x..area.x + area.width {
        buf.get_mut(x, area.y).set_style(Style::default().bg(theme.panel_bg));
    }
}

/// Render overlay content lines below the top border.
fn render_overlay_content(buf: &mut Buffer, area: Rect, lines: &[Line<'_>], theme: &Theme) {
    let style = Style::default().bg(theme.panel_bg);
    for (idx, line) in lines.iter().enumerate() {
        let y = area.y + idx as u16;
        if y >= area.y + area.height {
            break;
        }
        buf.set_style(Rect::new(area.x, y, area.width, 1), style);
        let inner = Rect::new(area.x + 1, y, area.width.saturating_sub(2), 1);
        Widget::render(ratatui::widgets::Paragraph::new(line.clone()), inner, buf);
    }
}

// ---------------------------------------------------------------------------
// Helper: single-line text normalization
// ---------------------------------------------------------------------------

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
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
