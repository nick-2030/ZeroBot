/// Side-effects produced by the TUI update loop.
///
/// Commands are returned from `AppState::update` and executed by the
/// event loop driver. They represent I/O operations that cannot happen
/// inside a pure state update.
#[derive(Debug, Clone)]
pub enum Command {
    /// Do nothing.
    None,
    /// Send a prompt to the agent for a new turn.
    SpawnAgent { prompt: String },
    /// Quit the application.
    Quit,
    /// Clear the terminal screen.
    ClearScreen,
    /// Copy text to the system clipboard.
    CopyToClipboard(String),
    /// Open the user's preferred external editor for multi-line input.
    OpenExternalEditor,
    /// Resume an existing session by its ID.
    ResumeSession { session_id: String },
    /// Rewind the conversation to a specific message and optionally re-submit.
    RewindTo {
        message_id: String,
        input: String,
    },
}
