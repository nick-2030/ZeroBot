//! Markdown rendering for the TUI.
//!
//! Parses markdown using `pulldown_cmark` and produces `ratatui::text::Line`
//! values with syntax-highlighted code blocks (via `syntect`), inline
//! formatting, lists, tables, and thinking blocks.

use std::sync::OnceLock;

use pulldown_cmark::{Alignment, CodeBlockKind, Event as MdEvent, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Theme as SyntectTheme, ThemeSet};
use syntect::parsing::SyntaxSet;
use unicode_width::UnicodeWidthStr;

use crate::tui::theme::THEME;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Render a markdown string into styled `Line`s suitable for ratatui.
///
/// This is the top-level entry point.  It handles thinking blocks (splitting
/// them out from normal markdown) and delegates to `markdown_to_lines` for the
/// actual pulldown-cmark rendering.
pub fn render_markdown(text: &str, width: u16) -> Vec<Line<'static>> {
    let normalized = normalize_thinking_fences(text);
    let segments = split_thinking_blocks(&normalized);
    if segments.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for seg in segments {
        match seg.kind {
            ThinkingSegmentKind::Normal => {
                out.extend(markdown_to_lines(&seg.content, width));
            }
            ThinkingSegmentKind::Thinking => {
                out.extend(format_thinking_block_lines(&seg.content, width));
            }
        }
    }
    out
}

/// Like [`render_markdown`] but prefixes the first line with a dot marker and
/// indents subsequent lines for use inside output blocks.
pub fn format_markdown_lines(text: &str, width: u16) -> Vec<Line<'static>> {
    let mut lines = render_markdown(text, width);
    if lines.is_empty() {
        return vec![Line::from(vec![dot_span(), Span::raw(" ")])];
    }
    let mut out = Vec::new();
    for (idx, mut line) in lines.drain(..).enumerate() {
        let mut spans = Vec::new();
        if idx == 0 {
            spans.push(dot_span());
            spans.push(Span::raw(" "));
        } else {
            spans.push(Span::raw("  "));
        }
        spans.extend(line.spans.drain(..));
        out.push(Line::from(spans));
    }
    out
}

// ---------------------------------------------------------------------------
// Thinking block support
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum ThinkingSegmentKind {
    Normal,
    Thinking,
}

struct ThinkingSegment {
    kind: ThinkingSegmentKind,
    content: String,
}

/// Normalize various thinking fence formats into `<thinking>` / `</thinking>`.
fn normalize_thinking_fences(text: &str) -> String {
    let mut out = String::new();
    let text = text
        .replace("<think>", "<thinking>")
        .replace("</think>", "</thinking>");
    let mut in_thinking = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if !in_thinking {
            if trimmed.starts_with("```thinking") || trimmed.starts_with("```analysis") {
                in_thinking = true;
                out.push_str("<thinking>\n");
                continue;
            }
            out.push_str(line);
            out.push('\n');
        } else if trimmed.starts_with("```") {
            in_thinking = false;
            out.push_str("</thinking>\n");
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    if in_thinking {
        out.push_str("</thinking>\n");
    }
    out
}

/// Split text into normal and thinking segments.
fn split_thinking_blocks(text: &str) -> Vec<ThinkingSegment> {
    const OPEN: &str = "<thinking>";
    const CLOSE: &str = "</thinking>";
    let mut out = Vec::new();
    let mut rest = text;
    loop {
        let Some(start) = rest.find(OPEN) else {
            if !rest.is_empty() {
                out.push(ThinkingSegment {
                    kind: ThinkingSegmentKind::Normal,
                    content: rest.to_string(),
                });
            }
            break;
        };
        let before = &rest[..start];
        if !before.is_empty() {
            out.push(ThinkingSegment {
                kind: ThinkingSegmentKind::Normal,
                content: before.to_string(),
            });
        }
        let after_open = &rest[start + OPEN.len()..];
        let Some(end) = after_open.find(CLOSE) else {
            out.push(ThinkingSegment {
                kind: ThinkingSegmentKind::Normal,
                content: rest.to_string(),
            });
            break;
        };
        let content = &after_open[..end];
        out.push(ThinkingSegment {
            kind: ThinkingSegmentKind::Thinking,
            content: content.to_string(),
        });
        rest = &after_open[end + CLOSE.len()..];
    }
    out
}

// ---------------------------------------------------------------------------
// Core markdown-to-lines conversion (pulldown_cmark)
// ---------------------------------------------------------------------------

/// Convert a markdown string (without thinking blocks) into styled `Line`s.
fn markdown_to_lines(text: &str, width: u16) -> Vec<Line<'static>> {
    let theme = &THEME;
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TABLES);
    let parser = Parser::new_ext(text, opts);

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current: Vec<Span<'static>> = Vec::new();
    let mut style_stack: Vec<Style> = vec![Style::default()];
    let mut list_stack: Vec<(bool, usize, usize)> = Vec::new(); // (ordered, index, indent_len)
    let mut pending_prefix: Option<String> = None;
    let mut current_prefix: Option<String> = None;
    let mut blockquote_depth = 0usize;
    let mut table_align: Vec<Alignment> = Vec::new();
    let mut table_rows: Vec<Vec<String>> = Vec::new();
    let mut current_row: Vec<String> = Vec::new();
    let mut current_cell = String::new();
    let mut in_table_cell = false;
    let mut table_header_rows = 0usize;
    let mut in_table_head = false;
    let mut code_block_lines: Vec<String> = Vec::new();
    let mut code_block_lang: Option<String> = None;
    let mut in_code_block = false;

    let flush_line = |lines: &mut Vec<Line<'static>>, current: &mut Vec<Span<'static>>| {
        if !current.is_empty() {
            lines.push(Line::from(std::mem::take(current)));
        }
    };

    let ensure_prefix =
        |current: &mut Vec<Span<'static>>,
         pending_prefix: &mut Option<String>,
         current_prefix: &Option<String>,
         list_stack: &[(bool, usize, usize)],
         blockquote_depth: usize| {
            if !current.is_empty() {
                return;
            }
            if blockquote_depth > 0 {
                let prefix = "│ ".repeat(blockquote_depth);
                current.push(Span::styled(
                    prefix,
                    Style::default().fg(theme.text_muted),
                ));
            }
            let mut prefix = String::new();
            if !list_stack.is_empty() {
                if let Some(p) = pending_prefix.take() {
                    prefix.push_str(&p);
                } else if let Some(indent) = current_prefix {
                    prefix.push_str(indent);
                }
            }
            if !prefix.is_empty() {
                current.push(Span::raw(prefix));
            }
        };

    for event in parser {
        match event {
            MdEvent::Start(tag) => match tag {
                Tag::Emphasis => {
                    let style = style_stack
                        .last()
                        .copied()
                        .unwrap_or_default()
                        .add_modifier(Modifier::ITALIC);
                    style_stack.push(style);
                }
                Tag::Strong => {
                    let style = style_stack
                        .last()
                        .copied()
                        .unwrap_or_default()
                        .add_modifier(Modifier::BOLD);
                    style_stack.push(style);
                }
                Tag::Strikethrough => {
                    let style = style_stack
                        .last()
                        .copied()
                        .unwrap_or_default()
                        .add_modifier(Modifier::CROSSED_OUT);
                    style_stack.push(style);
                }
                Tag::Heading { .. } => {
                    let style = style_stack
                        .last()
                        .copied()
                        .unwrap_or_default()
                        .add_modifier(Modifier::BOLD);
                    style_stack.push(style);
                }
                Tag::CodeBlock(kind) => {
                    flush_line(&mut lines, &mut current);
                    in_code_block = true;
                    code_block_lines.clear();
                    code_block_lang = match kind {
                        CodeBlockKind::Fenced(info) => info
                            .split_whitespace()
                            .next()
                            .map(|s| s.to_string())
                            .filter(|s| !s.is_empty()),
                        _ => None,
                    };
                }
                Tag::BlockQuote(_) => {
                    flush_line(&mut lines, &mut current);
                    blockquote_depth = blockquote_depth.saturating_add(1);
                }
                Tag::List(start) => {
                    let ordered = start.is_some();
                    let index = start.unwrap_or(1);
                    let indent = if ordered {
                        index.to_string().len() + 2
                    } else {
                        2
                    };
                    list_stack.push((ordered, index as usize, indent));
                }
                Tag::Item => {
                    if let Some((ordered, index, indent)) = list_stack.last_mut() {
                        let prefix = if *ordered {
                            let p = format!("{}. ", *index);
                            *index += 1;
                            p
                        } else {
                            "• ".to_string()
                        };
                        pending_prefix = Some(prefix);
                        current_prefix = Some(" ".repeat(*indent));
                    }
                }
                Tag::Link { .. } => {
                    let style = style_stack
                        .last()
                        .copied()
                        .unwrap_or_default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::UNDERLINED);
                    style_stack.push(style);
                }
                Tag::Table(align) => {
                    flush_line(&mut lines, &mut current);
                    table_align = align;
                    table_rows.clear();
                    current_row.clear();
                    current_cell.clear();
                    in_table_cell = false;
                    table_header_rows = 0;
                    in_table_head = false;
                }
                Tag::TableHead => {
                    in_table_head = true;
                }
                Tag::TableRow => {
                    current_row.clear();
                }
                Tag::TableCell => {
                    in_table_cell = true;
                    current_cell.clear();
                }
                _ => {}
            },
            MdEvent::End(tag) => match tag {
                TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough | TagEnd::Heading(_) => {
                    if style_stack.len() > 1 {
                        style_stack.pop();
                    }
                    if matches!(tag, TagEnd::Heading(_)) {
                        flush_line(&mut lines, &mut current);
                        lines.push(Line::from(Span::raw("")));
                    }
                }
                TagEnd::Paragraph => {
                    flush_line(&mut lines, &mut current);
                    lines.push(Line::from(Span::raw("")));
                }
                TagEnd::CodeBlock => {
                    in_code_block = false;
                    if code_block_lines.is_empty() {
                        code_block_lines.push(String::new());
                    }
                    lines.extend(render_code_block_lines(
                        &code_block_lines,
                        code_block_lang.as_deref(),
                        width,
                    ));
                    code_block_lines.clear();
                    code_block_lang = None;
                    lines.push(Line::from(Span::raw("")));
                }
                TagEnd::BlockQuote => {
                    flush_line(&mut lines, &mut current);
                    blockquote_depth = blockquote_depth.saturating_sub(1);
                    lines.push(Line::from(Span::raw("")));
                }
                TagEnd::List(_) => {
                    list_stack.pop();
                    pending_prefix = None;
                    current_prefix = None;
                    lines.push(Line::from(Span::raw("")));
                }
                TagEnd::Item => {
                    flush_line(&mut lines, &mut current);
                }
                TagEnd::Link => {
                    if style_stack.len() > 1 {
                        style_stack.pop();
                    }
                }
                TagEnd::TableCell => {
                    current_row.push(current_cell.trim().to_string());
                    current_cell.clear();
                    in_table_cell = false;
                }
                TagEnd::TableRow => {
                    if !current_row.is_empty() {
                        table_rows.push(current_row.clone());
                        if in_table_head {
                            table_header_rows += 1;
                        }
                    }
                    current_row.clear();
                }
                TagEnd::TableHead => {
                    if !current_row.is_empty() {
                        table_rows.push(current_row.clone());
                        table_header_rows += 1;
                        current_row.clear();
                    }
                    in_table_head = false;
                }
                TagEnd::Table => {
                    if !table_rows.is_empty() {
                        render_table_lines(
                            &table_rows,
                            &table_align,
                            table_header_rows,
                            &mut lines,
                        );
                        lines.push(Line::from(Span::raw("")));
                    }
                }
                _ => {}
            },
            MdEvent::Text(text) => {
                if in_code_block {
                    if code_block_lines.is_empty() {
                        code_block_lines.push(String::new());
                    }
                    for (idx, chunk) in text.split('\n').enumerate() {
                        if idx > 0 {
                            code_block_lines.push(String::new());
                        }
                        if let Some(last) = code_block_lines.last_mut() {
                            last.push_str(chunk);
                        }
                    }
                } else if in_table_cell {
                    current_cell.push_str(&text);
                } else {
                    ensure_prefix(
                        &mut current,
                        &mut pending_prefix,
                        &current_prefix,
                        &list_stack,
                        blockquote_depth,
                    );
                    let style = *style_stack.last().unwrap_or(&Style::default());
                    current.push(Span::styled(text.to_string(), style));
                }
            }
            MdEvent::Code(code) => {
                if in_code_block {
                    if code_block_lines.is_empty() {
                        code_block_lines.push(String::new());
                    }
                    if let Some(last) = code_block_lines.last_mut() {
                        last.push_str(&code);
                    }
                    continue;
                } else if in_table_cell {
                    current_cell.push_str(&code);
                    continue;
                }
                ensure_prefix(
                    &mut current,
                    &mut pending_prefix,
                    &current_prefix,
                    &list_stack,
                    blockquote_depth,
                );
                let style = style_stack
                    .last()
                    .copied()
                    .unwrap_or_default()
                    .fg(theme.warn);
                current.push(Span::styled(code.to_string(), style));
            }
            MdEvent::SoftBreak | MdEvent::HardBreak => {
                if in_code_block {
                    code_block_lines.push(String::new());
                } else if in_table_cell {
                    current_cell.push(' ');
                } else {
                    flush_line(&mut lines, &mut current);
                }
            }
            MdEvent::Rule => {
                flush_line(&mut lines, &mut current);
                lines.push(Line::from(Span::raw("\u{2500}".repeat(20))));
                lines.push(Line::from(Span::raw("")));
            }
            _ => {}
        }
    }

    if !current.is_empty() {
        lines.push(Line::from(current));
    }
    // Trim trailing empty lines
    while matches!(lines.last(), Some(line) if line.to_string().trim().is_empty()) {
        lines.pop();
    }
    lines
}

// ---------------------------------------------------------------------------
// Code block rendering with syntect highlighting
// ---------------------------------------------------------------------------

/// Render code block content with a box-drawing border and optional syntax
/// highlighting.
fn render_code_block_lines(lines: &[String], lang: Option<&str>, width: u16) -> Vec<Line<'static>> {
    let theme = &THEME;
    let mut out = Vec::new();
    let mut content_width = 1usize;
    for line in lines {
        content_width = content_width.max(UnicodeWidthStr::width(line.as_str()));
    }
    let label = lang
        .map(|l| format!(" {l} "))
        .filter(|s| !s.trim().is_empty());
    let available_width = width.saturating_sub(2) as usize;
    let outer_width = if available_width > 0 {
        available_width.max(6)
    } else {
        0
    };
    let inner_width = if outer_width > 2 {
        outer_width - 2
    } else {
        content_width.saturating_add(2)
    };
    let content_width = if inner_width > 2 {
        inner_width - 2
    } else {
        content_width
    };

    let border_style = Style::default().fg(theme.panel_border);
    let (syntax_set, syntect_theme) = syntect_assets();
    let syntax = lang
        .map(|l| l.to_lowercase())
        .and_then(|l| syntax_set.find_syntax_by_token(&l))
        .unwrap_or_else(|| syntax_set.find_syntax_plain_text());
    let mut highlighter = HighlightLines::new(syntax, syntect_theme);
    if let Some(label_text) = label {
        let mut label_text = label_text;
        let mut label_width = UnicodeWidthStr::width(label_text.as_str());
        if label_width > inner_width {
            label_text = truncate_to_width(&label_text, inner_width);
            label_width = UnicodeWidthStr::width(label_text.as_str());
        }
        let dash_count = inner_width.saturating_sub(label_width);
        let mut spans = Vec::new();
        spans.push(Span::styled("\u{256D}", border_style));
        spans.push(Span::styled("\u{2500}".repeat(dash_count), border_style));
        spans.push(Span::raw(label_text));
        spans.push(Span::styled("\u{256E}", border_style));
        out.push(Line::from(spans));
    } else {
        out.push(Line::from(Span::styled(
            format!("\u{256D}{}\u{256E}", "\u{2500}".repeat(inner_width)),
            border_style,
        )));
    }

    for line in lines {
        let trimmed = truncate_to_width(line, content_width);
        let trimmed_width = UnicodeWidthStr::width(trimmed.as_str());
        let pad = content_width.saturating_sub(trimmed_width);
        let regions = highlighter
            .highlight_line(&trimmed, syntax_set)
            .unwrap_or_default();
        let mut spans = Vec::new();
        spans.push(Span::styled("\u{2502}", border_style));
        spans.push(Span::raw(" "));
        if regions.is_empty() {
            spans.push(Span::styled(
                trimmed.clone(),
                Style::default().fg(theme.text),
            ));
        } else {
            for (style, text) in regions {
                let mut span_style = Style::default().fg(Color::Rgb(
                    style.foreground.r,
                    style.foreground.g,
                    style.foreground.b,
                ));
                if style.font_style.contains(FontStyle::BOLD) {
                    span_style = span_style.add_modifier(Modifier::BOLD);
                }
                if style.font_style.contains(FontStyle::ITALIC) {
                    span_style = span_style.add_modifier(Modifier::ITALIC);
                }
                if style.font_style.contains(FontStyle::UNDERLINE) {
                    span_style = span_style.add_modifier(Modifier::UNDERLINED);
                }
                spans.push(Span::styled(text.to_string(), span_style));
            }
        }
        spans.push(Span::raw(" ".repeat(pad)));
        spans.push(Span::raw(" "));
        spans.push(Span::styled("\u{2502}", border_style));
        out.push(Line::from(spans));
    }

    out.push(Line::from(Span::styled(
        format!("\u{2570}{}\u{256F}", "\u{2500}".repeat(inner_width)),
        border_style,
    )));
    out
}

// ---------------------------------------------------------------------------
// Table rendering
// ---------------------------------------------------------------------------

/// Render a table as box-drawn lines.
fn render_table_lines(
    rows: &[Vec<String>],
    align: &[Alignment],
    header_rows: usize,
    lines: &mut Vec<Line<'static>>,
) {
    let mut col_count = 0usize;
    for row in rows {
        col_count = col_count.max(row.len());
    }
    if col_count == 0 {
        return;
    }
    let mut widths = vec![3usize; col_count];
    for row in rows {
        for (idx, cell) in row.iter().enumerate() {
            let w = UnicodeWidthStr::width(cell.as_str());
            widths[idx] = widths[idx].max(w);
        }
    }

    let make_border = |left: char, mid: char, right: char| {
        let mut line = String::new();
        line.push(left);
        for col in 0..col_count {
            let segment = "\u{2500}".repeat(widths[col] + 2);
            line.push_str(&segment);
            if col + 1 < col_count {
                line.push(mid);
            }
        }
        line.push(right);
        line
    };

    let top = make_border('\u{256D}', '\u{252C}', '\u{256E}');
    lines.push(Line::from(Span::raw(top)));

    for (row_idx, row) in rows.iter().enumerate() {
        let mut line = String::new();
        for col in 0..col_count {
            let cell = row.get(col).map(|s| s.as_str()).unwrap_or("");
            let width = widths[col];
            let cell_width = UnicodeWidthStr::width(cell);
            let pad = width.saturating_sub(cell_width);
            let (pad_left, pad_right) = match align.get(col).copied().unwrap_or(Alignment::Left) {
                Alignment::Right => (pad, 0),
                Alignment::Center => (pad / 2, pad - pad / 2),
                _ => (0, pad),
            };
            line.push('\u{2502}');
            line.push(' ');
            line.push_str(&" ".repeat(pad_left));
            line.push_str(cell);
            line.push_str(&" ".repeat(pad_right));
            line.push(' ');
        }
        line.push('\u{2502}');
        if row_idx < header_rows {
            lines.push(Line::from(Span::styled(
                line,
                Style::default().add_modifier(Modifier::BOLD),
            )));
        } else {
            lines.push(Line::from(Span::raw(line)));
        }

        if row_idx + 1 == header_rows {
            let sep = make_border('\u{251C}', '\u{253C}', '\u{2524}');
            lines.push(Line::from(Span::raw(sep)));
        } else if row_idx + 1 < rows.len() {
            let sep = make_border('\u{251C}', '\u{253C}', '\u{2524}');
            lines.push(Line::from(Span::raw(sep)));
        }
    }

    let bottom = make_border('\u{2570}', '\u{2534}', '\u{256F}');
    lines.push(Line::from(Span::raw(bottom)));
}

// ---------------------------------------------------------------------------
// Thinking block rendering
// ---------------------------------------------------------------------------

/// Render thinking content inside a bordered box with a "思考" label.
fn format_thinking_block_lines(text: &str, width: u16) -> Vec<Line<'static>> {
    let theme = &THEME;
    let mut box_width = width.saturating_sub(6) as usize;
    if box_width < 12 {
        box_width = 12;
    }
    let inner_width = box_width.saturating_sub(2);
    let content_limit = inner_width.saturating_sub(2);
    let mut content_lines: Vec<String> = text.lines().map(|l| l.trim_end().to_string()).collect();
    while content_lines.first().is_some_and(|s| s.trim().is_empty()) {
        content_lines.remove(0);
    }
    while content_lines.last().is_some_and(|s| s.trim().is_empty()) {
        content_lines.pop();
    }
    if content_lines.is_empty() {
        content_lines.push("（无思考内容）".to_string());
    }
    let mut wrapped_lines = Vec::new();
    for line in content_lines {
        wrapped_lines.extend(wrap_text_to_width(&line, content_limit.max(1)));
    }

    let border_style = Style::default().fg(theme.panel_border);
    let title_style = Style::default()
        .fg(theme.accent_dim)
        .add_modifier(Modifier::BOLD);
    let text_style = Style::default().fg(theme.text_muted);

    let label = " 思考 ";
    let mut label_text = label.to_string();
    let mut label_width = UnicodeWidthStr::width(label_text.as_str());
    if label_width > inner_width {
        label_text = truncate_to_width(&label_text, inner_width);
        label_width = UnicodeWidthStr::width(label_text.as_str());
    }
    let dash_total = inner_width.saturating_sub(label_width);
    let left_dash = dash_total / 2;
    let right_dash = dash_total - left_dash;

    let mut out = Vec::new();
    let mut top = Vec::new();
    top.push(Span::styled("\u{256D}", border_style));
    top.push(Span::styled("\u{2500}".repeat(left_dash), border_style));
    top.push(Span::styled(label_text, title_style));
    top.push(Span::styled("\u{2500}".repeat(right_dash), border_style));
    top.push(Span::styled("\u{256E}", border_style));
    out.push(Line::from(top));

    for line in wrapped_lines {
        let line_width = UnicodeWidthStr::width(line.as_str());
        let pad = content_limit.saturating_sub(line_width);
        let mut spans = Vec::new();
        spans.push(Span::styled("\u{2502}", border_style));
        spans.push(Span::styled(" ", border_style));
        spans.push(Span::styled(line, text_style));
        spans.push(Span::styled(" ".repeat(pad), text_style));
        spans.push(Span::styled(" ", border_style));
        spans.push(Span::styled("\u{2502}", border_style));
        out.push(Line::from(spans));
    }

    out.push(Line::from(Span::styled(
        format!("\u{2570}{}\u{256F}", "\u{2500}".repeat(inner_width)),
        border_style,
    )));
    out
}

// ---------------------------------------------------------------------------
// Syntect assets
// ---------------------------------------------------------------------------

fn syntect_assets() -> (&'static SyntaxSet, &'static SyntectTheme) {
    static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();
    static THEME_SET: OnceLock<ThemeSet> = OnceLock::new();
    static THEME_NAME: &str = "base16-ocean.dark";
    let syntax_set = SYNTAX_SET.get_or_init(SyntaxSet::load_defaults_newlines);
    let theme_set = THEME_SET.get_or_init(ThemeSet::load_defaults);
    let syntect_theme = theme_set
        .themes
        .get(THEME_NAME)
        .or_else(|| theme_set.themes.values().next())
        .expect("syntect theme set is empty");
    (syntax_set, syntect_theme)
}

// ---------------------------------------------------------------------------
// Text utility helpers
// ---------------------------------------------------------------------------

fn dot_span() -> Span<'static> {
    let theme = &THEME;
    Span::styled("\u{25CF}", Style::default().fg(theme.accent))
}

fn truncate_to_width(text: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_string();
    }
    if max_width <= 3 {
        return text.chars().take(max_width).collect();
    }
    let mut out = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let w = UnicodeWidthStr::width(ch.to_string().as_str());
        if used + w > max_width - 3 {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push_str("...");
    out
}

fn wrap_text_to_width(text: &str, max_width: usize) -> Vec<String> {
    if max_width == 0 {
        return vec![String::new()];
    }
    let mut out = Vec::new();
    let mut current = String::new();
    let mut width = 0usize;
    for ch in text.chars() {
        let ch_width = UnicodeWidthStr::width(ch.to_string().as_str());
        if width + ch_width > max_width && !current.is_empty() {
            out.push(current);
            current = String::new();
            width = 0;
        }
        current.push(ch);
        width += ch_width;
    }
    if !current.is_empty() || out.is_empty() {
        out.push(current);
    }
    out
}
