use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use super::message_block::{AssistantPart, DiffContent, DiffLine, MessageBlock, ToolGroupStatus};
use super::theme::Theme;
use crate::tool::ToolName;

/// State for the scrollable message area.
///
/// Coordinate system: `scroll_offset = 0` means top of content.
/// Auto-scroll sets `scroll_offset = max_scroll` (bottom of content).
/// This aligns with ratatui's `Paragraph::scroll((row, 0))` API.
pub struct MessageAreaState {
    /// Current scroll position (0 = top of content).
    pub scroll_offset: u16,
    /// Whether to automatically scroll to follow new content.
    pub auto_scroll: bool,
    /// Total content height from last render (used for clamping).
    content_height: u16,
    /// Visible area height from last render.
    visible_height: u16,
}

impl Default for MessageAreaState {
    fn default() -> Self {
        Self {
            scroll_offset: 0,
            auto_scroll: true,
            content_height: 0,
            visible_height: 0,
        }
    }
}

impl MessageAreaState {
    /// Maximum scroll offset (0 if content fits in view).
    pub fn max_scroll(&self) -> u16 {
        self.content_height.saturating_sub(self.visible_height)
    }

    /// Scroll toward older content (up). Disables auto-scroll.
    pub fn scroll_up(&mut self, amount: u16) {
        self.scroll_offset = self.scroll_offset.saturating_sub(amount);
        self.auto_scroll = false;
    }

    /// Scroll toward newer content (down). Re-enables auto-scroll at bottom.
    pub fn scroll_down(&mut self, amount: u16) {
        let max = self.max_scroll();
        self.scroll_offset = self.scroll_offset.saturating_add(amount).min(max);
        if self.scroll_offset >= max {
            self.auto_scroll = true;
        }
    }

    /// Jump to the bottom (newest content). Re-enables auto-scroll.
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = self.max_scroll();
        self.auto_scroll = true;
    }

    /// Update dimensions from render. If auto-scroll, jump to bottom.
    /// Clamp offset to valid range.
    pub fn update_dimensions(&mut self, content_height: u16, visible_height: u16) {
        self.content_height = content_height;
        self.visible_height = visible_height;
        let max = self.max_scroll();
        if self.auto_scroll {
            self.scroll_offset = max;
        } else {
            self.scroll_offset = self.scroll_offset.min(max);
        }
    }
}

/// Render structured message blocks into the given area.
pub fn render_message_blocks(
    frame: &mut Frame,
    area: Rect,
    messages: &[MessageBlock],
    state: &mut MessageAreaState,
    theme: &Theme,
    activity: Option<(char, String)>,
) {
    let mut lines: Vec<Line> = Vec::new();
    let available_width = area.width.max(1) as usize;

    for msg in messages {
        match msg {
            MessageBlock::User { text } => {
                for text_line in text.lines() {
                    lines.push(Line::from(vec![
                        Span::styled(
                            "> ",
                            Style::default()
                                .fg(theme.user_msg)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            text_line.to_string(),
                            Style::default().fg(theme.user_msg),
                        ),
                    ]));
                }
            }

            MessageBlock::Assistant {
                thinking,
                parts,
            } => {
                // Thinking block (collapsed by default)
                if let Some(t) = thinking {
                    if t.expanded {
                        lines.push(Line::from(Span::styled(
                            format!("\u{25bc} Thinking ({} tokens)", t.token_count),
                            Style::default()
                                .fg(theme.reasoning)
                                .add_modifier(Modifier::ITALIC),
                        )));
                        for content_line in t.content.lines() {
                            lines.push(Line::from(Span::styled(
                                format!("  {content_line}"),
                                Style::default().fg(theme.reasoning),
                            )));
                        }
                    } else {
                        lines.push(Line::from(Span::styled(
                            format!("\u{25b6} Thinking ({} tokens)", t.token_count),
                            Style::default()
                                .fg(theme.reasoning)
                                .add_modifier(Modifier::ITALIC),
                        )));
                    }
                }

                // Parts in chronological order
                for part in parts {
                    match part {
                        AssistantPart::Text(text) => {
                            render_text_with_code_blocks(text, &mut lines, theme, available_width);
                        }
                        AssistantPart::ToolGroup(group) => {
                            for call in &group.calls {
                                let status_indicator = match (&group.status, &call.result_summary) {
                                    (_, Some(_)) if call.expanded => "\u{25bc}",
                                    (_, Some(_)) => "\u{25b6}",
                                    _ => "\u{2819}",
                                };

                                let result_part = match &call.result_summary {
                                    Some(summary) => format!(" \u{2192} {summary}"),
                                    None => match &group.status {
                                        ToolGroupStatus::Preparing => " preparing...".to_string(),
                                        ToolGroupStatus::Running { .. } => " running...".to_string(),
                                        ToolGroupStatus::Complete => String::new(),
                                    },
                                };

                                let color = if call.is_error {
                                    theme.error
                                } else {
                                    match call.tool_name {
                                        ToolName::Read | ToolName::Grep | ToolName::Glob
                                        | ToolName::List | ToolName::Webfetch => theme.tool_read,
                                        ToolName::Edit | ToolName::Write | ToolName::Patch
                                        | ToolName::Memory => theme.tool_write,
                                        ToolName::Bash | ToolName::Question | ToolName::Todo => theme.accent,
                                    }
                                };

                                lines.push(Line::from(Span::styled(
                                    format!(
                                        "{status_indicator} {} {}({}){}",
                                        call.tool_name.tool_marker(),
                                        call.tool_name, call.args_summary, result_part
                                    ),
                                    Style::default().fg(color),
                                )));

                                // Expanded output — diff content or raw output fallback
                                if call.expanded {
                                    if let Some(diff) = &call.diff_content {
                                        render_diff_lines(&mut lines, diff, call.result_summary.as_deref(), theme);
                                    } else if let Some(output) = &call.full_output {
                                        for output_line in output.lines() {
                                            lines.push(Line::from(Span::styled(
                                                format!("  {output_line}"),
                                                Style::default().fg(theme.dim),
                                            )));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            MessageBlock::System { text } => {
                for text_line in text.lines() {
                    lines.push(Line::from(Span::styled(
                        text_line.to_string(),
                        Style::default()
                            .fg(theme.dim)
                            .add_modifier(Modifier::ITALIC),
                    )));
                }
            }

            MessageBlock::Error { text } => {
                for text_line in text.lines() {
                    lines.push(Line::from(Span::styled(
                        text_line.to_string(),
                        Style::default().fg(theme.error),
                    )));
                }
            }

            MessageBlock::Permission {
                tool_name,
                args_summary,
            } => {
                // Top rule
                lines.push(Line::from(Span::styled(
                    "\u{2500}".repeat(40),
                    Style::default().fg(theme.permission),
                )));
                // Prompt line
                lines.push(Line::from(vec![
                    Span::styled(
                        "\u{26a0} Allow ",
                        Style::default()
                            .fg(theme.permission)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        tool_name.to_string(),
                        Style::default()
                            .fg(theme.accent)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!(": {args_summary}?"),
                        Style::default()
                            .fg(theme.permission)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]));
                // Options line with highlighted key letters
                lines.push(Line::from(vec![
                    Span::raw("  ["),
                    Span::styled(
                        "y",
                        Style::default()
                            .fg(theme.success)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw("]es / ["),
                    Span::styled(
                        "n",
                        Style::default()
                            .fg(theme.error)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw("]o / ["),
                    Span::styled(
                        "a",
                        Style::default()
                            .fg(theme.permission)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw("]lways"),
                ]));
                // Bottom rule
                lines.push(Line::from(Span::styled(
                    "\u{2500}".repeat(40),
                    Style::default().fg(theme.permission),
                )));
            }
        }

        // Blank line between messages
        lines.push(Line::from(""));
    }

    // Inline activity spinner (replaces the old "..." and status bar spinner)
    if let Some((spinner, text)) = activity {
        lines.push(Line::from(Span::styled(
            format!("{spinner} {text}"),
            Style::default().fg(theme.accent),
        )));
        lines.push(Line::from(""));
    }

    // Compute content height with wrapping
    let content_height_u32: u32 = lines
        .iter()
        .map(|line| {
            let line_width: usize = line.width();
            if line_width == 0 {
                1u32
            } else {
                ((line_width + available_width - 1) / available_width) as u32
            }
        })
        .sum();
    let content_height = content_height_u32.min(u16::MAX as u32) as u16;
    let visible_height = area.height.saturating_sub(2);

    state.update_dimensions(content_height, visible_height);

    let block = Block::default()
        .borders(Borders::NONE)
        .style(Style::default().fg(theme.fg));

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((state.scroll_offset, 0));

    frame.render_widget(paragraph, area);
}

/// Render diff content into styled lines with box-drawing frame.
fn render_diff_lines(
    lines: &mut Vec<Line<'_>>,
    diff: &DiffContent,
    result_summary: Option<&str>,
    theme: &Theme,
) {
    match diff {
        DiffContent::EditDiff { lines: diff_lines }
        | DiffContent::PatchDiff { lines: diff_lines } => {
            // Top border
            lines.push(Line::from(Span::styled(
                "  \u{250c}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}",
                Style::default().fg(theme.border),
            )));

            for diff_line in diff_lines {
                let (prefix, text, color) = match diff_line {
                    DiffLine::Removal(t) => ("-", t.as_str(), theme.error),
                    DiffLine::Addition(t) => ("+", t.as_str(), theme.success),
                    DiffLine::Context(t) => (" ", t.as_str(), theme.dim),
                    DiffLine::HunkHeader(t) => ("", t.as_str(), theme.dim),
                };
                lines.push(Line::from(vec![
                    Span::styled("  \u{2502} ", Style::default().fg(theme.border)),
                    Span::styled(
                        format!("{prefix}{text}"),
                        Style::default().fg(color),
                    ),
                ]));
            }

            // Bottom border
            lines.push(Line::from(Span::styled(
                "  \u{2514}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}",
                Style::default().fg(theme.border),
            )));
        }
        DiffContent::WriteSummary { line_count } => {
            // Determine if this is a create or overwrite from the result summary
            let verb = match result_summary {
                Some(s) if s.starts_with("Created") => "Created",
                _ => "Overwrote",
            };
            lines.push(Line::from(Span::styled(
                format!("  {verb} ({line_count} lines)"),
                Style::default().fg(theme.dim),
            )));
        }
    }
}

/// Detect fenced code blocks in assistant text and render with tinted background.
///
/// Uses a stateless line-by-line scanner: lines starting with ` ``` ` (≤3 leading
/// spaces) toggle code block mode. Opening fences emit a header line with optional
/// language label; closing fences are consumed. Code lines get `code_bg` background.
fn render_text_with_code_blocks(
    text: &str,
    lines: &mut Vec<Line<'_>>,
    theme: &Theme,
    available_width: usize,
) {
    let mut in_code_block = false;

    for text_line in text.lines() {
        let trimmed = text_line.trim_start_matches(' ');
        let leading_spaces = text_line.len() - trimmed.len();

        // A fence is ``` with ≤3 leading ASCII spaces (CommonMark rule)
        if leading_spaces <= 3 && trimmed.starts_with("```") {
            if !in_code_block {
                // Opening fence — extract language label
                let lang = trimmed[3..].trim();
                let code_bg_style = Style::default().fg(theme.dim).bg(theme.code_bg);

                if lang.is_empty() {
                    // No language: skip header entirely — code_bg on code lines
                    // provides framing. An all-space header would be invisible.
                } else {
                    // Language label followed by space fill (background tint provides framing)
                    let label = format!("{lang} ");
                    let fill_len = available_width.saturating_sub(label.chars().count());
                    let fill = " ".repeat(fill_len);
                    lines.push(
                        Line::from(vec![
                            Span::styled(label, code_bg_style),
                            Span::styled(fill, code_bg_style),
                        ])
                        .style(Style::default().bg(theme.code_bg)),
                    );
                }
                in_code_block = true;
            } else {
                // Closing fence — consume without rendering
                in_code_block = false;
            }
        } else if in_code_block {
            // Code line — tinted background
            lines.push(
                Line::from(Span::styled(
                    text_line.to_string(),
                    Style::default().fg(theme.assistant_msg).bg(theme.code_bg),
                ))
                .style(Style::default().bg(theme.code_bg)),
            );
        } else {
            // Normal prose line
            lines.push(Line::from(Span::styled(
                text_line.to_string(),
                Style::default().fg(theme.assistant_msg),
            )));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_starts_at_zero_with_auto_scroll() {
        let state = MessageAreaState::default();
        assert_eq!(state.scroll_offset, 0);
        assert!(state.auto_scroll);
    }

    #[test]
    fn update_dimensions_auto_scrolls_to_bottom() {
        let mut state = MessageAreaState::default();
        state.update_dimensions(100, 20);
        assert_eq!(state.scroll_offset, 80); // max_scroll = 100 - 20
        assert!(state.auto_scroll);
    }

    #[test]
    fn scroll_up_disables_auto_scroll() {
        let mut state = MessageAreaState::default();
        state.update_dimensions(100, 20);
        assert_eq!(state.scroll_offset, 80);
        state.scroll_up(5);
        assert_eq!(state.scroll_offset, 75);
        assert!(!state.auto_scroll);
    }

    #[test]
    fn scroll_up_clamps_at_zero() {
        let mut state = MessageAreaState::default();
        state.update_dimensions(100, 20);
        state.scroll_up(200);
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn scroll_down_to_bottom_re_enables_auto_scroll() {
        let mut state = MessageAreaState::default();
        state.update_dimensions(100, 20);
        state.scroll_up(30); // at 50 now
        assert!(!state.auto_scroll);
        state.scroll_down(30); // at 80 = max_scroll
        assert_eq!(state.scroll_offset, 80);
        assert!(state.auto_scroll);
    }

    #[test]
    fn scroll_down_clamps_at_max() {
        let mut state = MessageAreaState::default();
        state.update_dimensions(100, 20);
        state.scroll_up(10); // at 70
        state.scroll_down(200); // should clamp to 80
        assert_eq!(state.scroll_offset, 80);
    }

    #[test]
    fn update_dimensions_clamps_when_not_auto_scrolling() {
        let mut state = MessageAreaState::default();
        state.update_dimensions(100, 20);
        state.scroll_up(10); // at 70, auto_scroll = false
        // Content shrinks (e.g., after compact)
        state.update_dimensions(50, 20);
        // max_scroll = 30, so offset should clamp from 70 to 30
        assert_eq!(state.scroll_offset, 30);
        assert!(!state.auto_scroll);
    }

    #[test]
    fn max_scroll_zero_when_content_fits() {
        let mut state = MessageAreaState::default();
        state.update_dimensions(10, 20);
        assert_eq!(state.max_scroll(), 0);
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn scroll_to_bottom_works() {
        let mut state = MessageAreaState::default();
        state.update_dimensions(100, 20);
        state.scroll_up(50); // at 30
        state.scroll_to_bottom();
        assert_eq!(state.scroll_offset, 80);
        assert!(state.auto_scroll);
    }

    // -- render_diff_lines tests --

    use super::super::message_block::{DiffContent, DiffLine};
    use super::super::theme::Theme;

    #[test]
    fn render_diff_lines_edit_diff_structure() {
        let theme = Theme::default();
        let diff = DiffContent::EditDiff {
            lines: vec![
                DiffLine::Removal("old".into()),
                DiffLine::Addition("new".into()),
            ],
        };
        let mut output: Vec<Line> = Vec::new();
        render_diff_lines(&mut output, &diff, None, &theme);
        // top border + 2 diff lines + bottom border = 4 lines
        assert_eq!(output.len(), 4);
    }

    #[test]
    fn render_diff_lines_patch_diff_structure() {
        let theme = Theme::default();
        let diff = DiffContent::PatchDiff {
            lines: vec![
                DiffLine::HunkHeader("@@ -1 +1 @@".into()),
                DiffLine::Context("ctx".into()),
                DiffLine::Removal("old".into()),
                DiffLine::Addition("new".into()),
            ],
        };
        let mut output: Vec<Line> = Vec::new();
        render_diff_lines(&mut output, &diff, None, &theme);
        // top border + 4 diff lines + bottom border = 6 lines
        assert_eq!(output.len(), 6);
    }

    #[test]
    fn render_diff_lines_write_summary_created_verb() {
        let theme = Theme::default();
        let diff = DiffContent::WriteSummary { line_count: 10 };
        let mut output: Vec<Line> = Vec::new();
        render_diff_lines(&mut output, &diff, Some("Created /tmp/foo (42 bytes)"), &theme);
        assert_eq!(output.len(), 1);
        let text = format!("{:?}", output[0]);
        assert!(text.contains("Created"), "verb should be Created");
        assert!(text.contains("10 lines"), "should show line count");
    }

    #[test]
    fn render_diff_lines_write_summary_overwrote_verb() {
        let theme = Theme::default();
        let diff = DiffContent::WriteSummary { line_count: 5 };
        let mut output: Vec<Line> = Vec::new();
        render_diff_lines(&mut output, &diff, Some("Overwrote /tmp/foo (20 bytes)"), &theme);
        assert_eq!(output.len(), 1);
        let text = format!("{:?}", output[0]);
        assert!(text.contains("Overwrote"), "verb should be Overwrote");
    }

    #[test]
    fn render_diff_lines_write_summary_defaults_to_overwrote() {
        let theme = Theme::default();
        let diff = DiffContent::WriteSummary { line_count: 3 };
        let mut output: Vec<Line> = Vec::new();
        render_diff_lines(&mut output, &diff, None, &theme);
        assert_eq!(output.len(), 1);
        let text = format!("{:?}", output[0]);
        assert!(text.contains("Overwrote"), "should default to Overwrote when no summary");
    }

    #[test]
    fn render_diff_lines_empty_edit_diff() {
        let theme = Theme::default();
        let diff = DiffContent::EditDiff { lines: vec![] };
        let mut output: Vec<Line> = Vec::new();
        render_diff_lines(&mut output, &diff, None, &theme);
        // top border + 0 diff lines + bottom border = 2 lines
        assert_eq!(output.len(), 2);
    }

    // -- Buffer rendering tests --
    //
    // These test the actual render pipeline through ratatui's TestBackend,
    // catching layout bugs that pure data-model tests miss.

    use ratatui::layout::Rect;
    use super::super::message_block::{ThinkingBlock, ToolCall, ToolGroup, ToolGroupStatus};

    /// Helper: render message blocks into a buffer and return the buffer text as a single string.
    fn render_messages_to_string(
        width: u16,
        height: u16,
        messages: &[MessageBlock],
        activity: Option<(char, String)>,
    ) -> String {
        let theme = Theme::default();
        let mut state = MessageAreaState::default();
        let buf = super::super::render_to_buffer(width, height, |frame| {
            render_message_blocks(
                frame,
                Rect::new(0, 0, width, height),
                messages,
                &mut state,
                &theme,
                activity,
            );
        });
        // Collect all cells into a string, row by row
        let mut text = String::new();
        for y in 0..height {
            for x in 0..width {
                let cell = &buf[(x, y)];
                text.push_str(cell.symbol());
            }
            text.push('\n');
        }
        text
    }

    #[test]
    fn buffer_user_message_has_prefix() {
        let messages = vec![MessageBlock::User {
            text: "Hello world".to_string(),
        }];
        let text = render_messages_to_string(60, 10, &messages, None);
        assert!(text.contains("> Hello world"), "user message should have '> ' prefix, got:\n{text}");
    }

    #[test]
    fn buffer_assistant_text_rendered() {
        let messages = vec![MessageBlock::Assistant {
            thinking: None,
            parts: vec![AssistantPart::Text("Response text here".to_string())],
        }];
        let text = render_messages_to_string(60, 10, &messages, None);
        assert!(text.contains("Response text here"), "assistant text should appear, got:\n{text}");
        // Should NOT have "> " prefix
        assert!(!text.contains("> Response"), "assistant should not have user prefix");
    }

    #[test]
    fn buffer_thinking_collapsed_shows_arrow_and_token_count() {
        let messages = vec![MessageBlock::Assistant {
            thinking: Some(ThinkingBlock {
                token_count: 42,
                content: "deep thoughts".to_string(),
                expanded: false,
            }),
            parts: vec![],
        }];
        let text = render_messages_to_string(60, 10, &messages, None);
        assert!(text.contains("\u{25b6}"), "collapsed thinking should show ▶");
        assert!(text.contains("Thinking"), "should show 'Thinking'");
        assert!(text.contains("42"), "should show token count");
    }

    #[test]
    fn buffer_thinking_expanded_shows_content() {
        let messages = vec![MessageBlock::Assistant {
            thinking: Some(ThinkingBlock {
                token_count: 10,
                content: "my thoughts".to_string(),
                expanded: true,
            }),
            parts: vec![],
        }];
        let text = render_messages_to_string(60, 10, &messages, None);
        assert!(text.contains("\u{25bc}"), "expanded thinking should show ▼");
        assert!(text.contains("my thoughts"), "expanded thinking should show content");
    }

    #[test]
    fn buffer_tool_group_preparing_shows_spinner() {
        let messages = vec![MessageBlock::Assistant {
            thinking: None,
            parts: vec![AssistantPart::ToolGroup(ToolGroup {
                calls: vec![ToolCall {
                    tool_name: ToolName::Read,
                    args_summary: "src/main.rs".to_string(),
                    full_output: None,
                    result_summary: None,
                    diff_content: None,
                    is_error: false,
                    expanded: false,
                }],
                status: ToolGroupStatus::Preparing,
            })],
        }];
        let text = render_messages_to_string(80, 10, &messages, None);
        assert!(text.contains("preparing..."), "preparing tool should show 'preparing...'");
    }

    #[test]
    fn buffer_tool_group_complete_collapsed() {
        let messages = vec![MessageBlock::Assistant {
            thinking: None,
            parts: vec![AssistantPart::ToolGroup(ToolGroup {
                calls: vec![ToolCall {
                    tool_name: ToolName::Read,
                    args_summary: "src/main.rs".to_string(),
                    full_output: Some("file content".to_string()),
                    result_summary: Some("150 lines".to_string()),
                    diff_content: None,
                    is_error: false,
                    expanded: false,
                }],
                status: ToolGroupStatus::Complete,
            })],
        }];
        let text = render_messages_to_string(80, 10, &messages, None);
        assert!(text.contains("\u{25b6}"), "collapsed complete should show ▶");
        assert!(text.contains("read"), "should show tool name");
        assert!(text.contains("src/main.rs"), "should show args");
        assert!(text.contains("150 lines"), "should show result summary");
    }

    #[test]
    fn buffer_tool_group_expanded_with_diff() {
        let messages = vec![MessageBlock::Assistant {
            thinking: None,
            parts: vec![AssistantPart::ToolGroup(ToolGroup {
                calls: vec![ToolCall {
                    tool_name: ToolName::Edit,
                    args_summary: "src/main.rs".to_string(),
                    full_output: None,
                    result_summary: Some("edited".to_string()),
                    diff_content: Some(DiffContent::EditDiff {
                        lines: vec![
                            DiffLine::Removal("old line".into()),
                            DiffLine::Addition("new line".into()),
                        ],
                    }),
                    is_error: false,
                    expanded: true,
                }],
                status: ToolGroupStatus::Complete,
            })],
        }];
        let text = render_messages_to_string(80, 15, &messages, None);
        // Box-drawing frame
        assert!(text.contains("\u{250c}"), "should have top-left corner ┌");
        assert!(text.contains("\u{2514}"), "should have bottom-left corner └");
        assert!(text.contains("-old line"), "should show removal with -");
        assert!(text.contains("+new line"), "should show addition with +");
    }

    #[test]
    fn buffer_system_message_rendered() {
        let messages = vec![MessageBlock::System {
            text: "System notice".to_string(),
        }];
        let text = render_messages_to_string(60, 10, &messages, None);
        assert!(text.contains("System notice"), "system message should appear");
    }

    #[test]
    fn buffer_error_message_rendered() {
        let messages = vec![MessageBlock::Error {
            text: "Something broke".to_string(),
        }];
        let text = render_messages_to_string(60, 10, &messages, None);
        assert!(text.contains("Something broke"), "error message should appear");
    }

    #[test]
    fn buffer_permission_prompt_rendered() {
        let messages = vec![MessageBlock::Permission {
            tool_name: "bash".to_string(),
            args_summary: "rm -rf".to_string(),
        }];
        let text = render_messages_to_string(60, 10, &messages, None);
        assert!(text.contains("Allow"), "permission should show 'Allow'");
        assert!(text.contains("bash"), "permission should show tool name");
        assert!(text.contains("rm -rf"), "permission should show args");
        assert!(text.contains("]es"), "should show [y]es option");
        assert!(text.contains("]o"), "should show [n]o option");
        assert!(text.contains("]lways"), "should show [a]lways option");
    }

    #[test]
    fn buffer_activity_spinner_inline() {
        let messages = vec![];
        let text = render_messages_to_string(60, 10, &messages, Some(('⠋', "Thinking...".to_string())));
        assert!(text.contains("Thinking..."), "activity text should appear");
    }

    #[test]
    fn buffer_blank_line_between_messages() {
        let messages = vec![
            MessageBlock::User { text: "msg1".to_string() },
            MessageBlock::User { text: "msg2".to_string() },
        ];
        let text = render_messages_to_string(60, 10, &messages, None);
        // Find positions — msg2 should not immediately follow msg1
        let pos1 = text.find("> msg1").expect("msg1 not found");
        let pos2 = text.find("> msg2").expect("msg2 not found");
        // There should be at least one blank line between them (newline + spaces + newline)
        let between = &text[pos1..pos2];
        let line_count = between.lines().count();
        assert!(line_count >= 2, "should have blank line separation, got {line_count} lines between messages");
    }

    // -- render_text_with_code_blocks tests --

    #[test]
    fn code_block_renders_with_header() {
        let theme = Theme::default();
        let text = "before\n```rust\nfn main() {}\n```\nafter";
        let mut lines: Vec<Line> = Vec::new();
        render_text_with_code_blocks(text, &mut lines, &theme, 40);
        // 5 input lines → "before", header, "fn main() {}", (closing consumed), "after" = 4 output lines
        assert_eq!(lines.len(), 4, "expected 4 lines, got {}", lines.len());
        // Header should contain language label
        let header_text: String = lines[1].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(header_text.starts_with("rust "), "header should start with 'rust ', got: {header_text}");
        // Fill after label should be all spaces (copy-text constraint: no box-drawing chars)
        assert!(header_text["rust ".len()..].chars().all(|c| c == ' '),
            "header fill should be all spaces, got: {header_text}");
    }

    #[test]
    fn code_block_no_language_skips_header() {
        let theme = Theme::default();
        let text = "```\ncode\n```";
        let mut lines: Vec<Line> = Vec::new();
        render_text_with_code_blocks(text, &mut lines, &theme, 30);
        // No header for bare fences — just the code line (closing consumed)
        assert_eq!(lines.len(), 1);
        let code_text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(code_text, "code");
        assert_eq!(lines[0].style.bg, Some(theme.code_bg), "code line should have code_bg");
    }

    #[test]
    fn unclosed_code_block_tints_remaining() {
        let theme = Theme::default();
        let text = "before\n```python\nline1\nline2";
        let mut lines: Vec<Line> = Vec::new();
        render_text_with_code_blocks(text, &mut lines, &theme, 40);
        // "before", header, "line1", "line2" = 4 lines
        assert_eq!(lines.len(), 4);
        // Lines 2 and 3 (code lines) should have code_bg background on Line.style
        for i in 2..4 {
            assert_eq!(
                lines[i].style.bg, Some(theme.code_bg),
                "unclosed code line {i} should have code_bg"
            );
        }
    }

    #[test]
    fn empty_code_block() {
        let theme = Theme::default();
        let text = "```\n```";
        let mut lines: Vec<Line> = Vec::new();
        render_text_with_code_blocks(text, &mut lines, &theme, 20);
        // No header for bare fences, no code content — nothing to render
        assert_eq!(lines.len(), 0);
    }

    #[test]
    fn inline_backticks_not_treated_as_fence() {
        let theme = Theme::default();
        let text = "use `foo` and ``bar``";
        let mut lines: Vec<Line> = Vec::new();
        render_text_with_code_blocks(text, &mut lines, &theme, 40);
        assert_eq!(lines.len(), 1);
        // Should have no code_bg
        assert_eq!(lines[0].style.bg, None, "inline backticks should not trigger code block");
    }

    #[test]
    fn multiple_code_blocks() {
        let theme = Theme::default();
        let text = "text1\n```rust\nfn a() {}\n```\ntext2\n```go\nfunc b() {}\n```\ntext3";
        let mut lines: Vec<Line> = Vec::new();
        render_text_with_code_blocks(text, &mut lines, &theme, 40);
        // text1, header1, "fn a() {}", text2, header2, "func b() {}", text3 = 7 lines
        assert_eq!(lines.len(), 7, "expected 7 lines, got {}", lines.len());
        // Normal text lines should NOT have code_bg
        assert_eq!(lines[0].style.bg, None, "text1 should not have bg");
        assert_eq!(lines[3].style.bg, None, "text2 should not have bg");
        assert_eq!(lines[6].style.bg, None, "text3 should not have bg");
        // Code lines should have code_bg
        assert_eq!(lines[2].style.bg, Some(theme.code_bg), "code line 1 should have bg");
        assert_eq!(lines[5].style.bg, Some(theme.code_bg), "code line 2 should have bg");
    }

    #[test]
    fn deeply_indented_fence_ignored() {
        let theme = Theme::default();
        let text = "    ```rust\nstill normal";
        let mut lines: Vec<Line> = Vec::new();
        render_text_with_code_blocks(text, &mut lines, &theme, 40);
        // 4 spaces = not a fence, both lines rendered as normal text
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].style.bg, None, "4-space indented fence should be normal text");
        assert_eq!(lines[1].style.bg, None, "following line should be normal text");
    }

    #[test]
    fn code_block_header_has_bg() {
        let theme = Theme::default();
        let text = "```js\nconsole.log();\n```";
        let mut lines: Vec<Line> = Vec::new();
        render_text_with_code_blocks(text, &mut lines, &theme, 40);
        // Header line should have code_bg on Line.style
        assert_eq!(
            lines[0].style.bg,
            Some(theme.code_bg),
            "header line should have code_bg background"
        );
    }

    #[test]
    fn fence_closes_code_block() {
        let theme = Theme::default();
        let text = "```\ncode\n```\nafter";
        let mut lines: Vec<Line> = Vec::new();
        render_text_with_code_blocks(text, &mut lines, &theme, 40);
        // No header for bare fence, "code" + "after" = 2 lines (closing fence consumed)
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].style.bg, Some(theme.code_bg), "code line should have code_bg");
        assert_eq!(lines[1].style.bg, None, "line after closing fence should be normal text");
    }

    #[test]
    fn tab_indented_fence_ignored() {
        let theme = Theme::default();
        let text = "\t```rust\nstill normal";
        let mut lines: Vec<Line> = Vec::new();
        render_text_with_code_blocks(text, &mut lines, &theme, 40);
        // Tab is not a space — fence should not be recognized
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].style.bg, None, "tab-indented fence should be normal text");
        assert_eq!(lines[1].style.bg, None, "following line should be normal text");
    }

    #[test]
    fn buffer_code_block_renders_with_tint() {
        let messages = vec![MessageBlock::Assistant {
            thinking: None,
            parts: vec![AssistantPart::Text(
                "Here is code:\n```rust\nfn main() {}\n```\nDone.".to_string(),
            )],
        }];
        let text = render_messages_to_string(60, 15, &messages, None);
        // The header should appear with language label
        assert!(text.contains("rust"), "should contain language label 'rust'");
        assert!(!text.contains('\u{2500}'), "should not contain ─ (copy-text constraint)");
        // The code line should appear
        assert!(text.contains("fn main() {}"), "should contain code content");
        // The fence lines (```) should NOT appear
        assert!(!text.contains("```"), "fence markers should be consumed, not rendered");
        // Normal text should appear
        assert!(text.contains("Here is code:"), "text before block should appear");
        assert!(text.contains("Done."), "text after block should appear");
    }
}
