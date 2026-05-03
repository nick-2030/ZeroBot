//! Application state for the TUI.
//!
//! Contains all mutable state that drives the UI, migrated from the legacy
//! monolithic `tui.rs`.  Rendering components read `AppState` immutably;
//! event handlers mutate it.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use ratatui::text::Line;
use zerobot_core::config::PermissionMode;
use zerobot_core::provider::TokenUsage;
use zerobot_core::session::TodoItem;

use crate::slash::SlashMatch;
use crate::tui::overlay::OverlayType;

// ---------------------------------------------------------------------------
// Supporting types (migrated from legacy.rs)
// ---------------------------------------------------------------------------

/// Blink interval for the cursor.
const BLINK_INTERVAL: Duration = Duration::from_millis(500);

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
// Keybinding context (temporary — will be moved to keybindings/types.rs in Task 4)
// ---------------------------------------------------------------------------

/// Describes the current keybinding context so the dispatcher can select the
/// right key-map layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyContext {
    Global,
    Chat,
    Autocomplete,
    HistorySearch,
    Help,
    Confirmation,
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
        if now.duration_since(self.last_blink) >= BLINK_INTERVAL {
            self.blink_on = !self.blink_on;
            self.last_blink = now;
            true
        } else {
            false
        }
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
                // MessageSelector and TurnCost use Confirmation context for now.
                OverlayType::MessageSelector(_) | OverlayType::TurnCost(_) => {
                    ctxs.push(KeyContext::Confirmation)
                }
            }
        } else if self.slash_query.is_some() {
            ctxs.push(KeyContext::Autocomplete);
        } else {
            ctxs.push(KeyContext::Chat);
        }

        ctxs
    }
}
