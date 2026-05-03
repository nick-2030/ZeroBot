//! Spinner component — placeholder for loading animation.
//!
//! Full implementation will be added in a subsequent task.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::tui::app::AppState;

/// Renders a loading spinner. Currently a no-op placeholder.
pub struct Spinner;

impl Spinner {
    pub fn render(_buf: &mut Buffer, _area: Rect, _state: &AppState) {
        // Placeholder: full spinner animation implementation in a later task.
    }
}
