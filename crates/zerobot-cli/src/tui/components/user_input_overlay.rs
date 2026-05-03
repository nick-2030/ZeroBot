//! User input overlay rendering helper.
//!
//! The main rendering and key handling is done by the `UserInputOverlay` struct
//! in `overlay.rs` via the `OverlayComponent` trait.  This module provides
//! lightweight helper methods for supplementary rendering.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::tui::theme::THEME;

pub struct UserInputOverlayRenderer;

impl UserInputOverlayRenderer {
    /// Placeholder render — real rendering is handled by `OverlayComponent::render`.
    pub fn render(_buf: &mut Buffer, _area: Rect) {
        let _ = &THEME;
    }
}
