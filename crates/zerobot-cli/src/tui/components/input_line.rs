//! Input line component — renders the user text input area with a blinking cursor.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, Borders, Widget};
use unicode_width::UnicodeWidthStr;

use crate::tui::app::AppState;
use crate::tui::theme::THEME;

/// Renders the input prompt with the current user text and cursor.
pub struct InputLine;

impl InputLine {
    pub fn render(buf: &mut Buffer, area: Rect, state: &AppState) {
        let theme = &THEME;

        // Draw a bordered block with top + bottom borders.
        let block = Block::default()
            .borders(Borders::TOP | Borders::BOTTOM)
            .border_style(Style::default().fg(theme.panel_border));

        let inner = block.inner(area);
        Widget::render(block, area, buf);

        if inner.height == 0 || inner.width < 3 {
            return;
        }

        // Render the prompt prefix "> "
        let prompt = "> ";
        let prompt_style = Style::default()
            .fg(theme.input_prompt)
            .add_modifier(Modifier::BOLD);
        buf.set_span(inner.x, inner.y, &Span::styled(prompt, prompt_style), 2);

        // Render the user input text.
        let input_x = inner.x + 2;
        let available_width = inner.width.saturating_sub(2) as usize;
        let input_text = if UnicodeWidthStr::width(state.input.as_str()) > available_width {
            // Truncate from the left if input is wider than available space.
            truncate_left(&state.input, available_width)
        } else {
            state.input.clone()
        };
        buf.set_span(
            input_x,
            inner.y,
            &Span::styled(input_text, Style::default().fg(theme.text)),
            inner.width.saturating_sub(2),
        );

        // NOTE: The cursor position must be set at the Frame level by the
        // caller using `frame.set_cursor(x, y)`.  We cannot call set_cursor
        // here because the Component trait receives a `&mut Buffer`, not a
        // `Frame`.  Use [`Self::cursor_position`] to compute the coordinates.
        //
        // The cursor position is:
        //   x = input_x + unicode_width(input[..cursor])
        //   y = inner.y
    }

    /// Compute the cursor position for the input line, if visible.
    ///
    /// Returns `(x, y)` in terminal coordinates, suitable for
    /// `frame.set_cursor(x, y)`.
    pub fn cursor_position(area: Rect, state: &AppState) -> Option<(u16, u16)> {
        let block = Block::default()
            .borders(Borders::TOP | Borders::BOTTOM);
        let inner = block.inner(area);
        if inner.height == 0 || inner.width < 3 {
            return None;
        }
        let input_x = inner.x + 2;
        // Ensure cursor lands on a UTF-8 character boundary to avoid panicking
        // on slice if cursor happens to be mid-codepoint.
        let cursor_end = find_char_boundary(&state.input, state.cursor);
        let cursor_display_width = UnicodeWidthStr::width(&state.input[..cursor_end]);
        let cursor_x = input_x + cursor_display_width as u16;
        if cursor_x < inner.x + inner.width {
            Some((cursor_x, inner.y))
        } else {
            None
        }
    }
}

/// Truncate a string from the left to fit within `max_width` display columns.
fn truncate_left(text: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    if max_width < 4 {
        // Not enough room for "..." + content, just truncate raw.
        let mut end = 0;
        let mut w = 0;
        for (i, ch) in text.char_indices() {
            let cw = UnicodeWidthStr::width(ch.to_string().as_str());
            if w + cw > max_width {
                break;
            }
            w += cw;
            end = i + ch.len_utf8();
        }
        return text[..end].to_string();
    }
    let total_width = UnicodeWidthStr::width(text);
    if total_width <= max_width {
        return text.to_string();
    }
    let skip = total_width - max_width + 3; // +3 for "..."
    let mut skipped = 0usize;
    let mut start = 0;
    for (i, ch) in text.char_indices() {
        let w = UnicodeWidthStr::width(ch.to_string().as_str());
        if skipped + w > skip {
            start = i;
            break;
        }
        skipped += w;
        start = i + ch.len_utf8();
    }
    format!("...{}", &text[start..])
}

/// Find the nearest valid UTF-8 character boundary at or before `pos`.
///
/// If `pos` is already a char boundary, returns `pos`. Otherwise, walks back
/// to the nearest preceding boundary. Returns 0 if `pos` is beyond the string.
fn find_char_boundary(s: &str, pos: usize) -> usize {
    if pos >= s.len() {
        return s.len();
    }
    let mut boundary = pos;
    while boundary > 0 && !s.is_char_boundary(boundary) {
        boundary -= 1;
    }
    boundary
}
