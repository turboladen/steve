use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use super::theme::Theme;

/// Minimal message type for Phase 1. Will be replaced with session::Message later.
#[derive(Debug, Clone)]
pub struct DisplayMessage {
    pub role: DisplayRole,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayRole {
    User,
    Assistant,
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
                for text_line in msg.text.lines() {
                    lines.push(Line::from(Span::styled(
                        text_line.to_string(),
                        Style::default().fg(theme.assistant_msg),
                    )));
                }
            }
        }
        // Blank line between messages
        lines.push(Line::from(""));
    }

    // Calculate scroll for auto-scroll behavior
    let content_height = lines.len() as u16;
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
