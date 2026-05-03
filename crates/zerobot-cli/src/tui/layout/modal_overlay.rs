//! Modal overlay layout helpers.
//!
//! Provides utilities for positioning and drawing modal dialogs on top of the
//! main layout.

use ratatui::layout::Rect;
use ratatui::buffer::Buffer;
use ratatui::style::Style;

use crate::tui::theme::THEME;

/// Helper for computing and drawing modal overlay regions.
pub struct ModalOverlay;

impl ModalOverlay {
    /// Compute a centered rectangle within `area`.
    ///
    /// `percent_x` and `percent_y` control the width and height of the
    /// rectangle as a percentage (0-100) of the parent area.  The resulting
    /// rectangle is centered both horizontally and vertically.
    pub fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
        let px = percent_x.min(100);
        let py = percent_y.min(100);

        let popup_width = area.width * px / 100;
        let popup_height = area.height * py / 100;

        let popup_x = area.x + (area.width - popup_width) / 2;
        let popup_y = area.y + (area.height - popup_height) / 2;

        Rect::new(popup_x, popup_y, popup_width, popup_height)
    }

    /// Render a horizontal divider line at the top of `area` using the
    /// `modal_divider` color from the theme.
    ///
    /// The divider is a row of `U+2594 UPPER ONE EIGHTH BLOCK` characters
    /// (`\u{2594}`) to produce a subtle top rule.
    pub fn render_modal_divider(buf: &mut Buffer, area: Rect) {
        if area.height == 0 {
            return;
        }
        let theme = &*THEME;
        let style = Style::default().fg(theme.modal_divider);
        let divider_char = '\u{2594}'; // upper one eighth block
        let y = area.y;
        for x in area.x..area.x + area.width {
            buf.set_string(x, y, divider_char.to_string(), style);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centered_rect_basic() {
        let area = Rect::new(0, 0, 100, 100);
        let centered = ModalOverlay::centered_rect(50, 60, area);
        assert_eq!(centered.width, 50);
        assert_eq!(centered.height, 60);
        assert_eq!(centered.x, 25);
        assert_eq!(centered.y, 20);
    }

    #[test]
    fn centered_rect_clamped_to_100() {
        let area = Rect::new(10, 10, 80, 60);
        let centered = ModalOverlay::centered_rect(150, 200, area);
        assert_eq!(centered.width, 80);
        assert_eq!(centered.height, 60);
    }
}
