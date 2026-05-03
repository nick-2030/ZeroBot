//! Single message item component — placeholder for per-item rendering.
//!
//! Full implementation (collapse/expand for tool output, etc.) will be added
//! in a subsequent task.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::tui::app::OutputItem;

/// Renders a single output item. Currently a no-op placeholder.
pub struct MessageItem;

impl MessageItem {
    pub fn render(_item: &OutputItem, _buf: &mut Buffer, _area: Rect) {
        // Placeholder: full per-item rendering in a later task.
    }
}
