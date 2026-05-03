//! ScrollBox state management.
//!
//! Tracks scroll offset, viewport dimensions, and sticky-to-bottom behavior
//! for the main content scroll area.

/// Manages the scroll position and "stick to bottom" flag for a scrollable
/// content area.
///
/// When `sticky` is `true`, newly arriving content automatically scrolls to
/// the bottom.  Scrolling up clears the sticky flag; scrolling to the bottom
/// re-enables it.
#[derive(Debug, Clone)]
pub struct ScrollBoxState {
    /// Current scroll offset from the top (0 = first line visible).
    pub offset: u16,
    /// Total number of content lines available.
    pub total_lines: u16,
    /// Number of visible rows in the viewport.
    pub viewport_height: u16,
    /// When `true`, new content automatically scrolls to the bottom.
    pub sticky: bool,
}

impl ScrollBoxState {
    /// Create a new `ScrollBoxState` with sticky enabled and everything else
    /// zeroed.
    pub fn new() -> Self {
        Self {
            offset: 0,
            total_lines: 0,
            viewport_height: 0,
            sticky: true,
        }
    }

    /// Scroll down by `lines` rows.
    ///
    /// Clamps to the maximum valid offset.  If the scroll reaches the bottom,
    /// `sticky` is automatically set to `true`.
    pub fn scroll_down(&mut self, lines: u16) {
        let max_offset = self.max_offset();
        self.offset = (self.offset + lines).min(max_offset);
        if self.offset >= max_offset {
            self.sticky = true;
        }
    }

    /// Scroll up by `lines` rows.
    ///
    /// Clamps to 0.  Always clears the `sticky` flag.
    pub fn scroll_up(&mut self, lines: u16) {
        self.offset = self.offset.saturating_sub(lines);
        self.sticky = false;
    }

    /// Jump to the very top.  Clears the `sticky` flag.
    pub fn scroll_to_top(&mut self) {
        self.offset = 0;
        self.sticky = false;
    }

    /// Jump to the very bottom.  Sets `sticky` to `true`.
    pub fn scroll_to_bottom(&mut self) {
        self.offset = self.max_offset();
        self.sticky = true;
    }

    /// Enable sticky mode and immediately scroll to the bottom.
    pub fn stick_to_bottom(&mut self) {
        self.sticky = true;
        self.offset = self.max_offset();
    }

    /// Return the `(start, end)` range of visible content lines.
    ///
    /// `start` is inclusive, `end` is exclusive.  Both are clamped to
    /// `[0, total_lines]`.
    pub fn visible_range(&self) -> (u16, u16) {
        let start = self.offset.min(self.total_lines);
        let end = (self.offset + self.viewport_height).min(self.total_lines);
        (start, end)
    }

    /// Notify the scroll box that new content has arrived.
    ///
    /// Updates `total_lines`.  If `sticky` is `true`, the offset is
    /// automatically adjusted to keep the bottom in view.
    pub fn on_new_content(&mut self, total_lines: u16) {
        self.total_lines = total_lines;
        if self.sticky {
            self.offset = self.max_offset();
        }
    }

    /// Compute the maximum valid scroll offset.
    fn max_offset(&self) -> u16 {
        self.total_lines.saturating_sub(self.viewport_height)
    }
}

impl Default for ScrollBoxState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_is_sticky() {
        let s = ScrollBoxState::new();
        assert!(s.sticky);
        assert_eq!(s.offset, 0);
    }

    #[test]
    fn scroll_up_clears_sticky() {
        let mut s = ScrollBoxState::new();
        s.total_lines = 100;
        s.viewport_height = 10;
        s.sticky = true;
        s.scroll_up(1);
        assert!(!s.sticky);
    }

    #[test]
    fn scroll_down_sets_sticky_at_bottom() {
        let mut s = ScrollBoxState::new();
        s.total_lines = 100;
        s.viewport_height = 10;
        s.sticky = false;
        s.scroll_down(200);
        assert!(s.sticky);
        assert_eq!(s.offset, 90); // 100 - 10
    }

    #[test]
    fn visible_range_basic() {
        let mut s = ScrollBoxState::new();
        s.total_lines = 50;
        s.viewport_height = 10;
        s.offset = 5;
        assert_eq!(s.visible_range(), (5, 15));
    }

    #[test]
    fn visible_range_clamped() {
        let mut s = ScrollBoxState::new();
        s.total_lines = 5;
        s.viewport_height = 10;
        assert_eq!(s.visible_range(), (0, 5));
    }

    #[test]
    fn on_new_content_follows_when_sticky() {
        let mut s = ScrollBoxState::new();
        s.viewport_height = 10;
        s.sticky = true;
        s.on_new_content(50);
        assert_eq!(s.offset, 40);
    }

    #[test]
    fn on_new_content_ignores_when_not_sticky() {
        let mut s = ScrollBoxState::new();
        s.viewport_height = 10;
        s.sticky = false;
        s.offset = 5;
        s.on_new_content(50);
        assert_eq!(s.offset, 5);
    }
}
