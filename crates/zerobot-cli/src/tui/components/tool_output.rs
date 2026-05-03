//! Tool output rendering — collapsed and expanded views.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use crate::tui::theme::THEME;

pub struct ToolOutput;

impl ToolOutput {
    /// Render a collapsed (single-line) tool output summary.
    pub fn render_collapsed(
        buf: &mut Buffer,
        area: Rect,
        name: &str,
        ok: bool,
        duration_ms: Option<u64>,
    ) {
        let theme = &THEME;
        let (icon, color) = if ok {
            ("\u{2713}", theme.success)
        } else {
            ("\u{2717}", theme.error)
        };
        let mut spans = vec![
            Span::styled(icon, Style::default().fg(color)),
            Span::raw(" "),
            Span::styled(
                name.to_string(),
                Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
            ),
        ];
        if let Some(ms) = duration_ms {
            spans.push(Span::styled(
                format!(" ({ms}ms)"),
                Style::default().fg(theme.text_dim),
            ));
        }
        Widget::render(
            ratatui::widgets::Paragraph::new(Line::from(spans)),
            area,
            buf,
        );
    }

    /// Render an expanded tool output with full content.
    pub fn render_expanded(
        buf: &mut Buffer,
        area: Rect,
        name: &str,
        output: &str,
        ok: bool,
        duration_ms: Option<u64>,
    ) {
        let theme = &THEME;
        let (icon, color) = if ok {
            ("\u{2713}", theme.success)
        } else {
            ("\u{2717}", theme.error)
        };
        // Header line
        let mut header_spans = vec![
            Span::styled(icon, Style::default().fg(color)),
            Span::raw(" "),
            Span::styled(
                name.to_string(),
                Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
            ),
        ];
        if let Some(ms) = duration_ms {
            header_spans.push(Span::styled(
                format!(" ({ms}ms)"),
                Style::default().fg(theme.text_dim),
            ));
        }
        Widget::render(
            ratatui::widgets::Paragraph::new(Line::from(header_spans)),
            Rect::new(area.x, area.y, area.width, 1),
            buf,
        );
        // Output lines
        for (i, line) in output.lines().enumerate() {
            if i as u16 + 1 >= area.height {
                break;
            }
            let y = area.y + i as u16 + 1;
            buf.set_string(
                area.x + 2,
                y,
                line,
                Style::default().fg(theme.text_dim),
            );
        }
    }
}
