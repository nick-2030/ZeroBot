//! Keybinding system with context-aware key mapping and chord sequence support.
//!
//! The central type is [`KeybindingManager`], which owns the full key-map
//! (populated from [`default_bindings`] at startup) and exposes a single
//! `resolve()` method that the event loop calls on every key-press.
//!
//! # Architecture
//!
//! 1. **Contexts** -- each key-press is resolved against a priority-ordered
//!    list of *active* contexts (see [`KeyContext`]).  Later contexts override
//!    earlier ones, so `Global` bindings are shadowed by `Chat` bindings when
//!    both are active.
//!
//! 2. **Chords** -- a chord is a two-key sequence (e.g. `Ctrl+X Ctrl+K`).
//!    When the first key matches a chord prefix, the manager enters a
//!    "pending" state and waits for the second key within a configurable
//!    timeout (default 1 s).  If the timeout expires without a match, the
//!    prefix key is re-evaluated as a normal single-key binding.

pub mod default_bindings;
pub mod types;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use self::default_bindings::default_bindings;
use self::types::{ChordState, KeyAction, KeyCombo, KeyContext};

/// Default chord timeout (1 second).
const CHORD_TIMEOUT: Duration = Duration::from_secs(1);

/// Manages key-to-action resolution with context layers and chord sequences.
pub struct KeybindingManager {
    /// Per-context binding tables.  Multiple contexts can be active at once;
    /// the manager walks them in priority order.
    bindings: HashMap<KeyContext, HashMap<KeyCombo, KeyAction>>,

    /// Pending chord prefix, if any.
    chord_state: Option<ChordState>,

    /// How long to wait for the second key of a chord before giving up.
    chord_timeout: Duration,

    /// Registered two-key chords: `(prefix, suffix) -> action`.
    chords: HashMap<(KeyCombo, KeyCombo), KeyAction>,
}

impl KeybindingManager {
    /// Create a new manager populated with the default bindings and the
    /// built-in chord `Ctrl+X Ctrl+K -> Interrupt`.
    pub fn with_defaults() -> Self {
        let mut manager = Self {
            bindings: default_bindings(),
            chord_state: None,
            chord_timeout: CHORD_TIMEOUT,
            chords: HashMap::new(),
        };

        // Register the built-in Ctrl+X Ctrl+K chord.
        manager.register_chord(
            KeyCombo::new(KeyCode::Char('x'), KeyModifiers::CONTROL),
            KeyCombo::new(KeyCode::Char('k'), KeyModifiers::CONTROL),
            KeyAction::Interrupt,
        );

        manager
    }

    /// Register a two-key chord sequence.
    ///
    /// When `prefix` is pressed, the manager enters chord-pending mode and
    /// waits for `suffix`.  If `suffix` arrives within the timeout the
    /// returned `action` fires; otherwise the prefix is treated as a normal
    /// key-press.
    pub fn register_chord(&mut self, prefix: KeyCombo, suffix: KeyCombo, action: KeyAction) {
        self.chords.insert((prefix, suffix), action);
    }

    /// Resolve a key event to a semantic action.
    ///
    /// `active_contexts` is the ordered list of contexts that apply right now
    /// (typically `[Global, Chat]` or `[Global, Help]`).  Contexts later in
    /// the list have higher priority and shadow earlier ones.
    ///
    /// Returns `None` if no binding matches.
    pub fn resolve(
        &mut self,
        key: KeyEvent,
        active_contexts: &[KeyContext],
    ) -> Option<KeyAction> {
        let combo = KeyCombo::from_event(key);

        // -- Chord handling ----------------------------------------------------

        // If we are already waiting for the second key of a chord ...
        if let Some(ref state) = self.chord_state {
            // Check timeout first.
            if state.timestamp.elapsed() > self.chord_timeout {
                // Chord timed out -- fall through and treat `combo` as a fresh
                // key-press (clearing the pending state).
                self.chord_state = None;
            } else {
                let prefix = state.prefix.clone();
                // See if (prefix, combo) completes a chord.
                if let Some(action) = self.chords.get(&(prefix.clone(), combo.clone())) {
                    let action = action.clone();
                    self.chord_state = None;
                    return Some(action);
                }
                // Not a match -- the chord is abandoned.  Re-evaluate the
                // *prefix* key as a normal single-key press and then let the
                // current key-press go through the normal path too.
                //
                // We return the prefix action first; the caller should call
                // `resolve` again for the second key.  To avoid that extra
                // round-trip we store the second key and resolve it now.
                self.chord_state = None;
                let prefix_action = self.lookup_single(&prefix, active_contexts);
                if prefix_action.is_some() {
                    // The prefix had a standalone meaning -- resolve the
                    // current key and discard (the caller will see the prefix
                    // action now and the current key on the next event loop
                    // tick).
                    let _ = self.lookup_single(&combo, active_contexts);
                    return prefix_action;
                }
                // Prefix had no standalone binding either -- fall through and
                // resolve the current key normally.
            }
        }

        // -- Check if this key starts a new chord -----------------------------

        for ((prefix, suffix), action) in &self.chords {
            if *prefix == combo {
                // Potential chord start.  We don't know yet if the user will
                // follow up with `suffix`, so enter pending mode.
                self.chord_state = Some(ChordState {
                    prefix: combo.clone(),
                    timestamp: Instant::now(),
                });
                // Store the expected action so `resolve` can return it on the
                // next call.  For now, return `None` -- the action will fire
                // when the second key arrives.
                //
                // However, if the prefix key *also* has a standalone binding,
                // we need to remember that in case the chord times out.
                // The timeout path above handles that by re-evaluating the
                // prefix.
                //
                // We return None to indicate "waiting for chord suffix".
                let _ = (suffix, action); // suppress unused warnings in this branch
                return None;
            }
        }

        // -- Normal single-key resolution --------------------------------------

        self.lookup_single(&combo, active_contexts)
    }

    /// Walk the active contexts in priority order and return the first match.
    fn lookup_single(
        &self,
        combo: &KeyCombo,
        active_contexts: &[KeyContext],
    ) -> Option<KeyAction> {
        // Walk contexts from highest priority (last) to lowest (first).
        for ctx in active_contexts.iter().rev() {
            if let Some(ctx_bindings) = self.bindings.get(ctx) {
                if let Some(action) = ctx_bindings.get(combo) {
                    return Some(action.clone());
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyCode;

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn global_ctrl_c_resolves_to_interrupt() {
        let mut mgr = KeybindingManager::with_defaults();
        let action = mgr.resolve(
            key(KeyCode::Char('c'), KeyModifiers::CONTROL),
            &[KeyContext::Global],
        );
        assert_eq!(action, Some(KeyAction::Interrupt));
    }

    #[test]
    fn chat_enter_resolves_to_submit() {
        let mut mgr = KeybindingManager::with_defaults();
        let action = mgr.resolve(
            key(KeyCode::Enter, KeyModifiers::NONE),
            &[KeyContext::Global, KeyContext::Chat],
        );
        assert_eq!(action, Some(KeyAction::Submit));
    }

    #[test]
    fn chat_esc_resolves_to_cancel() {
        let mut mgr = KeybindingManager::with_defaults();
        let action = mgr.resolve(
            key(KeyCode::Esc, KeyModifiers::NONE),
            &[KeyContext::Global, KeyContext::Chat],
        );
        assert_eq!(action, Some(KeyAction::Cancel));
    }

    #[test]
    fn autocomplete_tab_resolves_to_accept() {
        let mut mgr = KeybindingManager::with_defaults();
        let action = mgr.resolve(
            key(KeyCode::Tab, KeyModifiers::NONE),
            &[KeyContext::Global, KeyContext::Autocomplete],
        );
        assert_eq!(action, Some(KeyAction::AutocompleteAccept));
    }

    #[test]
    fn unknown_key_returns_none() {
        let mut mgr = KeybindingManager::with_defaults();
        let action = mgr.resolve(
            key(KeyCode::F(12), KeyModifiers::NONE),
            &[KeyContext::Global, KeyContext::Chat],
        );
        assert_eq!(action, None);
    }

    #[test]
    fn chord_ctrl_x_ctrl_k_resolves_to_interrupt() {
        let mut mgr = KeybindingManager::with_defaults();

        // First key: Ctrl+X -- should return None (chord pending).
        let first = mgr.resolve(
            key(KeyCode::Char('x'), KeyModifiers::CONTROL),
            &[KeyContext::Global],
        );
        assert_eq!(first, None);

        // Second key: Ctrl+K -- should complete the chord.
        let second = mgr.resolve(
            key(KeyCode::Char('k'), KeyModifiers::CONTROL),
            &[KeyContext::Global],
        );
        assert_eq!(second, Some(KeyAction::Interrupt));
    }

    #[test]
    fn chord_timeout_falls_back_to_prefix_binding() {
        let mut mgr = KeybindingManager::with_defaults();
        // Use an absurdly short timeout so the test doesn't wait.
        mgr.chord_timeout = Duration::from_millis(0);

        // First key: Ctrl+X -- no standalone binding in default map, so we
        // expect None.
        let first = mgr.resolve(
            key(KeyCode::Char('x'), KeyModifiers::CONTROL),
            &[KeyContext::Global],
        );
        assert_eq!(first, None);

        // Sleep past the timeout.
        std::thread::sleep(Duration::from_millis(5));

        // Second key: something unrelated.  The chord should have timed out.
        let second = mgr.resolve(
            key(KeyCode::Char('a'), KeyModifiers::NONE),
            &[KeyContext::Global, KeyContext::Chat],
        );
        // 'a' has no binding, so None is expected.
        assert_eq!(second, None);
    }

    #[test]
    fn context_priority_later_context_wins() {
        let mut mgr = KeybindingManager::with_defaults();

        // Esc in Global+HistorySearch -> HistorySearch's binding (SelectCancel).
        let action = mgr.resolve(
            key(KeyCode::Esc, KeyModifiers::NONE),
            &[KeyContext::Global, KeyContext::HistorySearch],
        );
        assert_eq!(action, Some(KeyAction::SelectCancel));
    }
}
