use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use super::message_block::{MessageBlock, ToolGroupStatus};
use super::theme::Theme;

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
                text,
                tool_groups,
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

                // Tool groups
                for group in tool_groups {
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
                        } else if call.tool_name.is_write_tool()
                            || call.tool_name.is_memory()
                        {
                            theme.tool_write
                        } else {
                            theme.tool_read
                        };

                        lines.push(Line::from(Span::styled(
                            format!(
                                "{status_indicator} \u{26a1} {}({}){}",
                                call.tool_name, call.args_summary, result_part
                            ),
                            Style::default().fg(color),
                        )));

                        // Expanded output
                        if call.expanded {
                            if let Some(output) = &call.full_output {
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

                // Response text
                for text_line in text.lines() {
                    lines.push(Line::from(Span::styled(
                        text_line.to_string(),
                        Style::default().fg(theme.assistant_msg),
                    )));
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
    let available_width = area.width.max(1) as usize;
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
}
