//! Core types for the keybinding system.
//!
//! `KeyContext` identifies the current input mode (chat, autocomplete, overlay,
//! etc.) so the dispatcher can select the right key-map layer.
//!
//! `KeyAction` enumerates every semantic action that a key-press can trigger.
//!
//! `KeyCombo` is a thin wrapper around `(KeyCode, KeyModifiers)` that
//! implements `Hash` and `Eq` so it can be used as a `HashMap` key.

use std::fmt;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

// ---------------------------------------------------------------------------
// KeyContext
// ---------------------------------------------------------------------------

/// Identifies the active keybinding context layer(s).
///
/// Multiple contexts can be active simultaneously (e.g. `Global` + `Chat`).
/// The dispatcher resolves in priority order: later contexts override earlier
/// ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyContext {
    Global,
    Chat,
    Autocomplete,
    Confirmation,
    HistorySearch,
    Scroll,
    MessageSelector,
    Help,
    Select,
}

// ---------------------------------------------------------------------------
// KeyAction
// ---------------------------------------------------------------------------

/// A semantic action triggered by a key combination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyAction {
    // -- Global --
    Interrupt,
    Exit,
    Redraw,
    ToggleTodos,
    ToggleTranscript,
    CycleMode,
    ShowHelp,

    // -- Chat / input --
    Cancel,
    Submit,
    Undo,
    ExternalEditor,
    Stash,
    ImagePaste,

    // -- History --
    HistoryPrevious,
    HistoryNext,
    HistorySearch,

    // -- Scroll --
    PageUp,
    PageDown,
    ScrollToTop,
    ScrollToBottom,
    LineUp,
    LineDown,
    CopySelection,

    // -- Autocomplete --
    AutocompleteAccept,
    AutocompleteDismiss,
    AutocompletePrevious,
    AutocompleteNext,

    // -- Confirmation overlay --
    ConfirmYes,
    ConfirmNo,
    ConfirmPrevious,
    ConfirmNext,
    ConfirmToggle,
    ConfirmNextField,

    // -- Message selector --
    SelectorUp,
    SelectorDown,
    SelectorTop,
    SelectorBottom,
    SelectorSelect,

    // -- Generic select (pick-list) --
    SelectPrevious,
    SelectNext,
    SelectAccept,
    SelectCancel,

    /// An action identified by a custom string (for user-defined bindings).
    Custom(String),
}

// ---------------------------------------------------------------------------
// KeyCombo
// ---------------------------------------------------------------------------

/// A single key combination: a key code plus modifier flags.
///
/// This is the unit that gets looked up in the binding tables.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct KeyCombo {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

impl KeyCombo {
    /// Build a `KeyCombo` from a crossterm `KeyEvent`.
    pub fn from_event(key: KeyEvent) -> Self {
        Self {
            code: key.code,
            modifiers: key.modifiers,
        }
    }

    /// Build a `KeyCombo` from raw parts.
    pub fn new(code: KeyCode, modifiers: KeyModifiers) -> Self {
        Self { code, modifiers }
    }
}

impl fmt::Display for KeyCombo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.modifiers.contains(KeyModifiers::CONTROL) {
            write!(f, "C-")?;
        }
        if self.modifiers.contains(KeyModifiers::SHIFT) {
            write!(f, "S-")?;
        }
        if self.modifiers.contains(KeyModifiers::ALT) {
            write!(f, "A-")?;
        }
        match self.code {
            KeyCode::Char(c) => write!(f, "{c}"),
            KeyCode::Enter => write!(f, "Enter"),
            KeyCode::Esc => write!(f, "Esc"),
            KeyCode::Tab => write!(f, "Tab"),
            KeyCode::BackTab => write!(f, "S-Tab"),
            KeyCode::Up => write!(f, "Up"),
            KeyCode::Down => write!(f, "Down"),
            KeyCode::Left => write!(f, "Left"),
            KeyCode::Right => write!(f, "Right"),
            KeyCode::Home => write!(f, "Home"),
            KeyCode::End => write!(f, "End"),
            KeyCode::PageUp => write!(f, "PageUp"),
            KeyCode::PageDown => write!(f, "PageDown"),
            KeyCode::Backspace => write!(f, "Backspace"),
            KeyCode::Delete => write!(f, "Delete"),
            KeyCode::Insert => write!(f, "Insert"),
            KeyCode::F(n) => write!(f, "F{n}"),
            other => write!(f, "{other:?}"),
        }
    }
}

// ---------------------------------------------------------------------------
// ChordState
// ---------------------------------------------------------------------------

/// Tracks an in-progress chord sequence (e.g. `Ctrl+X` followed by `Ctrl+K`).
pub struct ChordState {
    /// The first key of the chord that was already pressed.
    pub prefix: KeyCombo,
    /// When the prefix was pressed, used for timeout detection.
    pub timestamp: Instant,
}
