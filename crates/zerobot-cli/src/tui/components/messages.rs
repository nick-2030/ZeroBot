//! Messages component — virtualised rendering of the output scrollback.
//!
//! Collects all `OutputItem`s from `AppState` into styled `Line`s and renders
//! only the visible portion based on the current scroll offset.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::tui::app::{AppState, DotColor, OutputItem};
use crate::tui::markdown::format_markdown_lines;
use crate::tui::theme::THEME;

/// Renders the message list area with virtualised scrolling.
pub struct Messages;

impl Messages {
    pub fn render(buf: &mut Buffer, area: Rect, state: &AppState) {
        let theme = &THEME;
        let width = area.width;
        let visible_height = area.height as usize;

        if visible_height == 0 || width == 0 {
            return;
        }

        let all_lines = Self::collect_all_lines(state, width);
        let total = all_lines.len();

        // Determine the scroll offset.
        let scroll = if state.stick_to_bottom || total <= visible_height {
            // Show the bottom of the list.
            total.saturating_sub(visible_height)
        } else {
            (state.scroll as usize).min(total.saturating_sub(visible_height))
        };

        // Render only the visible lines.
        for (i, line) in all_lines.iter().skip(scroll).take(visible_height).enumerate() {
            let y = area.y + i as u16;
            buf.set_line(area.x, y, line, width);
        }

        // Fill remaining rows with the panel background.
        let rendered = total.saturating_sub(scroll).min(visible_height);
        for i in rendered..visible_height {
            let y = area.y + i as u16;
            for x in area.x..area.x + width {
                let cell = buf.get_mut(x, y);
                cell.set_symbol(" ");
                cell.set_style(Style::default().bg(theme.panel_bg));
            }
        }
    }

    /// Collect all output items into a flat list of styled lines.
    ///
    /// This mirrors the `display_lines` logic from the legacy implementation.
    fn collect_all_lines(state: &AppState, width: u16) -> Vec<Line<'static>> {
        let mut out: Vec<Line<'static>> = Vec::new();

        for item in &state.output {
            let mut lines = match item {
                OutputItem::Lines(lines) => lines.clone(),
                OutputItem::Block { color, text } => format_block_lines(*color, text),
                OutputItem::Markdown(text) => format_markdown_lines(text, width),
                OutputItem::ToolRunning { label, arguments } => {
                    format_running_tool_line(label, arguments, state.blink_on())
                }
                OutputItem::ToolOutput {
                    color,
                    tool_name,
                    label,
                    arguments,
                    output,
                    expanded,
                    duration_ms,
                } => {
                    format_tool_output_lines(
                        *color,
                        tool_name,
                        label.as_deref(),
                        arguments,
                        output,
                        *expanded,
                        state.show_full_tool_output,
                        *duration_ms,
                    )
                }
                OutputItem::HookRunning { label } => {
                    vec![format_running_hook_line(label, state.blink_on())]
                }
                OutputItem::HookOutput { ok, label } => {
                    vec![format_hook_output_line(*ok, label)]
                }
            };

            if lines.is_empty() {
                continue;
            }

            // Insert a blank line between items.
            if !out.is_empty() {
                out.push(Line::from(Span::raw("")));
            }
            out.append(&mut lines);
        }

        // Append the streaming buffer if currently streaming.
        if state.streaming {
            if !out.is_empty() {
                out.push(Line::from(Span::raw("")));
            }
            out.extend(format_block_lines(DotColor::White, &state.stream_buffer));
        }

        out
    }
}

// ---------------------------------------------------------------------------
// Dot helpers
// ---------------------------------------------------------------------------

/// Map a `DotColor` to the corresponding theme color.
fn dot_color(color: DotColor) -> ratatui::style::Color {
    let theme = &THEME;
    match color {
        DotColor::White => theme.accent,
        DotColor::Green => theme.success,
        DotColor::Yellow => theme.warn,
        DotColor::Red => theme.error,
    }
}

/// Render a filled circle span (`●`) in the given `DotColor`.
fn dot_span(color: DotColor) -> Span<'static> {
    Span::styled("\u{25CF}", Style::default().fg(dot_color(color)))
}

/// Render a medium circle span (`⏺`) used for tool indicators.
fn tool_dot_span(color: DotColor) -> Span<'static> {
    Span::styled("\u{23FA}", Style::default().fg(dot_color(color)))
}

/// Render a blinking tool dot: visible when `blink_on`, invisible otherwise.
fn running_tool_dot_span(blink_on: bool) -> Span<'static> {
    if blink_on {
        tool_dot_span(DotColor::White)
    } else {
        Span::styled(" ", Style::default())
    }
}

// ---------------------------------------------------------------------------
// Block / text formatting
// ---------------------------------------------------------------------------

/// Format a `Block` output item into styled lines with a leading dot.
fn format_block_lines(color: DotColor, text: &str) -> Vec<Line<'static>> {
    let cleaned = text.trim_end_matches('\n');
    if cleaned.trim().is_empty() {
        return vec![Line::from(vec![dot_span(color), Span::raw(" ")])];
    }
    let mut lines = Vec::new();
    for (idx, line) in cleaned.lines().enumerate() {
        if idx == 0 {
            lines.push(Line::from(vec![
                dot_span(color),
                Span::raw(" "),
                Span::raw(line.to_string()),
            ]));
        } else {
            lines.push(Line::from(Span::raw(format!("  {line}"))));
        }
    }
    lines
}

// ---------------------------------------------------------------------------
// Tool / hook line formatters
// ---------------------------------------------------------------------------

/// Format running tool lines: `[dot] **tool_name** (key_param)`
fn format_running_tool_line(label: &str, arguments: &str, blink_on: bool) -> Vec<Line<'static>> {
    let theme = &THEME;
    let display = format_tool_display_name(label, arguments);
    vec![Line::from(vec![
        running_tool_dot_span(blink_on),
        Span::raw(" "),
        Span::styled(
            label.to_string(),
            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" ({display})"), Style::default().fg(theme.text_dim)),
    ])]
}

/// Format completed tool output lines.
fn format_tool_output_lines(
    color: DotColor,
    tool_name: &str,
    label: Option<&str>,
    arguments: &str,
    output: &str,
    expanded: bool,
    show_full: bool,
    duration_ms: Option<u64>,
) -> Vec<Line<'static>> {
    let theme = &THEME;
    let mut lines = Vec::new();

    // Header: [dot] **tool_name** (key_param) (duration)
    let display = format_tool_display_name(tool_name, arguments);
    let mut header = vec![
        tool_dot_span(color),
        Span::raw(" "),
        Span::styled(
            tool_name.to_string(),
            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" ({display})"), Style::default().fg(theme.text_dim)),
    ];
    if let Some(ms) = duration_ms {
        header.push(Span::styled(
            format!(" ({ms}ms)"),
            Style::default().fg(theme.text_muted),
        ));
    }
    lines.push(Line::from(header));

    // Output preview (truncated or expanded)
    if !output.is_empty() && (expanded || show_full) {
        for line in output.lines() {
            lines.push(Line::from(Span::styled(
                format!("  {line}"),
                Style::default().fg(theme.text_dim),
            )));
        }
    } else if !output.is_empty() {
        let preview: Vec<&str> = output.lines().take(3).collect();
        let total = output.lines().count();
        for line in &preview {
            lines.push(Line::from(Span::styled(
                format!("  {line}"),
                Style::default().fg(theme.text_dim),
            )));
        }
        if total > 3 {
            lines.push(Line::from(Span::styled(
                format!("  ... (+{} lines)", total - 3),
                Style::default().fg(theme.text_muted),
            )));
        }
    }

    lines
}

/// Extract the key display parameter for a tool based on its name.
fn format_tool_display_name(tool_name: &str, arguments: &str) -> String {
    let args: Option<serde_json::Value> = serde_json::from_str(arguments).ok();
    match tool_name.to_lowercase().as_str() {
        "bash" | "shell" => {
            if let Some(ref a) = args {
                if let Some(cmd) = a.get("command").and_then(|v| v.as_str()) {
                    return truncate_display(cmd, 80);
                }
            }
            truncate_display(arguments, 80)
        }
        "read" | "write" | "edit" | "apply_patch" | "patch" => {
            if let Some(ref a) = args {
                if let Some(path) = a
                    .get("file_path")
                    .or_else(|| a.get("path"))
                    .and_then(|v| v.as_str())
                {
                    return path.to_string();
                }
            }
            truncate_display(arguments, 60)
        }
        "glob" | "grep" | "ls" => {
            if let Some(ref a) = args {
                if let Some(p) = a
                    .get("pattern")
                    .or_else(|| a.get("query"))
                    .or_else(|| a.get("path"))
                    .and_then(|v| v.as_str())
                {
                    return truncate_display(p, 60);
                }
            }
            truncate_display(arguments, 60)
        }
        _ => {
            if let Some(ref a) = args {
                if let Some(obj) = a.as_object() {
                    for val in obj.values() {
                        if let Some(s) = val.as_str() {
                            if !s.is_empty() {
                                return truncate_display(s, 60);
                            }
                        }
                    }
                }
            }
            truncate_display(arguments, 60)
        }
    }
}

fn truncate_display(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        let truncated: String = text.chars().take(max_chars - 3).collect();
        format!("{truncated}...")
    }
}

/// Format a running hook indicator line with a flashing icon.
fn format_running_hook_line(label: &str, blink_on: bool) -> Line<'static> {
    let theme = &THEME;
    let icon = if blink_on { "\u{26A1}" } else { " " };
    Line::from(vec![
        Span::styled(icon, Style::default().fg(theme.warn)),
        Span::styled(" ", Style::default()),
        Span::styled(
            format!("Hook: {label}"),
            Style::default().fg(theme.warn).add_modifier(Modifier::DIM),
        ),
    ])
}

/// Format a completed hook output line with a pass/fail icon.
fn format_hook_output_line(ok: bool, label: &str) -> Line<'static> {
    let theme = &THEME;
    let (icon, color) = if ok {
        ("\u{2713}", theme.success)
    } else {
        ("\u{2717}", theme.error)
    };
    Line::from(vec![
        Span::styled(icon, Style::default().fg(color)),
        Span::styled(" ", Style::default()),
        Span::styled(
            format!("Hook: {label}"),
            Style::default().fg(color).add_modifier(Modifier::DIM),
        ),
    ])
}
