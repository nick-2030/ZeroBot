//! Default keybinding tables.
//!
//! Each context maps a set of `KeyCombo -> KeyAction` pairs.  The
//! `KeybindingManager` merges these at construction time and allows runtime
//! overrides.

use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyModifiers};

use super::types::{KeyAction, KeyCombo, KeyContext};

/// Build the default keybinding table.
///
/// Returns a map from context to its bindings.  Contexts with no bindings are
/// omitted.
pub fn default_bindings() -> HashMap<KeyContext, HashMap<KeyCombo, KeyAction>> {
    let mut table: HashMap<KeyContext, HashMap<KeyCombo, KeyAction>> = HashMap::new();

    // -- Helpers ---------------------------------------------------------------

    /// Shorthand: `KeyCombo` from a `KeyCode` with no modifiers.
    fn bare(code: KeyCode) -> KeyCombo {
        KeyCombo::new(code, KeyModifiers::NONE)
    }

    /// Shorthand: `KeyCombo` from a `KeyCode` with Ctrl held.
    fn ctrl(code: KeyCode) -> KeyCombo {
        KeyCombo::new(code, KeyModifiers::CONTROL)
    }

    // -- Global ----------------------------------------------------------------

    {
        let m = table.entry(KeyContext::Global).or_default();
        m.insert(ctrl(KeyCode::Char('c')), KeyAction::Interrupt);
        m.insert(ctrl(KeyCode::Char('d')), KeyAction::Exit);
        m.insert(ctrl(KeyCode::Char('l')), KeyAction::Redraw);
        m.insert(ctrl(KeyCode::Char('t')), KeyAction::ToggleTodos);
        m.insert(ctrl(KeyCode::Char('o')), KeyAction::ToggleTranscript);
        m.insert(ctrl(KeyCode::Char('r')), KeyAction::HistorySearch);
        m.insert(ctrl(KeyCode::Char('h')), KeyAction::ShowHelp);
    }

    // -- Chat / main input ----------------------------------------------------

    {
        let m = table.entry(KeyContext::Chat).or_default();
        m.insert(bare(KeyCode::Esc), KeyAction::Cancel);
        m.insert(bare(KeyCode::Enter), KeyAction::Submit);
        m.insert(bare(KeyCode::Up), KeyAction::HistoryPrevious);
        m.insert(bare(KeyCode::Down), KeyAction::HistoryNext);
        m.insert(bare(KeyCode::BackTab), KeyAction::CycleMode);
        // Ctrl+_ (underscore) is ASCII 0x1F — crossterm delivers it as
        // KeyCode::Char('_') with CONTROL.
        m.insert(ctrl(KeyCode::Char('_')), KeyAction::Undo);
        m.insert(ctrl(KeyCode::Char('g')), KeyAction::ExternalEditor);
        m.insert(ctrl(KeyCode::Char('s')), KeyAction::Stash);
        m.insert(ctrl(KeyCode::Char('v')), KeyAction::ImagePaste);
    }

    // -- Autocomplete (slash-command picker) -----------------------------------

    {
        let m = table.entry(KeyContext::Autocomplete).or_default();
        m.insert(bare(KeyCode::Tab), KeyAction::AutocompleteAccept);
        m.insert(bare(KeyCode::Esc), KeyAction::AutocompleteDismiss);
        m.insert(bare(KeyCode::Up), KeyAction::AutocompletePrevious);
        m.insert(bare(KeyCode::Down), KeyAction::AutocompleteNext);
    }

    // -- Confirmation overlay --------------------------------------------------

    {
        let m = table.entry(KeyContext::Confirmation).or_default();
        m.insert(bare(KeyCode::Char('y')), KeyAction::ConfirmYes);
        m.insert(bare(KeyCode::Enter), KeyAction::ConfirmYes);
        m.insert(bare(KeyCode::Char('n')), KeyAction::ConfirmNo);
        m.insert(bare(KeyCode::Esc), KeyAction::ConfirmNo);
        m.insert(bare(KeyCode::Up), KeyAction::ConfirmPrevious);
        m.insert(bare(KeyCode::Down), KeyAction::ConfirmNext);
        m.insert(bare(KeyCode::Tab), KeyAction::ConfirmNextField);
    }

    // -- History search overlay -----------------------------------------------

    {
        let m = table.entry(KeyContext::HistorySearch).or_default();
        m.insert(bare(KeyCode::Esc), KeyAction::SelectCancel);
        m.insert(bare(KeyCode::Enter), KeyAction::SelectAccept);
    }

    // -- Scroll mode -----------------------------------------------------------

    {
        let m = table.entry(KeyContext::Scroll).or_default();
        m.insert(bare(KeyCode::PageUp), KeyAction::PageUp);
        m.insert(bare(KeyCode::PageDown), KeyAction::PageDown);
        m.insert(ctrl(KeyCode::Home), KeyAction::ScrollToTop);
        m.insert(ctrl(KeyCode::End), KeyAction::ScrollToBottom);
    }

    // -- Message selector overlay ----------------------------------------------

    {
        let m = table.entry(KeyContext::MessageSelector).or_default();
        m.insert(bare(KeyCode::Char('j')), KeyAction::SelectorDown);
        m.insert(bare(KeyCode::Char('k')), KeyAction::SelectorUp);
        m.insert(bare(KeyCode::Up), KeyAction::SelectorUp);
        m.insert(bare(KeyCode::Down), KeyAction::SelectorDown);
        m.insert(
            KeyCombo::new(KeyCode::Up, KeyModifiers::CONTROL),
            KeyAction::SelectorTop,
        );
        m.insert(
            KeyCombo::new(KeyCode::Down, KeyModifiers::CONTROL),
            KeyAction::SelectorBottom,
        );
        m.insert(bare(KeyCode::Enter), KeyAction::SelectorSelect);
    }

    // -- Help overlay ----------------------------------------------------------

    {
        let m = table.entry(KeyContext::Help).or_default();
        m.insert(bare(KeyCode::Esc), KeyAction::SelectCancel);
        m.insert(bare(KeyCode::Char('q')), KeyAction::SelectCancel);
    }

    // -- Generic select (pick-list) -------------------------------------------

    {
        let m = table.entry(KeyContext::Select).or_default();
        m.insert(bare(KeyCode::Up), KeyAction::SelectPrevious);
        m.insert(bare(KeyCode::Down), KeyAction::SelectNext);
        m.insert(bare(KeyCode::Enter), KeyAction::SelectAccept);
        m.insert(bare(KeyCode::Esc), KeyAction::SelectCancel);
    }

    table
}
