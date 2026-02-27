use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use super::theme::Theme;

/// A display message for the message area.
#[derive(Debug, Clone)]
pub struct DisplayMessage {
    pub role: DisplayRole,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayRole {
    User,
    Assistant,
    Tool,
    ToolResult,
    Error,
    System,
    Permission,
}

/// State for the scrollable message area.
pub struct MessageAreaState {
    pub scroll_offset: u16,
    pub auto_scroll: bool,
}

impl Default for MessageAreaState {
    fn default() -> Self {
        Self {
            scroll_offset: 0,
            auto_scroll: true,
        }
    }
}

impl MessageAreaState {
    pub fn scroll_up(&mut self, amount: u16) {
        self.scroll_offset = self.scroll_offset.saturating_add(amount);
        self.auto_scroll = false;
    }

    pub fn scroll_down(&mut self, amount: u16) {
        self.scroll_offset = self.scroll_offset.saturating_sub(amount);
        if self.scroll_offset == 0 {
            self.auto_scroll = true;
        }
    }

    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
        self.auto_scroll = true;
    }
}

/// Render the message list into the given area.
pub fn render_messages(
    frame: &mut Frame,
    area: Rect,
    messages: &[DisplayMessage],
    state: &mut MessageAreaState,
    theme: &Theme,
    is_loading: bool,
) {
    let mut lines: Vec<Line> = Vec::new();

    for msg in messages {
        match msg.role {
            DisplayRole::User => {
                lines.push(Line::from(vec![
                    Span::styled(
                        "> ",
                        Style::default()
                            .fg(theme.user_msg)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        msg.text.as_str(),
                        Style::default().fg(theme.user_msg),
                    ),
                ]));
            }
            DisplayRole::Assistant => {
                if msg.text.is_empty() && is_loading {
                    // Show a streaming indicator for the empty message being filled
                    lines.push(Line::from(Span::styled(
                        "...",
                        Style::default()
                            .fg(theme.dim)
                            .add_modifier(Modifier::DIM),
                    )));
                } else {
                    for text_line in msg.text.lines() {
                        lines.push(Line::from(Span::styled(
                            text_line.to_string(),
                            Style::default().fg(theme.assistant_msg),
                        )));
                    }
                }
            }
            DisplayRole::Tool => {
                for text_line in msg.text.lines() {
                    lines.push(Line::from(Span::styled(
                        text_line.to_string(),
                        Style::default()
                            .fg(theme.tool_call)
                            .add_modifier(Modifier::BOLD),
                    )));
                }
            }
            DisplayRole::ToolResult => {
                for text_line in msg.text.lines() {
                    lines.push(Line::from(Span::styled(
                        text_line.to_string(),
                        Style::default().fg(theme.dim),
                    )));
                }
            }
            DisplayRole::Error => {
                for text_line in msg.text.lines() {
                    lines.push(Line::from(Span::styled(
                        text_line.to_string(),
                        Style::default().fg(theme.error),
                    )));
                }
            }
            DisplayRole::System => {
                for text_line in msg.text.lines() {
                    lines.push(Line::from(Span::styled(
                        text_line.to_string(),
                        Style::default()
                            .fg(theme.dim)
                            .add_modifier(Modifier::ITALIC),
                    )));
                }
            }
            DisplayRole::Permission => {
                for text_line in msg.text.lines() {
                    lines.push(Line::from(Span::styled(
                        text_line.to_string(),
                        Style::default()
                            .fg(theme.warning)
                            .add_modifier(Modifier::BOLD),
                    )));
                }
            }
        }
        // Blank line between messages
        lines.push(Line::from(""));
    }

    // Calculate scroll for auto-scroll behavior.
    // We must account for line wrapping: each line may occupy multiple rows
    // when the Paragraph widget wraps it to fit the available width.
    let available_width = area.width.max(1) as usize;
    let content_height_u32: u32 = lines
        .iter()
        .map(|line| {
            let line_width: usize = line.width();
            if line_width == 0 {
                1u32 // blank lines still take one row
            } else {
                ((line_width + available_width - 1) / available_width) as u32
            }
        })
        .sum();
    // Cap at u16::MAX to avoid overflow — conversations this long should
    // use /compact to reclaim space, so capping scroll is acceptable.
    let content_height = content_height_u32.min(u16::MAX as u32) as u16;
    let visible_height = area.height.saturating_sub(2); // account for block borders
    if state.auto_scroll && content_height > visible_height {
        state.scroll_offset = content_height.saturating_sub(visible_height);
    }

    let block = Block::default()
        .borders(Borders::NONE)
        .style(Style::default().fg(theme.fg));

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((state.scroll_offset, 0));

    frame.render_widget(paragraph, area);
}
