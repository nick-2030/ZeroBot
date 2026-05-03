//! Application state for the TUI.
//!
//! Contains all mutable state that drives the UI, migrated from the legacy
//! monolithic `tui.rs`.  Rendering components read `AppState` immutably;
//! event handlers mutate it.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use zerobot_core::config::PermissionMode;
use zerobot_core::provider::TokenUsage;
use zerobot_core::session::TodoItem;

use crate::slash::{SlashCommandKind, SlashMatch, SlashRegistry};
use crate::tui::command::Command;
use crate::tui::keybindings::types::KeyContext;
use crate::tui::message::Message;
use crate::tui::overlay::OverlayType;

// ---------------------------------------------------------------------------
// Supporting types (migrated from legacy.rs)
// ---------------------------------------------------------------------------

/// Blink interval for the cursor.
const BLINK_INTERVAL: Duration = Duration::from_millis(500);

/// Spinner verb pool — randomly selected on each submission.
const SPINNER_VERBS: &[&str] = &[
    "Thinking",
    "Processing",
    "Analyzing",
    "Reasoning",
    "Computing",
    "Working",
];

/// Status of the agent / LLM interaction.
#[derive(Clone, Debug)]
pub enum Status {
    Idle,
    Thinking,
    Tool(String),
    Hook(String),
    Error(String),
    WaitingUserInput,
    WaitingApproval,
}

/// Color marker used by `OutputItem::Block` and `ToolOutput`.
#[derive(Copy, Clone, Debug)]
pub enum DotColor {
    White,
    Green,
    Yellow,
    Red,
}

/// Per-turn token accounting.
#[derive(Clone, Debug)]
pub struct TurnCost {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation: u64,
    pub cache_read: u64,
}

/// A single item in the output scrollback.
#[derive(Clone, Debug)]
pub enum OutputItem {
    Lines(Vec<Line<'static>>),
    Block {
        color: DotColor,
        text: String,
    },
    Markdown(String),
    ToolRunning {
        label: String,
        arguments: String,
    },
    ToolOutput {
        color: DotColor,
        tool_name: String,
        label: Option<String>,
        arguments: String,
        output: String,
        expanded: bool,
        duration_ms: Option<u64>,
    },
    HookRunning {
        label: String,
    },
    HookOutput {
        ok: bool,
        label: String,
    },
}

/// Metadata for a currently-executing tool call.
#[derive(Clone, Debug)]
pub struct RunningTool {
    pub output_idx: usize,
    pub label: String,
    pub arguments: String,
    pub start_time: Instant,
}

// ---------------------------------------------------------------------------
// AppState
// ---------------------------------------------------------------------------

/// Central application state shared across all TUI components.
pub struct AppState {
    // -- Identity --
    pub session_id: String,
    pub provider_id: String,
    pub model: String,

    // -- Status --
    pub status: Status,
    pub permission_mode: PermissionMode,

    // -- Output / scroll --
    pub output: Vec<OutputItem>,
    pub stream_buffer: String,
    pub streaming: bool,
    pub scroll: u16,
    pub stick_to_bottom: bool,
    pub total_lines: usize,

    // -- Input --
    pub input: String,
    pub cursor: usize,

    // -- Token stats --
    pub usage: Option<TokenUsage>,
    pub context_used: Option<usize>,
    pub context_limit: Option<u32>,
    pub session_input_tokens: u64,
    pub session_output_tokens: u64,
    pub session_cache_creation_tokens: u64,
    pub session_cache_read_tokens: u64,
    pub session_turn_count: u32,
    pub turn_costs: Vec<TurnCost>,

    // -- Overlays --
    pub overlay: Option<OverlayType>,
    pub overlay_queue: VecDeque<OverlayType>,
    pub overlay_prev_status: Option<Status>,

    // -- Slash / autocomplete --
    pub slash_registry: SlashRegistry,
    pub slash_query: Option<String>,
    pub slash_matches: Vec<SlashMatch>,
    pub slash_selected: usize,
    pub slash_page: usize,
    pub slash_hint: String,

    // -- Running tools & hooks --
    pub running_tools: HashMap<String, RunningTool>,
    pub running_hook_output_idx: Option<usize>,
    pub active_hooks: Vec<String>,

    // -- Todos --
    pub todos: Vec<TodoItem>,

    // -- Display --
    pub show_full_tool_output: bool,
    pub viewport_width: u16,
    pub viewport_height: u16,

    // -- Quit --
    pub should_quit: bool,

    // -- Key debounce --
    pub last_idle_esc: Option<Instant>,
    pub last_idle_ctrl_c: Option<Instant>,

    // -- Misc --
    pub status_notice: Option<String>,
    dirty: bool,
    pub last_copyable_output: Option<String>,

    // -- Blink timer --
    blink_on: bool,
    last_blink: Instant,

    // -- Spinner --
    pub thinking_start: Option<Instant>,
    pub spinner_verb: String,
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

impl AppState {
    /// Create a new `AppState` with sensible defaults for a fresh session.
    pub fn new(session_id: String, provider_id: String, model: String) -> Self {
        Self {
            session_id,
            provider_id,
            model,
            status: Status::Idle,
            permission_mode: PermissionMode::Default,
            output: Vec::new(),
            stream_buffer: String::new(),
            streaming: false,
            scroll: 0,
            stick_to_bottom: true,
            total_lines: 0,
            input: String::new(),
            cursor: 0,
            usage: None,
            context_used: None,
            context_limit: None,
            session_input_tokens: 0,
            session_output_tokens: 0,
            session_cache_creation_tokens: 0,
            session_cache_read_tokens: 0,
            session_turn_count: 0,
            turn_costs: Vec::new(),
            overlay: None,
            overlay_queue: VecDeque::new(),
            overlay_prev_status: None,
            slash_registry: SlashRegistry::extended(vec![]),
            slash_query: None,
            slash_matches: Vec::new(),
            slash_selected: 0,
            slash_page: 0,
            slash_hint: String::new(),
            running_tools: HashMap::new(),
            running_hook_output_idx: None,
            active_hooks: Vec::new(),
            todos: Vec::new(),
            show_full_tool_output: false,
            viewport_width: 0,
            viewport_height: 0,
            should_quit: false,
            last_idle_esc: None,
            last_idle_ctrl_c: None,
            status_notice: None,
            dirty: true,
            last_copyable_output: None,
            blink_on: true,
            last_blink: Instant::now(),
            thinking_start: None,
            spinner_verb: String::new(),
        }
    }

    // -- Dirty flag management ------------------------------------------------

    /// Mark the state as needing a re-render.
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Returns `true` if a re-render is needed.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Clear the dirty flag (typically called after rendering).
    pub fn clear_dirty(&mut self) {
        self.dirty = false;
    }

    // -- Viewport -------------------------------------------------------------

    /// Update the terminal viewport dimensions.
    pub fn resize(&mut self, w: u16, h: u16) {
        self.viewport_width = w;
        self.viewport_height = h;
        self.mark_dirty();
    }

    // -- Blink timer ----------------------------------------------------------

    /// Advance the cursor blink state.  Returns `true` if the blink toggled
    /// (meaning a re-render is needed for the cursor).
    pub fn tick(&mut self) -> bool {
        let now = Instant::now();
        let mut dirty = false;
        if now.duration_since(self.last_blink) >= BLINK_INTERVAL {
            self.blink_on = !self.blink_on;
            self.last_blink = now;
            dirty = true;
        }
        // Re-render while spinner is active (for animation)
        if self.thinking_start.is_some() {
            dirty = true;
        }
        dirty
    }

    /// Whether the cursor blink is currently in the "on" phase.
    pub fn blink_on(&self) -> bool {
        self.blink_on
    }

    // -- Keybinding contexts --------------------------------------------------

    /// Return the set of keybinding contexts that are active right now.
    ///
    /// The keybinding dispatcher uses this to decide which key-map layer(s)
    /// to consult.
    pub fn active_contexts(&self) -> Vec<KeyContext> {
        let mut ctxs = vec![KeyContext::Global];

        if let Some(ref overlay) = self.overlay {
            match overlay {
                OverlayType::HistorySearch(_) => ctxs.push(KeyContext::HistorySearch),
                OverlayType::Help(_) => ctxs.push(KeyContext::Help),
                OverlayType::ToolApproval(_) => ctxs.push(KeyContext::Confirmation),
                // UserInput overlays don't have a dedicated context yet; they
                // inherit Confirmation semantics for now.
                OverlayType::UserInput(_) => ctxs.push(KeyContext::Confirmation),
                OverlayType::MessageSelector(_) => ctxs.push(KeyContext::MessageSelector),
                OverlayType::TurnCost(_) => ctxs.push(KeyContext::Confirmation),
            }
        } else if self.slash_query.is_some() {
            ctxs.push(KeyContext::Autocomplete);
        } else {
            ctxs.push(KeyContext::Chat);
        }

        ctxs
    }

    // -- Slash autocomplete ----------------------------------------------------

    /// Update slash query state based on current input.
    fn update_slash_state(&mut self) {
        if self.input.starts_with('/') {
            let query = &self.input[1..];
            self.slash_query = Some(self.input.clone());
            self.slash_matches = self.slash_registry.matches(query);
            self.slash_selected = 0;
        } else {
            self.slash_query = None;
            self.slash_matches.clear();
        }
    }

    // -- Output helpers ---------------------------------------------------------

    pub fn push_lines(&mut self, lines: Vec<Line<'static>>) {
        if lines.is_empty() {
            return;
        }
        self.output.push(OutputItem::Lines(lines));
        self.stick_to_bottom = true;
        self.mark_dirty();
    }

    pub fn push_block(&mut self, color: DotColor, text: &str) {
        self.output.push(OutputItem::Block {
            color,
            text: text.to_string(),
        });
        self.stick_to_bottom = true;
        self.mark_dirty();
    }

    pub fn push_markdown_block(&mut self, text: &str) {
        self.output.push(OutputItem::Markdown(text.to_string()));
        self.stick_to_bottom = true;
        self.mark_dirty();
    }

    /// Handle a builtin slash command locally (without sending to agent).
    pub fn handle_builtin_command(&mut self, name: &str) {
        let theme = &crate::tui::theme::THEME;
        match name {
            "help" => {
                let hint = self.slash_registry.hint(20);
                self.push_lines(vec![
                    Line::from(Span::styled(
                        "Available commands:",
                        Style::default().fg(theme.text),
                    )),
                    Line::from(Span::styled(hint, Style::default().fg(theme.text_dim))),
                ]);
            }
            "copy" => {
                if self.last_copyable_output.is_some() {
                    self.push_lines(vec![Line::from(Span::styled(
                        "Copied to clipboard.",
                        Style::default().fg(theme.success),
                    ))]);
                } else {
                    self.push_lines(vec![Line::from(Span::styled(
                        "No output to copy.",
                        Style::default().fg(theme.warn),
                    ))]);
                }
            }
            "cost" => {
                let input = self.session_input_tokens;
                let output = self.session_output_tokens;
                let turns = self.session_turn_count;
                self.push_lines(vec![Line::from(Span::styled(
                    format!(
                        "Session: {turns} turns, {input} input tokens, {output} output tokens"
                    ),
                    Style::default().fg(theme.text),
                ))]);
            }
            "status" => {
                let mode = match self.permission_mode {
                    zerobot_core::config::PermissionMode::Default => "default",
                    zerobot_core::config::PermissionMode::Plan => "plan",
                    zerobot_core::config::PermissionMode::AcceptEdits => "auto-edit",
                    zerobot_core::config::PermissionMode::BypassPermissions => "bypass",
                };
                self.push_lines(vec![Line::from(Span::styled(
                    format!(
                        "Model: {} | Provider: {} | Mode: {}",
                        self.model, self.provider_id, mode
                    ),
                    Style::default().fg(theme.text),
                ))]);
            }
            _ => {
                self.push_lines(vec![Line::from(Span::styled(
                    format!("/{name}: not implemented yet"),
                    Style::default().fg(theme.text_muted),
                ))]);
            }
        }
        self.mark_dirty();
    }

    pub fn push_running_tool(&mut self, tool_call_id: &str, label: &str, arguments: &str) {
        self.output.push(OutputItem::ToolRunning {
            label: label.to_string(),
            arguments: arguments.to_string(),
        });
        let idx = self.output.len().saturating_sub(1);
        self.running_tools.insert(
            tool_call_id.to_string(),
            RunningTool {
                output_idx: idx,
                label: label.to_string(),
                arguments: arguments.to_string(),
                start_time: Instant::now(),
            },
        );
        self.stick_to_bottom = true;
        self.mark_dirty();
    }

    pub fn complete_running_tool(&mut self, tool_call_id: &str, name: &str, output: &str, ok: bool) {
        let color = if ok {
            DotColor::Green
        } else {
            DotColor::Red
        };
        if let Some(rt) = self.running_tools.remove(tool_call_id) {
            let duration_ms = Some(rt.start_time.elapsed().as_millis() as u64);
            let item = OutputItem::ToolOutput {
                color,
                tool_name: name.to_string(),
                label: Some(rt.label),
                arguments: rt.arguments,
                output: output.to_string(),
                expanded: false,
                duration_ms,
            };
            if rt.output_idx < self.output.len() {
                self.output[rt.output_idx] = item;
                self.stick_to_bottom = true;
                self.mark_dirty();
                return;
            }
        }
        // Fallback: push as new item
        self.output.push(OutputItem::ToolOutput {
            color,
            tool_name: name.to_string(),
            label: None,
            arguments: String::new(),
            output: output.to_string(),
            expanded: false,
            duration_ms: None,
        });
        self.stick_to_bottom = true;
        self.mark_dirty();
    }

    pub fn push_running_hook(&mut self, label: &str) {
        self.output.push(OutputItem::HookRunning {
            label: label.to_string(),
        });
        self.running_hook_output_idx = Some(self.output.len().saturating_sub(1));
        self.stick_to_bottom = true;
        self.mark_dirty();
    }

    pub fn complete_running_hook(&mut self, ok: bool, label: &str) {
        let item = OutputItem::HookOutput {
            ok,
            label: label.to_string(),
        };
        if let Some(idx) = self.running_hook_output_idx.take() {
            if idx < self.output.len() {
                self.output[idx] = item;
                self.stick_to_bottom = true;
                self.mark_dirty();
                return;
            }
        }
        self.output.push(item);
        self.stick_to_bottom = true;
        self.mark_dirty();
    }

    pub fn append_stream_delta(&mut self, text: &str) {
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
        self.mark_dirty();
    }

    pub fn finalize_stream(&mut self) {
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
        self.mark_dirty();
    }

    // -- Update -----------------------------------------------------------------

    /// Process a Message and return a Command for the event loop to execute.
    pub fn update(&mut self, msg: Message) -> Command {
        match msg {
            // -- Input --
            Message::InputChar(ch) => {
                self.input.insert(self.cursor, ch);
                self.cursor += ch.len_utf8();
                self.update_slash_state();
                self.mark_dirty();
                Command::None
            }
            Message::InputBackspace => {
                if self.cursor > 0 {
                    let prev = self.input[..self.cursor]
                        .char_indices()
                        .last()
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                    self.input.drain(prev..self.cursor);
                    self.cursor = prev;
                    self.update_slash_state();
                    self.mark_dirty();
                }
                Command::None
            }
            Message::InputDelete => {
                if self.cursor < self.input.len() {
                    let next = self.input[self.cursor..]
                        .char_indices()
                        .nth(1)
                        .map(|(i, _)| self.cursor + i)
                        .unwrap_or(self.input.len());
                    self.input.drain(self.cursor..next);
                    self.mark_dirty();
                }
                Command::None
            }
            Message::InputSubmit => {
                if self.input.trim().is_empty() {
                    return Command::None;
                }
                let raw = self.input.trim().to_string();
                self.input.clear();
                self.cursor = 0;
                self.slash_query = None;
                self.slash_matches.clear();

                // Handle slash commands
                if let Some(cmd) = raw.strip_prefix('/') {
                    let cmd_name = cmd.trim().to_lowercase();
                    match cmd_name.as_str() {
                        "exit" | "quit" | "q" => {
                            self.should_quit = true;
                            return Command::Quit;
                        }
                        "clear" => {
                            self.output.clear();
                            self.stream_buffer.clear();
                            self.streaming = false;
                            self.running_tools.clear();
                            self.scroll = 0;
                            self.stick_to_bottom = true;
                            self.mark_dirty();
                            return Command::ClearScreen;
                        }
                        _ => {}
                    }
                    // Look up in slash registry (clone to release borrow on self)
                    if let Some(spec) = self.slash_registry.find(&cmd_name) {
                        let name = spec.name.clone();
                        let kind = spec.kind.clone();
                        match kind {
                            SlashCommandKind::Builtin => {
                                self.handle_builtin_command(&name);
                                return Command::None;
                            }
                            SlashCommandKind::Template(tpl) => {
                                // Echo the command
                                self.push_lines(vec![Line::from(Span::styled(
                                    format!("> /{name}"),
                                    Style::default().fg(
                                        crate::tui::theme::THEME.input_prompt,
                                    ),
                                ))]);
                                self.status = Status::Thinking;
                                self.thinking_start = Some(Instant::now());
                                self.spinner_verb =
                                    SPINNER_VERBS[self.output.len() % SPINNER_VERBS.len()]
                                        .to_string();
                                return Command::SpawnAgent {
                                    prompt: tpl.template,
                                };
                            }
                        }
                    }
                    // Unknown command
                    self.push_lines(vec![Line::from(Span::styled(
                        format!("Unknown command: /{cmd_name}"),
                        Style::default()
                            .fg(crate::tui::theme::THEME.error),
                    ))]);
                    return Command::None;
                }

                // Show user input in output area with ">" prefix
                self.push_lines(vec![Line::from(vec![
                    Span::styled(
                        "> ",
                        Style::default()
                            .fg(crate::tui::theme::THEME.input_prompt),
                    ),
                    Span::raw(raw.clone()),
                ])]);
                self.status = Status::Thinking;
                self.thinking_start = Some(Instant::now());
                self.spinner_verb =
                    SPINNER_VERBS[self.output.len() % SPINNER_VERBS.len()].to_string();
                Command::SpawnAgent { prompt: raw }
            }
            Message::InputMoveCursor(delta) => {
                if delta > 0 {
                    for _ in 0..delta {
                        if self.cursor < self.input.len() {
                            let next = self.input[self.cursor..]
                                .char_indices()
                                .nth(1)
                                .map(|(i, _)| self.cursor + i)
                                .unwrap_or(self.input.len());
                            self.cursor = next;
                        }
                    }
                } else {
                    for _ in 0..(-delta) {
                        if self.cursor > 0 {
                            let prev = self.input[..self.cursor]
                                .char_indices()
                                .last()
                                .map(|(i, _)| i)
                                .unwrap_or(0);
                            self.cursor = prev;
                        }
                    }
                }
                self.mark_dirty();
                Command::None
            }
            Message::InputClear => {
                self.input.clear();
                self.cursor = 0;
                self.mark_dirty();
                Command::None
            }
            Message::InputDeleteWord => {
                if self.cursor > 0 {
                    let before = &self.input[..self.cursor];
                    let trimmed = before.trim_end();
                    let last_space = trimmed.rfind(' ').map(|i| i + 1).unwrap_or(0);
                    self.input.drain(last_space..self.cursor);
                    self.cursor = last_space;
                    self.mark_dirty();
                }
                Command::None
            }
            Message::InputDeleteToEnd => {
                self.input.truncate(self.cursor);
                self.mark_dirty();
                Command::None
            }
            Message::CursorToStart => {
                self.cursor = 0;
                self.mark_dirty();
                Command::None
            }
            Message::CursorToEnd => {
                self.cursor = self.input.len();
                self.mark_dirty();
                Command::None
            }
            Message::InputPaste(text) => {
                for ch in text.chars() {
                    self.input.insert(self.cursor, ch);
                    self.cursor += ch.len_utf8();
                }
                self.mark_dirty();
                Command::None
            }

            // -- Scroll --
            Message::ScrollUp => {
                self.stick_to_bottom = false;
                self.scroll = self.scroll.saturating_sub(1);
                self.mark_dirty();
                Command::None
            }
            Message::ScrollDown => {
                self.scroll = self.scroll.saturating_add(1);
                self.mark_dirty();
                Command::None
            }
            Message::ScrollToTop => {
                self.scroll = 0;
                self.stick_to_bottom = false;
                self.mark_dirty();
                Command::None
            }
            Message::ScrollToBottom => {
                self.scroll = self
                    .total_lines
                    .saturating_sub(self.viewport_height as usize) as u16;
                self.stick_to_bottom = true;
                self.mark_dirty();
                Command::None
            }
            Message::StickToBottom => {
                self.stick_to_bottom = !self.stick_to_bottom;
                if self.stick_to_bottom {
                    self.scroll = self
                        .total_lines
                        .saturating_sub(self.viewport_height as usize)
                        as u16;
                }
                self.mark_dirty();
                Command::None
            }

            // -- App-level --
            Message::Quit => {
                self.should_quit = true;
                Command::Quit
            }
            Message::Interrupt => {
                // Event loop handles interrupt directly
                Command::None
            }
            Message::ClearScreen => {
                self.output.clear();
                self.stream_buffer.clear();
                self.streaming = false;
                self.running_tools.clear();
                self.scroll = 0;
                self.stick_to_bottom = true;
                self.mark_dirty();
                Command::ClearScreen
            }
            Message::Redraw => {
                self.mark_dirty();
                Command::None
            }
            Message::CyclePermissionMode => {
                self.permission_mode = match self.permission_mode {
                    PermissionMode::Default => PermissionMode::Plan,
                    PermissionMode::Plan => PermissionMode::AcceptEdits,
                    PermissionMode::AcceptEdits => PermissionMode::Default,
                    PermissionMode::BypassPermissions => PermissionMode::Default,
                };
                self.mark_dirty();
                Command::None
            }
            Message::ToggleFullToolOutput => {
                self.show_full_tool_output = !self.show_full_tool_output;
                self.mark_dirty();
                Command::None
            }
            Message::ShowTurnCost => {
                // Show turn cost overlay (handled via overlay system)
                Command::None
            }
            Message::ShowHelp => {
                // Show help overlay (handled via overlay system)
                Command::None
            }

            // -- Agent events --
            Message::AgentDelta(delta) => {
                self.append_stream_delta(&delta);
                Command::None
            }
            Message::AgentMessage(content) => {
                self.finalize_stream();
                self.push_markdown_block(&content);
                if !content.trim().is_empty() {
                    self.last_copyable_output = Some(content);
                }
                Command::None
            }
            Message::ToolStarted {
                tool_call_id,
                name,
                input,
            } => {
                self.finalize_stream();
                let label = format_tool_label(&name, &input, self.viewport_width);
                self.push_running_tool(&tool_call_id, &label, &input);
                self.status = Status::Tool(label);
                Command::None
            }
            Message::ToolFinished {
                tool_call_id,
                name,
                output,
                ok,
            } => {
                self.complete_running_tool(&tool_call_id, &name, output.trim(), ok);
                if self.running_tools.is_empty() {
                    self.status = Status::Thinking;
                }
                Command::None
            }
            Message::ToolBatchStarted { .. } => {
                // No special handling needed
                Command::None
            }
            Message::AgentDone => {
                self.finalize_stream();
                self.status = Status::Idle;
                self.thinking_start = None;
                Command::None
            }
            Message::AgentError(err) => {
                self.finalize_stream();
                self.status = Status::Error(err.clone());
                self.thinking_start = None;
                self.push_block(DotColor::Red, &err);
                Command::None
            }
            Message::SessionCost {
                input_tokens,
                output_tokens,
                cache_creation_tokens,
                cache_read_tokens,
                turn_count,
            } => {
                if turn_count > self.session_turn_count && self.session_turn_count > 0 {
                    let turn_input = input_tokens.saturating_sub(self.session_input_tokens);
                    let turn_output = output_tokens.saturating_sub(self.session_output_tokens);
                    let turn_cache_creation =
                        cache_creation_tokens.saturating_sub(self.session_cache_creation_tokens);
                    let turn_cache_read =
                        cache_read_tokens.saturating_sub(self.session_cache_read_tokens);
                    self.turn_costs.push(TurnCost {
                        input_tokens: turn_input,
                        output_tokens: turn_output,
                        cache_creation: turn_cache_creation,
                        cache_read: turn_cache_read,
                    });
                }
                self.session_input_tokens = input_tokens;
                self.session_output_tokens = output_tokens;
                self.session_cache_creation_tokens = cache_creation_tokens;
                self.session_cache_read_tokens = cache_read_tokens;
                self.session_turn_count = turn_count;
                self.mark_dirty();
                Command::None
            }
            Message::ContextUsage { used, limit } => {
                self.context_used = Some(used);
                self.context_limit = limit;
                self.mark_dirty();
                Command::None
            }
            Message::PermissionDenied {
                tool_name,
                reason,
                permission_reason,
            } => {
                let mut msg = format!("权限拒绝: {tool_name} - {reason}");
                if let Some(pr) = permission_reason {
                    msg.push_str(&format!(" ({pr})"));
                }
                self.push_block(DotColor::Yellow, &msg);
                Command::None
            }
            Message::HookStarted {
                event,
                hook_name,
                status_message,
            } => {
                self.finalize_stream();
                let label = status_message.unwrap_or(hook_name.clone());
                self.push_running_hook(&label);
                self.active_hooks.push(hook_name);
                self.status = Status::Hook(event);
                Command::None
            }
            Message::HookFinished {
                hook_name,
                ok,
                message,
                ..
            } => {
                let label = message.unwrap_or(hook_name.clone());
                self.complete_running_hook(ok, &label);
                self.active_hooks.retain(|h| h != &hook_name);
                if self.active_hooks.is_empty() && self.running_tools.is_empty() {
                    self.status = Status::Thinking;
                }
                Command::None
            }
            Message::PluginWarning {
                plugin,
                hook,
                message,
                degraded,
            } => {
                let text = if degraded {
                    format!("插件降级: plugin={plugin}, hook={hook}, message={message}")
                } else {
                    format!("插件告警: plugin={plugin}, hook={hook}, message={message}")
                };
                self.push_block(DotColor::Yellow, &text);
                Command::None
            }
            Message::SelfReviewCompleted {
                summary,
                memory_changes,
                skill_changes,
            } => {
                self.push_block(
                    DotColor::Green,
                    &format!(
                        "自检完成: {summary} (记忆:{memory_changes}, 技能:{skill_changes})"
                    ),
                );
                Command::None
            }

            // -- Overlay --
            Message::ShowOverlay(_kind) => {
                // Overlay display is handled by the event loop
                Command::None
            }
            Message::CloseOverlay => {
                self.overlay = None;
                if let Some(prev) = self.overlay_prev_status.take() {
                    self.status = prev;
                }
                self.mark_dirty();
                Command::None
            }
            Message::OverlaySelect
            | Message::OverlayConfirm
            | Message::OverlayCancel
            | Message::OverlayNextField
            | Message::OverlayInput(_) => {
                // These are handled by the overlay's own handle_key
                Command::None
            }

            // -- Slash --
            Message::SlashQuery(_) => {
                // Slash state is managed by update_slash_state()
                Command::None
            }
            Message::SlashSelect => {
                if let Some(selected) = self.slash_matches.get(self.slash_selected) {
                    let cmd = format!("/{}", selected.name);
                    self.input = cmd;
                    self.cursor = self.input.len();
                    self.slash_query = None;
                    self.slash_matches.clear();
                    self.mark_dirty();
                }
                Command::None
            }
            Message::SlashExecute(cmd) => {
                self.slash_query = None;
                self.slash_matches.clear();
                Command::SpawnAgent { prompt: cmd }
            }
            Message::SlashPage(delta) => {
                if !self.slash_matches.is_empty() {
                    let len = self.slash_matches.len();
                    if delta > 0 {
                        self.slash_selected = (self.slash_selected + 1) % len;
                    } else if delta < 0 {
                        self.slash_selected = if self.slash_selected == 0 {
                            len - 1
                        } else {
                            self.slash_selected - 1
                        };
                    }
                    self.mark_dirty();
                }
                Command::None
            }

            // -- History --
            Message::HistorySearch(_) => Command::None,
            Message::HistorySelect => Command::None,

            // -- Session --
            Message::SessionLoaded { session_id } => {
                self.session_id = session_id;
                self.mark_dirty();
                Command::None
            }
            Message::RewindTo { message_id } => Command::RewindTo {
                message_id,
                input: String::new(),
            },

            Message::Noop => Command::None,
        }
    }
}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

fn format_tool_label(name: &str, args: &str, width: u16) -> String {
    let max_args = (width as usize).saturating_sub(name.len() + 10).max(20);
    let short_args = if args.chars().count() > max_args {
        let truncated: String = args.chars().take(max_args).collect();
        format!("{truncated}...")
    } else {
        args.to_string()
    };
    format!("{name}({short_args})")
}
