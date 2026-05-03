//! Layout system for the full-screen TUI.
//!
//! Splits the terminal area into logical regions: sticky prompt, scroll box,
//! bottom area (input / spinner), status bar, and optional modal overlay.
//! The layout is recomputed every frame from the current `AppState`.

pub mod scroll_box;
pub mod bottom_area;
pub mod modal_overlay;

pub use scroll_box::ScrollBoxState;
pub use bottom_area::BottomArea;
pub use modal_overlay::ModalOverlay;

use ratatui::layout::{Constraint, Direction, Layout, Rect};

use crate::tui::app::AppState;

/// Computed layout regions for a single frame.
///
/// Each field is a `Rect` that the caller can pass to the corresponding
/// renderer.  `modal_overlay` is `Some` only when an overlay is active.
#[derive(Debug, Clone)]
pub struct LayoutAreas {
    /// Single-line area shown at the very top when the user has scrolled away
    /// from the bottom (acts as a "scroll ↑" prompt).
    pub sticky_prompt: Rect,
    /// Main scrollable content area.
    pub scroll_box: Rect,
    /// Bottom area containing the input prompt and optional spinner.
    pub bottom_area: Rect,
    /// Single-line status bar at the very bottom of the screen.
    pub status_bar: Rect,
    /// Centered rectangle for the active modal overlay, if any.
    pub modal_overlay: Option<Rect>,
}

/// Stateless layout calculator.
///
/// Call [`FullscreenLayout::compute`] each frame to obtain the current set of
/// `LayoutAreas` from the full terminal area and application state.
pub struct FullscreenLayout;

impl FullscreenLayout {
    /// Compute the layout areas for the current frame.
    ///
    /// Regions are allocated from bottom to top:
    ///
    /// 1. **status_bar** -- always 1 row at the very bottom.
    /// 2. **bottom_area** -- height determined by [`BottomArea::height_needed`],
    ///    capped at half the remaining area.
    /// 3. **sticky_prompt** -- 1 row when `state.scroll > 0`, otherwise 0.
    /// 4. **scroll_box** -- all remaining vertical space.
    ///
    /// If `state.overlay` is `Some`, a centered modal rectangle (60% x 70% of
    /// the full area) is computed and stored in `modal_overlay`.
    pub fn compute(area: Rect, state: &AppState) -> LayoutAreas {
        let total_height = area.height;

        // Reserve 1 row for the status bar at the bottom.
        let status_bar_height: u16 = 1;
        let above_status = total_height.saturating_sub(status_bar_height);

        // Bottom area: variable, capped at half of what is left.
        let bottom_needed = BottomArea::height_needed(state);
        let bottom_area_height = bottom_needed.min(above_status / 2);
        let above_bottom = above_status.saturating_sub(bottom_area_height);

        // Sticky prompt: 1 row when scrolled, 0 otherwise.
        let sticky_height: u16 = if state.scroll > 0 { 1 } else { 0 };
        let scroll_box_height = above_bottom.saturating_sub(sticky_height);

        // Build the vertical layout from top to bottom.
        let constraints = [
            Constraint::Length(sticky_height),
            Constraint::Length(scroll_box_height),
            Constraint::Length(bottom_area_height),
            Constraint::Length(status_bar_height),
        ];

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(area);

        let modal_overlay = if state.overlay.is_some() {
            Some(ModalOverlay::centered_rect(60, 70, area))
        } else {
            None
        };

        LayoutAreas {
            sticky_prompt: chunks[0],
            scroll_box: chunks[1],
            bottom_area: chunks[2],
            status_bar: chunks[3],
            modal_overlay,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::AppState;

    fn test_state() -> AppState {
        AppState::new("test".into(), "provider".into(), "model".into())
    }

    #[test]
    fn compute_idle_layout() {
        let area = Rect::new(0, 0, 80, 24);
        let state = test_state();
        let areas = FullscreenLayout::compute(area, &state);

        assert_eq!(areas.status_bar.height, 1);
        assert_eq!(areas.bottom_area.height, 3); // Idle => 3
        assert_eq!(areas.sticky_prompt.height, 0); // scroll == 0
        // scroll_box = 24 - 1 - 3 - 0 = 20
        assert_eq!(areas.scroll_box.height, 20);
        assert!(areas.modal_overlay.is_none());
    }

    #[test]
    fn compute_with_scrolled_state() {
        let area = Rect::new(0, 0, 80, 24);
        let mut state = test_state();
        state.scroll = 5;
        let areas = FullscreenLayout::compute(area, &state);

        assert_eq!(areas.sticky_prompt.height, 1);
        // scroll_box = 24 - 1 - 3 - 1 = 19
        assert_eq!(areas.scroll_box.height, 19);
    }

    #[test]
    fn compute_with_thinking_status() {
        let area = Rect::new(0, 0, 80, 24);
        let mut state = test_state();
        state.status = crate::tui::app::Status::Thinking;
        let areas = FullscreenLayout::compute(area, &state);

        assert_eq!(areas.bottom_area.height, 4); // 3 base + 1 spinner
    }

    #[test]
    fn compute_with_overlay() {
        let area = Rect::new(0, 0, 100, 50);
        let mut state = test_state();
        // We just need overlay to be Some; use a dummy HistorySearchOverlay.
        use crate::tui::overlay::HistorySearchOverlay;
        state.overlay = Some(crate::tui::overlay::OverlayType::HistorySearch(
            HistorySearchOverlay::new(),
        ));
        let areas = FullscreenLayout::compute(area, &state);

        let modal = areas.modal_overlay.expect("modal should be present");
        assert_eq!(modal.width, 60);  // 60% of 100
        assert_eq!(modal.height, 35); // 70% of 50
    }
}
