use std::path::Path;
use std::time::Duration;

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};
use ratatui_textarea::TextArea;
use unicode_width::UnicodeWidthChar;

use super::status_line::{format_elapsed_human, format_tokens};
use super::theme::Theme;

/// Overhead rows above the textarea: 1 border + 1 context line.
const INPUT_OVERHEAD: u16 = 2;
/// Minimum textarea rows.
const MIN_TEXTAREA_ROWS: u16 = 3;
/// Minimum total input height: overhead + min textarea rows.
pub const MIN_INPUT_HEIGHT: u16 = INPUT_OVERHEAD + MIN_TEXTAREA_ROWS; // 5
/// Max percentage of terminal height the input can consume.
pub const MAX_INPUT_PCT: u16 = 40;
/// Width of the chevron prompt ("> ").
pub const CHEVRON_WIDTH: u16 = 2;

/// The current agent mode. Placeholder until agent module is built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentMode {
    Build,
    Plan,
}

impl AgentMode {
    pub fn display_name(&self) -> &str {
        match self {
            AgentMode::Build => "Build",
            AgentMode::Plan => "Plan",
        }
    }

    pub fn toggle(&self) -> AgentMode {
        match self {
            AgentMode::Build => AgentMode::Plan,
            AgentMode::Plan => AgentMode::Build,
        }
    }
}

/// Context information displayed above the input textarea.
pub struct InputContext {
    pub working_dir: String,
    pub last_prompt_tokens: u64,
    pub context_window: u64,
    pub context_usage_pct: u8,
    /// Total elapsed time for current/last streaming request.
    pub elapsed: Option<Duration>,
}

/// A multi-line paste that has been visually collapsed into a summary.
///
/// The textarea shows a compact `[N lines; Mb pasted]` string while
/// `full_text` preserves the original content for submission.
#[derive(Debug, Clone)]
pub struct CollapsedPaste {
    pub full_text: String,
    pub summary: String,
}

impl CollapsedPaste {
    pub fn new(full_text: String) -> Self {
        // split('\n') counts trailing newlines correctly (unlike lines())
        let line_count = full_text.split('\n').count();
        let byte_count = full_text.len();
        let summary = if byte_count >= 1024 {
            format!(
                "[{} lines; {:.1}kb pasted]",
                line_count,
                byte_count as f64 / 1024.0
            )
        } else {
            format!("[{} lines; {}b pasted]", line_count, byte_count)
        };
        Self { full_text, summary }
    }
}

/// State for the input area.
pub struct InputState {
    pub textarea: TextArea<'static>,
    pub mode: AgentMode,
    /// Vertical scroll offset for the wrapped textarea rendering.
    pub scroll_offset: u16,
    /// When set, the textarea shows a summary instead of raw pasted text.
    pub collapsed_paste: Option<CollapsedPaste>,
    /// Whether the paste preview overlay is visible (Ctrl+P toggle).
    pub paste_preview_visible: bool,
}

impl Default for InputState {
    fn default() -> Self {
        let mut textarea = TextArea::default();
        textarea.set_cursor_line_style(Style::default());
        textarea.set_placeholder_text("Type a message...");
        Self {
            textarea,
            mode: AgentMode::Build,
            scroll_offset: 0,
            collapsed_paste: None,
            paste_preview_visible: false,
        }
    }
}

/// Count the total visual lines needed to display `lines` at the given `width`.
///
/// Each logical line wraps at character boundaries based on unicode display width.
/// Empty lines count as 1 visual row.
pub fn count_visual_lines(lines: &[String], width: usize) -> u16 {
    if width == 0 {
        return lines.len().max(1) as u16;
    }
    let total: u32 = lines
        .iter()
        .map(|line| {
            let line_width: usize = line.chars().map(|c| UnicodeWidthChar::width(c).unwrap_or(0)).sum();
            if line_width == 0 {
                1u32
            } else {
                ((line_width + width - 1) / width) as u32
            }
        })
        .sum();
    total.min(u16::MAX as u32).max(1) as u16
}

/// Result of wrapping a single logical line for rendering.
struct WrappedLine {
    /// The visual lines (ratatui `Line`s) produced by wrapping.
    visual_lines: Vec<Line<'static>>,
    /// If cursor was on this logical line, the visual row within `visual_lines`.
    cursor_visual_row: Option<usize>,
}

/// Wrap a logical line into visual lines at character boundaries,
/// rendering a cursor at the given column if `cursor_col` is `Some`.
fn wrap_line_with_cursor(
    line: &str,
    width: usize,
    cursor_col: Option<usize>,
    normal_style: Style,
    cursor_style: Style,
) -> WrappedLine {
    if width == 0 {
        // Degenerate case: render the whole line as one visual line
        let spans = if let Some(col) = cursor_col {
            build_cursor_spans(line, col, normal_style, cursor_style)
        } else {
            vec![Span::styled(line.to_string(), normal_style)]
        };
        return WrappedLine {
            visual_lines: vec![Line::from(spans)],
            cursor_visual_row: cursor_col.map(|_| 0),
        };
    }

    let mut visual_lines: Vec<Line<'static>> = Vec::new();
    let mut current_chars: Vec<char> = Vec::new();
    let mut current_width: usize = 0;
    let char_count = line.chars().count();
    let mut cursor_visual_row: Option<usize> = None;
    // Track which visual row each char index starts on and its column
    let mut char_visual_positions: Vec<(usize, usize)> = Vec::new(); // (visual_row, visual_col)

    for ch in line.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);

        // Check if adding this char would exceed width
        if current_width + ch_width > width && !current_chars.is_empty() {
            // Flush current visual line
            let text: String = current_chars.drain(..).collect();
            visual_lines.push(Line::from(Span::styled(text, normal_style)));
            current_width = 0;
        }

        char_visual_positions.push((visual_lines.len(), current_width));
        current_chars.push(ch);
        current_width += ch_width;
    }

    // Flush remaining chars
    if !current_chars.is_empty() {
        let text: String = current_chars.into_iter().collect();
        visual_lines.push(Line::from(Span::styled(text, normal_style)));
    }

    // Handle empty line
    if visual_lines.is_empty() {
        visual_lines.push(Line::from(Span::styled("", normal_style)));
    }

    // Now handle cursor rendering
    if let Some(col) = cursor_col {
        if col >= char_count {
            // Cursor at EOL — compute the display width of the last visual line
            let last_row = visual_lines.len() - 1;
            let last_line_width: usize = visual_lines[last_row]
                .spans
                .iter()
                .flat_map(|s| s.content.chars())
                .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
                .sum();

            if last_line_width >= width {
                // Cursor wraps to a new visual line
                visual_lines.push(Line::from(Span::styled(" ", cursor_style)));
                cursor_visual_row = Some(visual_lines.len() - 1);
            } else {
                // Append cursor space to last line
                let last_line_text: String = visual_lines[last_row]
                    .spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect();
                let mut spans = vec![Span::styled(last_line_text, normal_style)];
                spans.push(Span::styled(" ", cursor_style));
                visual_lines[last_row] = Line::from(spans);
                cursor_visual_row = Some(last_row);
            }
        } else {
            // Cursor within text
            let (vis_row, _vis_col) = char_visual_positions[col];
            cursor_visual_row = Some(vis_row);

            // Re-render just that visual line with cursor highlighting
            let line_text: String = visual_lines[vis_row]
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect();

            // Find the char offset within this visual line
            let line_start_char = if vis_row == 0 {
                0
            } else {
                // Index of first char on this visual row within the logical line
                char_visual_positions
                    .iter()
                    .position(|(r, _)| *r == vis_row)
                    .unwrap_or(0)
            };
            let col_in_line = col - line_start_char;

            let spans = build_cursor_spans(&line_text, col_in_line, normal_style, cursor_style);
            visual_lines[vis_row] = Line::from(spans);
        }
    }

    WrappedLine {
        visual_lines,
        cursor_visual_row,
    }
}

/// Build spans for a line with an inverted cursor at `cursor_col`.
fn build_cursor_spans(
    text: &str,
    cursor_col: usize,
    normal_style: Style,
    cursor_style: Style,
) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    let mut spans = Vec::new();

    if cursor_col > 0 {
        let before: String = chars[..cursor_col].iter().collect();
        spans.push(Span::styled(before, normal_style));
    }

    if cursor_col < chars.len() {
        spans.push(Span::styled(chars[cursor_col].to_string(), cursor_style));
        if cursor_col + 1 < chars.len() {
            let after: String = chars[cursor_col + 1..].iter().collect();
            spans.push(Span::styled(after, normal_style));
        }
    } else {
        // Cursor past end — show cursor as space
        spans.push(Span::styled(" ", cursor_style));
    }

    spans
}

impl InputState {
    /// Desired input area height based on current content.
    /// Capped between `MIN_INPUT_HEIGHT` and `max_height`.
    /// `available_width` is the textarea column width for wrapping calculations.
    pub fn desired_height(&self, max_height: u16, available_width: u16) -> u16 {
        let cap = max_height.max(MIN_INPUT_HEIGHT);
        let lines: Vec<String> = self.textarea.lines().iter().map(|s| s.to_string()).collect();
        let width = available_width as usize;
        let mut visual_rows = count_visual_lines(&lines, width);

        // Account for EOL cursor overflow: when the cursor is at the end of a
        // line that exactly fills the available width, the cursor wraps to a new
        // visual row. Reserve that extra row so the input box doesn't scroll.
        if width > 0 {
            let (cursor_row, cursor_col) = self.textarea.cursor();
            if let Some(cursor_line) = lines.get(cursor_row) {
                let char_count = cursor_line.chars().count();
                if cursor_col >= char_count {
                    let line_width: usize = cursor_line
                        .chars()
                        .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
                        .sum();
                    if line_width > 0 && line_width % width == 0 {
                        visual_rows = visual_rows.saturating_add(1);
                    }
                }
            }
        }

        let textarea_rows = visual_rows.max(MIN_TEXTAREA_ROWS);
        let total = INPUT_OVERHEAD + textarea_rows;
        total.clamp(MIN_INPUT_HEIGHT, cap)
    }

    /// Replace the current text in the textarea (for autocomplete insertion).
    pub fn set_text(&mut self, text: &str) {
        self.collapsed_paste = None;
        self.paste_preview_visible = false;
        let mut textarea = TextArea::default();
        textarea.set_cursor_line_style(Style::default());
        textarea.set_placeholder_text("Type a message...");
        // Insert the new text
        textarea.insert_str(text);
        self.textarea = textarea;
        self.scroll_offset = 0;
    }

    /// Take the current text and clear the input.
    ///
    /// If a collapsed paste is active, returns the full pasted content
    /// (combined with any text that preceded the paste).
    pub fn take_text(&mut self) -> String {
        let text = if let Some(paste) = self.collapsed_paste.take() {
            paste.full_text
        } else {
            let lines = self.textarea.lines().to_vec();
            lines.join("\n")
        };
        // Clear by replacing with a fresh textarea
        let mut textarea = TextArea::default();
        textarea.set_cursor_line_style(Style::default());
        textarea.set_placeholder_text("Type a message...");
        self.textarea = textarea;
        self.scroll_offset = 0;
        self.paste_preview_visible = false;
        text
    }

    /// Whether a collapsed paste is currently active.
    pub fn is_collapsed(&self) -> bool {
        self.collapsed_paste.is_some()
    }

    /// Collapse a multi-line paste into a summary displayed in the textarea.
    ///
    /// Single-line pastes (< 2 lines) are inserted normally. Multi-line pastes
    /// replace the textarea content with a compact summary while preserving the
    /// full text for submission.
    pub fn collapse_paste(&mut self, pasted_text: &str) {
        let line_count = pasted_text.split('\n').count();
        if line_count < 2 {
            self.textarea.insert_str(pasted_text);
            return;
        }

        // If already collapsed, expand first to restore real content
        if self.collapsed_paste.is_some() {
            self.expand_paste();
        }

        // Insert the pasted text to combine with existing content
        self.textarea.insert_str(pasted_text);

        // Snapshot the full content, then replace textarea with summary
        let full_text = self.textarea.lines().to_vec().join("\n");
        let collapsed = CollapsedPaste::new(full_text);
        let summary = collapsed.summary.clone();
        self.collapsed_paste = Some(collapsed);

        // Replace textarea with summary
        let mut textarea = TextArea::default();
        textarea.set_cursor_line_style(Style::default());
        textarea.set_placeholder_text("Type a message...");
        textarea.insert_str(&summary);
        self.textarea = textarea;
        self.scroll_offset = 0;
    }

    /// Restore the full pasted text to the textarea, clearing collapsed state.
    pub fn expand_paste(&mut self) {
        if let Some(paste) = self.collapsed_paste.take() {
            self.paste_preview_visible = false;
            let mut textarea = TextArea::default();
            textarea.set_cursor_line_style(Style::default());
            textarea.set_placeholder_text("Type a message...");
            textarea.insert_str(&paste.full_text);
            self.textarea = textarea;
            self.scroll_offset = 0;
        }
    }
}

/// Replace $HOME prefix with `~` for display.
pub fn abbreviate_path(path: &Path) -> String {
    if let Ok(home) = std::env::var("HOME") {
        let home_path = Path::new(&home);
        if let Ok(suffix) = path.strip_prefix(home_path) {
            return format!("~/{}", suffix.display());
        }
    }
    path.display().to_string()
}

/// Render the textarea with custom line wrapping and cursor.
///
/// Replaces `TextArea`'s built-in rendering with a pre-wrapped `Paragraph`
/// so that long lines wrap visually instead of scrolling horizontally.
fn render_wrapped_textarea(
    frame: &mut Frame,
    area: Rect,
    state: &mut InputState,
    theme: &Theme,
) {
    let width = area.width as usize;
    let lines = state.textarea.lines();
    let (cursor_row, cursor_col) = state.textarea.cursor();

    let normal_style = if state.is_collapsed() {
        Style::default().fg(theme.dim)
    } else {
        Style::default().fg(theme.fg)
    };
    let cursor_style = Style::default().fg(theme.bg).bg(theme.fg);

    // Check if textarea is empty (single empty line)
    if lines.len() == 1 && lines[0].is_empty() {
        let placeholder_style = Style::default().fg(theme.dim);
        let mut all_lines: Vec<Line<'static>> = vec![Line::from(vec![
            Span::styled("Type a message...", placeholder_style),
        ])];
        // Show cursor at position 0
        all_lines[0] = Line::from(vec![
            Span::styled(" ", cursor_style),
            Span::styled("ype a message...", placeholder_style),
        ]);
        let para = Paragraph::new(all_lines);
        frame.render_widget(para, area);
        state.scroll_offset = 0;
        return;
    }

    let mut all_visual_lines: Vec<Line<'static>> = Vec::new();
    let mut cursor_visual_row: u16 = 0;

    for (i, line) in lines.iter().enumerate() {
        let cur_col = if i == cursor_row {
            Some(cursor_col)
        } else {
            None
        };

        let wrapped = wrap_line_with_cursor(line, width, cur_col, normal_style, cursor_style);

        if i == cursor_row {
            if let Some(row_in_wrapped) = wrapped.cursor_visual_row {
                cursor_visual_row = all_visual_lines.len() as u16 + row_in_wrapped as u16;
            }
        }

        all_visual_lines.extend(wrapped.visual_lines);
    }

    // Adjust scroll to keep cursor visible
    let visible_rows = area.height;
    if cursor_visual_row < state.scroll_offset {
        state.scroll_offset = cursor_visual_row;
    }
    if visible_rows > 0 && cursor_visual_row >= state.scroll_offset.saturating_add(visible_rows) {
        state.scroll_offset = cursor_visual_row - visible_rows + 1;
    }

    let para = Paragraph::new(all_visual_lines).scroll((state.scroll_offset, 0));
    frame.render_widget(para, area);
}

/// Render the input area as a 2-line starship-style prompt.
///
/// ```text
/// Line 1 (context): [Build] ~/projects/steve              12k/128k (10%)
/// Line 2+ (input):  > type here...
/// ```
pub fn render_input(
    frame: &mut Frame,
    area: Rect,
    state: &mut InputState,
    theme: &Theme,
    context: &InputContext,
) {
    // Top border for visual separation from message area
    let border_block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(theme.border_color(context.context_usage_pct)));
    let inner_area = border_block.inner(area);
    frame.render_widget(border_block, area);

    // Split vertically: 1 row for context line, rest for textarea
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // context line
            Constraint::Min(1),   // textarea with chevron
        ])
        .split(inner_area);

    let context_area = vertical[0];
    let textarea_area = vertical[1];

    // -- Context line --
    let mode_color = match state.mode {
        AgentMode::Build => theme.mode_build,
        AgentMode::Plan => theme.mode_plan,
    };

    let mut left_spans: Vec<Span> = vec![
        Span::styled(
            format!(" {} ", state.mode.display_name()),
            Style::default()
                .fg(theme.bg)
                .bg(mode_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            context.working_dir.clone(),
            Style::default().fg(theme.dim),
        ),
    ];

    let mut right_spans: Vec<Span> = Vec::new();

    // Elapsed timer (shown during/after streaming)
    if let Some(elapsed) = context.elapsed {
        right_spans.push(Span::styled(
            format_elapsed_human(elapsed),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ));
    }

    // Token display
    if context.context_window > 0 {
        if !right_spans.is_empty() {
            right_spans.push(Span::styled(" · ", Style::default().fg(theme.dim)));
        }
        let pct = context.context_usage_pct;
        let token_color = theme.context_color(pct);
        right_spans.push(Span::styled(
            format!(
                "{}/{} ({}%)",
                format_tokens(context.last_prompt_tokens),
                format_tokens(context.context_window),
                pct,
            ),
            Style::default().fg(token_color),
        ));
    } else if context.last_prompt_tokens > 0 {
        if !right_spans.is_empty() {
            right_spans.push(Span::styled(" · ", Style::default().fg(theme.dim)));
        }
        right_spans.push(Span::styled(
            format_tokens(context.last_prompt_tokens),
            Style::default().fg(theme.dim),
        ));
    }

    // Calculate padding between left and right
    let left_width: usize = left_spans.iter().map(|s| s.width()).sum();
    let right_width: usize = right_spans.iter().map(|s| s.width()).sum();
    let available = context_area.width as usize;
    let padding = available.saturating_sub(left_width + right_width);

    left_spans.push(Span::raw(" ".repeat(padding)));
    left_spans.extend(right_spans);

    let context_line = Paragraph::new(Line::from(left_spans));
    frame.render_widget(context_line, context_area);

    // -- Input: chevron + textarea --
    let input_horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(CHEVRON_WIDTH), // "> "
            Constraint::Min(1),               // textarea
        ])
        .split(textarea_area);

    let chevron = Paragraph::new(Span::styled(
        "> ",
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
    ));
    frame.render_widget(chevron, input_horizontal[0]);

    render_wrapped_textarea(frame, input_horizontal[1], state, theme);
}

/// Maximum number of paste preview lines shown in the overlay.
const MAX_PASTE_PREVIEW_LINES: usize = 20;

/// Render the paste preview overlay centered in the message area.
///
/// Shows the first lines of a collapsed paste so the user can inspect
/// what was pasted without expanding it into the textarea.
pub fn render_paste_preview(
    frame: &mut Frame,
    message_area: Rect,
    state: &InputState,
    theme: &Theme,
    context_pct: u8,
) {
    // Only render when visible AND there's a collapsed paste to preview
    let paste = match (&state.collapsed_paste, state.paste_preview_visible) {
        (Some(paste), true) => paste,
        _ => return,
    };

    let lines: Vec<&str> = paste.full_text.lines().take(MAX_PASTE_PREVIEW_LINES).collect();
    let total_lines = paste.full_text.lines().count();
    let truncated = total_lines > MAX_PASTE_PREVIEW_LINES;

    // Build display lines
    let mut display_lines: Vec<Line<'_>> = lines
        .iter()
        .map(|line| Line::from(Span::styled(*line, Style::default().fg(theme.fg))))
        .collect();

    if truncated {
        display_lines.push(Line::from(Span::styled(
            format!("  ... ({} more lines)", total_lines - MAX_PASTE_PREVIEW_LINES),
            Style::default().fg(theme.dim),
        )));
    }

    // Calculate popup dimensions
    let content_height = display_lines.len() as u16;
    let popup_height = (content_height + 2).min(message_area.height.saturating_sub(2)); // +2 for borders
    let popup_width = 60u16.min(message_area.width.saturating_sub(4));

    if popup_width < 20 || popup_height < 5 {
        return;
    }

    // Center in message area
    let popup_x = message_area.x + (message_area.width.saturating_sub(popup_width)) / 2;
    let popup_y = message_area.y + (message_area.height.saturating_sub(popup_height)) / 2;

    let popup_area = Rect {
        x: popup_x,
        y: popup_y,
        width: popup_width,
        height: popup_height,
    };

    // Clear behind the popup
    frame.render_widget(Clear, popup_area);

    let border_style = Style::default().fg(theme.border_color(context_pct));
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(Line::from(vec![
            Span::styled(
                " Paste Preview ",
                Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
            ),
        ]))
        .title_bottom(Line::from(vec![
            Span::styled(" Ctrl+P to close ", Style::default().fg(theme.dim)),
        ]));

    let paragraph = Paragraph::new(display_lines)
        .block(block)
        .wrap(Wrap { trim: false });

    frame.render_widget(paragraph, popup_area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abbreviate_path_replaces_home() {
        if let Ok(home) = std::env::var("HOME") {
            let test_path = Path::new(&home).join("projects").join("steve");
            let result = abbreviate_path(&test_path);
            assert!(result.starts_with("~/"), "expected ~/ prefix, got: {result}");
            assert!(result.contains("projects/steve"));
        }
    }

    #[test]
    fn abbreviate_path_no_home_prefix() {
        let path = Path::new("/tmp/something");
        let result = abbreviate_path(path);
        assert_eq!(result, "/tmp/something");
    }

    #[test]
    fn mode_toggle() {
        assert_eq!(AgentMode::Build.toggle(), AgentMode::Plan);
        assert_eq!(AgentMode::Plan.toggle(), AgentMode::Build);
    }

    #[test]
    fn mode_display_name() {
        assert_eq!(AgentMode::Build.display_name(), "Build");
        assert_eq!(AgentMode::Plan.display_name(), "Plan");
    }

    #[test]
    fn take_text_clears_input() {
        let mut state = InputState::default();
        state.textarea.insert_str("hello world");
        let text = state.take_text();
        assert_eq!(text, "hello world");
        assert_eq!(state.textarea.lines().join(""), "");
    }

    // -- Buffer rendering tests --

    use ratatui::layout::Rect;

    /// Helper: render input area into a buffer and return the buffer + text string.
    fn render_input_to_parts(
        width: u16,
        height: u16,
        mode: AgentMode,
        pct: u8,
        last_prompt: u64,
        ctx_window: u64,
    ) -> (ratatui::buffer::Buffer, String) {
        let theme = Theme::default();
        let mut state = InputState::default();
        state.mode = mode;
        let context = InputContext {
            working_dir: "~/projects/steve".to_string(),
            last_prompt_tokens: last_prompt,
            context_window: ctx_window,
            context_usage_pct: pct,
            elapsed: None,
        };
        let buf = super::super::render_to_buffer(width, height, |frame| {
            render_input(
                frame,
                Rect::new(0, 0, width, height),
                &mut state,
                &theme,
                &context,
            );
        });
        let mut text = String::new();
        for y in 0..height {
            for x in 0..width {
                let cell = &buf[(x, y)];
                text.push_str(cell.symbol());
            }
            text.push('\n');
        }
        (buf, text)
    }

    #[test]
    fn buffer_build_mode_badge() {
        let (buf, text) = render_input_to_parts(80, 5, AgentMode::Build, 10, 12800, 128000);
        assert!(text.contains("Build"), "should show Build mode badge");
        // Find the "B" of "Build" and check it has the mode_build background color
        let theme = Theme::default();
        for x in 0..80 {
            let cell = &buf[(x, 1)]; // context line is row 1 (after border)
            if cell.symbol() == "B" {
                assert_eq!(cell.bg, theme.mode_build, "Build badge should have mode_build bg color");
                break;
            }
        }
    }

    #[test]
    fn buffer_plan_mode_badge() {
        let (buf, text) = render_input_to_parts(80, 5, AgentMode::Plan, 10, 12800, 128000);
        assert!(text.contains("Plan"), "should show Plan mode badge");
        let theme = Theme::default();
        for x in 0..80 {
            let cell = &buf[(x, 1)];
            if cell.symbol() == "P" {
                assert_eq!(cell.bg, theme.mode_plan, "Plan badge should have mode_plan bg color");
                break;
            }
        }
    }

    #[test]
    fn buffer_context_pressure_green() {
        let (buf, text) = render_input_to_parts(80, 5, AgentMode::Build, 30, 38400, 128000);
        assert!(text.contains("30%"), "should show 30%");
        let theme = Theme::default();
        // Find the "3" of "30%" and check color is dim (green = low pressure)
        for x in (0..80).rev() {
            let cell = &buf[(x, 1)];
            if cell.symbol() == "3" {
                assert_eq!(cell.fg, theme.dim, "30% should use dim color (low pressure)");
                break;
            }
        }
    }

    #[test]
    fn buffer_context_pressure_amber() {
        let (buf, text) = render_input_to_parts(80, 5, AgentMode::Build, 50, 64000, 128000);
        assert!(text.contains("50%"), "should show 50%");
        let theme = Theme::default();
        // Find the "5" of "50%" and check color is amber-brown (40-59% tier)
        let mut found = false;
        for x in (0..80).rev() {
            let cell = &buf[(x, 1)];
            if cell.symbol() == "5" {
                assert_eq!(cell.fg, theme.context_amber, "50% should use amber-brown color");
                found = true;
                break;
            }
        }
        assert!(found, "should find '5' digit in buffer for 50% context pressure");
    }

    #[test]
    fn buffer_context_pressure_yellow() {
        let (buf, text) = render_input_to_parts(80, 5, AgentMode::Build, 60, 76800, 128000);
        assert!(text.contains("60%"), "should show 60%");
        let theme = Theme::default();
        for x in (0..80).rev() {
            let cell = &buf[(x, 1)];
            if cell.symbol() == "6" {
                assert_eq!(cell.fg, theme.warning, "60% should use warning color");
                break;
            }
        }
    }

    #[test]
    fn buffer_context_pressure_red() {
        let (buf, text) = render_input_to_parts(80, 5, AgentMode::Build, 85, 108800, 128000);
        assert!(text.contains("85%"), "should show 85%");
        let theme = Theme::default();
        for x in (0..80).rev() {
            let cell = &buf[(x, 1)];
            if cell.symbol() == "8" {
                assert_eq!(cell.fg, theme.error, "85% should use error color (red)");
                break;
            }
        }
    }

    // -- desired_height tests --

    #[test]
    fn desired_height_empty_textarea() {
        let state = InputState::default();
        assert_eq!(state.desired_height(20, 80), MIN_INPUT_HEIGHT);
    }

    #[test]
    fn desired_height_single_line() {
        let mut state = InputState::default();
        state.textarea.insert_str("hello");
        assert_eq!(state.desired_height(20, 80), MIN_INPUT_HEIGHT);
    }

    #[test]
    fn desired_height_six_lines() {
        let mut state = InputState::default();
        // Insert 6 lines via newline-separated text
        state.textarea.insert_str("1\n2\n3\n4\n5\n6");
        assert_eq!(state.textarea.lines().len(), 6, "insert_str must split on newlines");
        // 2 overhead + 6 lines = 8
        assert_eq!(state.desired_height(20, 80), 8);
    }

    #[test]
    fn desired_height_clamped_to_max() {
        let mut state = InputState::default();
        // Insert 30 lines
        let text = (1..=30).map(|i| i.to_string()).collect::<Vec<_>>().join("\n");
        state.textarea.insert_str(&text);
        // 2 + 30 = 32, but max_height is 12
        assert_eq!(state.desired_height(12, 80), 12);
    }

    #[test]
    fn desired_height_max_equals_min() {
        let state = InputState::default();
        assert_eq!(state.desired_height(MIN_INPUT_HEIGHT, 80), MIN_INPUT_HEIGHT);
    }

    #[test]
    fn desired_height_max_below_min_does_not_panic() {
        let state = InputState::default();
        // max_height < MIN_INPUT_HEIGHT should return MIN_INPUT_HEIGHT, not panic
        assert_eq!(state.desired_height(2, 80), MIN_INPUT_HEIGHT);
    }

    #[test]
    fn desired_height_after_take_text() {
        let mut state = InputState::default();
        state.textarea.insert_str("1\n2\n3\n4\n5\n6");
        assert_eq!(state.desired_height(20, 80), 8);
        let _ = state.take_text();
        assert_eq!(state.desired_height(20, 80), MIN_INPUT_HEIGHT);
    }

    #[test]
    fn desired_height_wraps_long_line() {
        let mut state = InputState::default();
        // 100-char line at width 50 → 2 visual rows
        state.textarea.insert_str(&"x".repeat(100));
        // 2 overhead + max(2, 3 MIN_TEXTAREA_ROWS) = 5
        assert_eq!(state.desired_height(20, 50), MIN_INPUT_HEIGHT);
        // 200-char line at width 50 → 4 visual rows + 1 EOL cursor overflow = 5
        let mut state2 = InputState::default();
        state2.textarea.insert_str(&"x".repeat(200));
        // 2 overhead + 5 visual rows = 7
        assert_eq!(state2.desired_height(20, 50), 7);
    }

    #[test]
    fn desired_height_width_zero_no_panic() {
        let mut state = InputState::default();
        state.textarea.insert_str("hello");
        // Should not panic with width 0
        let h = state.desired_height(20, 0);
        assert!(h >= MIN_INPUT_HEIGHT);
    }

    // -- count_visual_lines tests --

    #[test]
    fn count_visual_lines_empty() {
        let lines = vec!["".to_string()];
        assert_eq!(count_visual_lines(&lines, 80), 1);
    }

    #[test]
    fn count_visual_lines_short_lines() {
        let lines = vec!["hello".to_string(), "world".to_string()];
        assert_eq!(count_visual_lines(&lines, 80), 2);
    }

    #[test]
    fn count_visual_lines_exact_width() {
        let lines = vec!["x".repeat(50)];
        assert_eq!(count_visual_lines(&lines, 50), 1);
    }

    #[test]
    fn count_visual_lines_wraps() {
        let lines = vec!["x".repeat(120)];
        assert_eq!(count_visual_lines(&lines, 50), 3); // ceil(120/50) = 3
    }

    #[test]
    fn count_visual_lines_width_zero() {
        let lines = vec!["hello".to_string(), "world".to_string()];
        assert_eq!(count_visual_lines(&lines, 0), 2);
    }

    // -- wrap_line_with_cursor tests --

    #[test]
    fn wrap_line_no_cursor_short() {
        let normal = Style::default();
        let cursor = Style::default().add_modifier(Modifier::REVERSED);
        let result = wrap_line_with_cursor("hello", 80, None, normal, cursor);
        assert_eq!(result.visual_lines.len(), 1);
        assert!(result.cursor_visual_row.is_none());
    }

    #[test]
    fn wrap_line_cursor_at_start() {
        let normal = Style::default();
        let cursor = Style::default().add_modifier(Modifier::REVERSED);
        let result = wrap_line_with_cursor("hello", 80, Some(0), normal, cursor);
        assert_eq!(result.visual_lines.len(), 1);
        assert_eq!(result.cursor_visual_row, Some(0));
        // First span should be the cursor char 'h'
        assert_eq!(result.visual_lines[0].spans[0].content.as_ref(), "h");
        assert_eq!(result.visual_lines[0].spans[0].style, cursor);
    }

    #[test]
    fn wrap_line_cursor_at_eol() {
        let normal = Style::default();
        let cursor = Style::default().add_modifier(Modifier::REVERSED);
        let result = wrap_line_with_cursor("hi", 80, Some(2), normal, cursor);
        assert_eq!(result.visual_lines.len(), 1);
        assert_eq!(result.cursor_visual_row, Some(0));
        // Should have normal "hi" + cursor space
        assert_eq!(result.visual_lines[0].spans.len(), 2);
        assert_eq!(result.visual_lines[0].spans[1].content.as_ref(), " ");
        assert_eq!(result.visual_lines[0].spans[1].style, cursor);
    }

    #[test]
    fn wrap_line_wraps_at_width() {
        let normal = Style::default();
        let cursor = Style::default().add_modifier(Modifier::REVERSED);
        // 10 chars at width 5 → 2 visual lines
        let result = wrap_line_with_cursor("abcdefghij", 5, None, normal, cursor);
        assert_eq!(result.visual_lines.len(), 2);
    }

    #[test]
    fn wrap_line_cursor_on_second_visual_line() {
        let normal = Style::default();
        let cursor = Style::default().add_modifier(Modifier::REVERSED);
        // "abcdefghij" at width 5, cursor at col 7 ('h') → visual row 1
        let result = wrap_line_with_cursor("abcdefghij", 5, Some(7), normal, cursor);
        assert_eq!(result.cursor_visual_row, Some(1));
    }

    #[test]
    fn wrap_line_eol_cursor_wraps_to_new_line() {
        let normal = Style::default();
        let cursor = Style::default().add_modifier(Modifier::REVERSED);
        // Exactly 5 chars at width 5, cursor at col 5 (EOL) → wraps to row 1
        let result = wrap_line_with_cursor("abcde", 5, Some(5), normal, cursor);
        assert_eq!(result.visual_lines.len(), 2);
        assert_eq!(result.cursor_visual_row, Some(1));
    }

    #[test]
    fn desired_height_eol_cursor_overflow_reserves_extra_row() {
        let mut state = InputState::default();
        // Insert exactly 50 chars at width 50 — line fills exactly.
        // Cursor is at EOL (col 50), so it overflows to a new visual row.
        state.textarea.insert_str(&"x".repeat(50));
        // Without the fix: count_visual_lines gives 1, desired_height = 2 + max(1, 3) = 5
        // With the fix: 1 + 1 = 2 visual rows, desired_height = 2 + max(2, 3) = 5
        // Need more than MIN_TEXTAREA_ROWS to see the effect:
        // 200 chars at width 50 = 4 visual rows, cursor at EOL = 5 visual rows
        let mut state2 = InputState::default();
        state2.textarea.insert_str(&"x".repeat(200));
        // cursor at col 200 (EOL), line_width=200, 200%50=0 → overflow row
        assert_eq!(state2.desired_height(20, 50), 7); // 2 + 5 = 7
    }

    #[test]
    fn take_text_multiline_round_trip() {
        let mut state = InputState::default();
        state.textarea.insert_str("line1\nline2\nline3");
        let text = state.take_text();
        assert_eq!(text, "line1\nline2\nline3");
    }

    // -- CollapsedPaste tests --

    #[test]
    fn collapsed_paste_summary_format_small() {
        let paste = CollapsedPaste::new("line1\nline2\nline3\nline4".to_string());
        assert_eq!(paste.summary, "[4 lines; 23b pasted]");
        // Trailing newline counts the extra line correctly
        let paste2 = CollapsedPaste::new("line1\nline2\n".to_string());
        assert_eq!(paste2.summary, "[3 lines; 12b pasted]");
    }

    #[test]
    fn collapsed_paste_summary_format_large() {
        let text = "x".repeat(1024) + "\nsecond line";
        let paste = CollapsedPaste::new(text);
        assert!(paste.summary.contains("2 lines"));
        assert!(paste.summary.contains("kb pasted]"));
    }

    #[test]
    fn collapse_paste_multiline_sets_state() {
        let mut state = InputState::default();
        state.collapse_paste("a\nb\nc\nd");
        assert!(state.is_collapsed());
        let textarea_text = state.textarea.lines().join("\n");
        assert!(textarea_text.contains("4 lines"), "textarea should show summary, got: {textarea_text}");
    }

    #[test]
    fn collapse_paste_single_line_no_collapse() {
        let mut state = InputState::default();
        state.collapse_paste("just one line");
        assert!(!state.is_collapsed());
        assert_eq!(state.textarea.lines().join(""), "just one line");
    }

    #[test]
    fn take_text_returns_full_when_collapsed() {
        let mut state = InputState::default();
        state.collapse_paste("line1\nline2\nline3");
        let text = state.take_text();
        assert_eq!(text, "line1\nline2\nline3");
    }

    #[test]
    fn take_text_clears_collapsed() {
        let mut state = InputState::default();
        state.collapse_paste("a\nb\nc");
        let _ = state.take_text();
        assert!(!state.is_collapsed());
    }

    #[test]
    fn expand_paste_restores_full_text() {
        let mut state = InputState::default();
        state.collapse_paste("one\ntwo\nthree");
        assert!(state.is_collapsed());
        state.expand_paste();
        assert!(!state.is_collapsed());
        let text = state.textarea.lines().join("\n");
        assert_eq!(text, "one\ntwo\nthree");
    }

    #[test]
    fn expand_paste_noop_when_not_collapsed() {
        let mut state = InputState::default();
        state.textarea.insert_str("hello");
        state.expand_paste(); // should not panic or change anything
        assert_eq!(state.textarea.lines().join(""), "hello");
    }

    #[test]
    fn collapse_paste_with_existing_text() {
        let mut state = InputState::default();
        state.textarea.insert_str("prefix: ");
        state.collapse_paste("a\nb\nc");
        assert!(state.is_collapsed());
        let text = state.take_text();
        assert!(text.starts_with("prefix: "), "should preserve existing text, got: {text}");
        assert!(text.contains("a\nb\nc"), "should contain pasted content, got: {text}");
    }

    #[test]
    fn double_paste_recollapse() {
        let mut state = InputState::default();
        state.collapse_paste("first\npaste");
        assert!(state.is_collapsed());
        state.collapse_paste("second\npaste");
        assert!(state.is_collapsed());
        let text = state.take_text();
        assert!(text.contains("first\npaste"), "should contain first paste, got: {text}");
        assert!(text.contains("second\npaste"), "should contain second paste, got: {text}");
    }

    #[test]
    fn set_text_clears_collapsed() {
        let mut state = InputState::default();
        state.collapse_paste("a\nb\nc");
        assert!(state.is_collapsed());
        state.set_text("/help");
        assert!(!state.is_collapsed());
        assert_eq!(state.textarea.lines().join(""), "/help");
    }

    #[test]
    fn buffer_top_border_present() {
        let (buf, _text) = render_input_to_parts(80, 5, AgentMode::Build, 0, 0, 0);
        // Row 0 should have a horizontal border character (─ or similar)
        // The top border is from Borders::TOP on the block
        let mut has_border = false;
        for x in 0..80 {
            let cell = &buf[(x, 0)];
            let sym = cell.symbol();
            if sym == "─" || sym == "━" || sym == "-" {
                has_border = true;
                break;
            }
        }
        assert!(has_border, "row 0 should contain a horizontal border character");
    }

    // -- paste_preview_visible reset tests --

    #[test]
    fn take_text_resets_paste_preview() {
        let mut state = InputState::default();
        state.collapse_paste("a\nb\nc");
        state.paste_preview_visible = true;
        let _ = state.take_text();
        assert!(!state.paste_preview_visible);
    }

    #[test]
    fn set_text_resets_paste_preview() {
        let mut state = InputState::default();
        state.collapse_paste("a\nb\nc");
        state.paste_preview_visible = true;
        state.set_text("new text");
        assert!(!state.paste_preview_visible);
    }

    #[test]
    fn expand_paste_resets_paste_preview() {
        let mut state = InputState::default();
        state.collapse_paste("a\nb\nc");
        state.paste_preview_visible = true;
        state.expand_paste();
        assert!(!state.paste_preview_visible);
    }

    // -- paste preview rendering tests --

    #[test]
    fn paste_preview_not_rendered_when_invisible() {
        let state = InputState::default();
        let theme = Theme::default();
        let buf = super::super::render_to_buffer(60, 20, |frame| {
            render_paste_preview(
                frame,
                Rect::new(0, 0, 60, 20),
                &state,
                &theme,
                0,
            );
        });
        let mut text = String::new();
        for y in 0..20 {
            for x in 0..60 {
                text.push_str(buf[(x, y)].symbol());
            }
        }
        assert!(!text.contains("Paste Preview"), "should not render when not visible");
    }

    #[test]
    fn paste_preview_rendered_when_visible() {
        let mut state = InputState::default();
        state.collapse_paste("line1\nline2\nline3\nline4");
        state.paste_preview_visible = true;
        let theme = Theme::default();
        let buf = super::super::render_to_buffer(60, 20, |frame| {
            render_paste_preview(
                frame,
                Rect::new(0, 0, 60, 20),
                &state,
                &theme,
                0,
            );
        });
        let mut text = String::new();
        for y in 0..20 {
            for x in 0..60 {
                text.push_str(buf[(x, y)].symbol());
            }
        }
        assert!(text.contains("Paste Preview"), "should show title, got:\n{text}");
        assert!(text.contains("line1"), "should show paste content, got:\n{text}");
    }

    #[test]
    fn paste_preview_truncates_long_content() {
        let mut state = InputState::default();
        let long_paste = (1..=30).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        state.collapse_paste(&long_paste);
        state.paste_preview_visible = true;
        let theme = Theme::default();
        let buf = super::super::render_to_buffer(60, 30, |frame| {
            render_paste_preview(
                frame,
                Rect::new(0, 0, 60, 30),
                &state,
                &theme,
                0,
            );
        });
        let mut text = String::new();
        for y in 0..30 {
            for x in 0..60 {
                text.push_str(buf[(x, y)].symbol());
            }
        }
        assert!(text.contains("more lines"), "should show truncation indicator, got:\n{text}");
    }

    // -- elapsed timer tests --

    /// Helper: render input area with optional elapsed duration.
    fn render_input_with_elapsed(
        width: u16,
        height: u16,
        mode: AgentMode,
        pct: u8,
        last_prompt: u64,
        ctx_window: u64,
        elapsed: Option<Duration>,
    ) -> (ratatui::buffer::Buffer, String) {
        let theme = Theme::default();
        let mut state = InputState::default();
        state.mode = mode;
        let context = InputContext {
            working_dir: "~/projects/steve".to_string(),
            last_prompt_tokens: last_prompt,
            context_window: ctx_window,
            context_usage_pct: pct,
            elapsed,
        };
        let buf = super::super::render_to_buffer(width, height, |frame| {
            render_input(
                frame,
                Rect::new(0, 0, width, height),
                &mut state,
                &theme,
                &context,
            );
        });
        let mut text = String::new();
        for y in 0..height {
            for x in 0..width {
                let cell = &buf[(x, y)];
                text.push_str(cell.symbol());
            }
            text.push('\n');
        }
        (buf, text)
    }

    #[test]
    fn buffer_elapsed_with_tokens() {
        let (_buf, text) = render_input_with_elapsed(
            80,
            5,
            AgentMode::Build,
            10,
            12800,
            128000,
            Some(Duration::from_secs(83)),
        );
        assert!(text.contains("1m 23s"), "should show elapsed time, got:\n{text}");
        assert!(text.contains("12.8k/128.0k"), "should still show tokens, got:\n{text}");
        let elapsed_pos = text.find("1m 23s").unwrap();
        let token_pos = text.find("12.8k").unwrap();
        assert!(elapsed_pos < token_pos, "elapsed should appear before tokens");
    }

    #[test]
    fn buffer_elapsed_without_tokens() {
        let (_buf, text) = render_input_with_elapsed(
            80,
            5,
            AgentMode::Build,
            0,
            0,
            0,
            Some(Duration::from_secs(5)),
        );
        assert!(text.contains("5s"), "should show elapsed time alone, got:\n{text}");
        assert!(!text.contains("·"), "no separator when no tokens");
    }

    #[test]
    fn buffer_no_elapsed_with_tokens() {
        let (_buf, text) = render_input_with_elapsed(
            80,
            5,
            AgentMode::Build,
            10,
            12800,
            128000,
            None,
        );
        assert!(text.contains("12.8k/128.0k"), "should show tokens, got:\n{text}");
        assert!(!text.contains("·"), "no separator when no elapsed");
    }
}
