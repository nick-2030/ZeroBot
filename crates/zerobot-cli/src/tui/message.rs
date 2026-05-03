use zerobot_core::events::AgentEvent;

/// All messages that flow through the TUI event loop.
///
/// Messages are produced by components, input handlers, and agent events,
/// then consumed by the `AppState` update logic to produce `Command` values.
#[derive(Debug, Clone)]
pub enum Message {
    // ── Input ──────────────────────────────────────────────────────────
    /// Insert a character at the cursor position.
    InputChar(char),
    /// Delete the character before the cursor.
    InputBackspace,
    /// Delete the character at the cursor.
    InputDelete,
    /// Submit the current input.
    InputSubmit,
    /// Move the cursor by a signed offset (positive = right).
    InputMoveCursor(i32),
    /// Clear all input text.
    InputClear,
    /// Delete the word before the cursor (Ctrl+W / Alt+Backspace).
    InputDeleteWord,
    /// Delete from cursor to end of line (Ctrl+K).
    InputDeleteToEnd,
    /// Move cursor to start of input (Ctrl+A / Home).
    CursorToStart,
    /// Move cursor to end of input (Ctrl+E / End).
    CursorToEnd,
    /// Paste text into the input.
    InputPaste(String),

    // ── Scroll ─────────────────────────────────────────────────────────
    /// Scroll the conversation view up by one page/line.
    ScrollUp,
    /// Scroll the conversation view down by one page/line.
    ScrollDown,
    /// Jump to the top of the conversation.
    ScrollToTop,
    /// Jump to the bottom of the conversation.
    ScrollToBottom,
    /// Toggle stick-to-bottom auto-scroll behavior.
    StickToBottom,

    // ── App-level ──────────────────────────────────────────────────────
    /// Quit the application.
    Quit,
    /// Interrupt the running agent.
    Interrupt,
    /// Clear the screen and re-render.
    ClearScreen,
    /// Force a full redraw.
    Redraw,
    /// Cycle through permission modes (auto / ask / deny).
    CyclePermissionMode,
    /// Toggle showing full tool output vs. collapsed.
    ToggleFullToolOutput,
    /// Show/hide the per-turn cost overlay.
    ShowTurnCost,
    /// Show the help overlay.
    ShowHelp,

    // ── Agent events ───────────────────────────────────────────────────
    /// Streaming text delta from the agent.
    AgentDelta(String),
    /// Complete assistant message from the agent.
    AgentMessage(String),
    /// A tool call has started.
    ToolStarted {
        tool_call_id: String,
        name: String,
        input: String,
    },
    /// A tool call has finished.
    ToolFinished {
        tool_call_id: String,
        name: String,
        output: String,
        ok: bool,
    },
    /// A batch of tool calls has started (parallel execution).
    ToolBatchStarted {
        tool_call_ids: Vec<String>,
        parallel: bool,
    },
    /// The agent turn has completed.
    AgentDone,
    /// An error occurred during the agent turn.
    AgentError(String),
    /// Session cost information (tokens used, cache stats).
    SessionCost {
        input_tokens: u64,
        output_tokens: u64,
        cache_creation_tokens: u64,
        cache_read_tokens: u64,
        turn_count: u32,
    },
    /// Context window usage update.
    ContextUsage {
        used: usize,
        limit: Option<u32>,
    },
    /// A tool call was denied by the permission system.
    PermissionDenied {
        tool_name: String,
        reason: String,
        permission_reason: Option<String>,
    },
    /// A hook has started executing.
    HookStarted {
        event: String,
        hook_name: String,
        status_message: Option<String>,
    },
    /// A hook has finished executing.
    HookFinished {
        event: String,
        hook_name: String,
        ok: bool,
        message: Option<String>,
    },
    /// A plugin produced a warning.
    PluginWarning {
        plugin: String,
        hook: String,
        message: String,
        degraded: bool,
    },
    /// Self-review completed after a turn.
    SelfReviewCompleted {
        summary: String,
        memory_changes: usize,
        skill_changes: usize,
    },

    // ── Overlay ────────────────────────────────────────────────────────
    /// Show an overlay with the given name.
    ShowOverlay(OverlayKind),
    /// Close the currently open overlay.
    CloseOverlay,
    /// Select the next item in the overlay.
    OverlaySelect,
    /// Confirm the current overlay selection.
    OverlayConfirm,
    /// Cancel/close the overlay.
    OverlayCancel,
    /// Move to the next focusable field in the overlay.
    OverlayNextField,
    /// Input text within an overlay field.
    OverlayInput(String),

    // ── Slash commands ─────────────────────────────────────────────────
    /// Trigger a slash command query (e.g. "/" typed).
    SlashQuery(String),
    /// Select a slash command from the list.
    SlashSelect,
    /// Execute the selected slash command.
    SlashExecute(String),
    /// Page through slash command results.
    SlashPage(i32),

    // ── History ────────────────────────────────────────────────────────
    /// Search through input history.
    HistorySearch(String),
    /// Select a history entry.
    HistorySelect,

    // ── Session ────────────────────────────────────────────────────────
    /// A session has been loaded (e.g. on resume).
    SessionLoaded { session_id: String },
    /// Rewind the conversation to a specific message.
    RewindTo { message_id: String },

    // ── Misc ───────────────────────────────────────────────────────────
    /// No-op message; ignored by the update loop.
    Noop,
}

/// Identifies which overlay to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlayKind {
    Help,
    TurnCost,
    Permission,
    SlashCommand,
    History,
    Rewind,
    ConfirmInterrupt,
    Settings,
}

impl Message {
    /// Convert an `AgentEvent` from `zerobot-core` into the corresponding TUI `Message`.
    pub fn from_agent_event(event: AgentEvent) -> Self {
        match event {
            AgentEvent::AssistantDelta { content } => Message::AgentDelta(content),
            AgentEvent::AssistantMessage { content } => Message::AgentMessage(content),
            AgentEvent::ToolCallStarted {
                tool_call_id,
                name,
                input,
            } => Message::ToolStarted {
                tool_call_id,
                name,
                input,
            },
            AgentEvent::ToolCallFinished {
                tool_call_id,
                name,
                output,
                ok,
            } => Message::ToolFinished {
                tool_call_id,
                name,
                output,
                ok,
            },
            AgentEvent::ToolBatchStarted {
                tool_call_ids,
                parallel,
            } => Message::ToolBatchStarted {
                tool_call_ids,
                parallel,
            },
            AgentEvent::Done => Message::AgentDone,
            AgentEvent::Error { message } => Message::AgentError(message),
            AgentEvent::SessionCost {
                input_tokens,
                output_tokens,
                cache_creation_tokens,
                cache_read_tokens,
                turn_count,
            } => Message::SessionCost {
                input_tokens,
                output_tokens,
                cache_creation_tokens,
                cache_read_tokens,
                turn_count,
            },
            AgentEvent::ContextUsage { used, limit } => {
                Message::ContextUsage { used, limit }
            }
            AgentEvent::PermissionDenied {
                tool_name,
                reason,
                permission_reason,
            } => Message::PermissionDenied {
                tool_name,
                reason,
                permission_reason,
            },
            AgentEvent::HookStarted {
                event,
                hook_name,
                status_message,
            } => Message::HookStarted {
                event,
                hook_name,
                status_message,
            },
            AgentEvent::HookFinished {
                event,
                hook_name,
                ok,
                message,
            } => Message::HookFinished {
                event,
                hook_name,
                ok,
                message,
            },
            AgentEvent::PluginWarning {
                plugin,
                hook,
                message,
                degraded,
            } => Message::PluginWarning {
                plugin,
                hook,
                message,
                degraded,
            },
            AgentEvent::SelfReviewCompleted {
                summary,
                memory_changes,
                skill_changes,
            } => Message::SelfReviewCompleted {
                summary,
                memory_changes,
                skill_changes,
            },
            // Events that don't map to a specific TUI message become Noop.
            AgentEvent::SessionStarted { .. }
            | AgentEvent::SessionResumed { .. }
            | AgentEvent::UserMessage { .. }
            | AgentEvent::Usage { .. }
            | AgentEvent::CwdChanged { .. }
            | AgentEvent::FileChanged { .. }
            | AgentEvent::HookSessionAdded { .. }
            | AgentEvent::HookSessionRemoved { .. }
            | AgentEvent::Stop => Message::Noop,
        }
    }
}
