use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::Span,
    widgets::{Block, Borders, Paragraph},
};
use tui_textarea::TextArea;

use super::theme::Theme;

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

/// State for the input area.
pub struct InputState {
    pub textarea: TextArea<'static>,
    pub mode: AgentMode,
}

impl Default for InputState {
    fn default() -> Self {
        let mut textarea = TextArea::default();
        textarea.set_cursor_line_style(Style::default());
        textarea.set_placeholder_text("Type a message...");
        Self {
            textarea,
            mode: AgentMode::Build,
        }
    }
}

impl InputState {
    /// Take the current text and clear the input.
    pub fn take_text(&mut self) -> String {
        let lines = self.textarea.lines().to_vec();
        let text = lines.join("\n");
        // Clear by replacing with a fresh textarea
        let mut textarea = TextArea::default();
        textarea.set_cursor_line_style(Style::default());
        textarea.set_placeholder_text("Type a message...");
        self.textarea = textarea;
        text
    }
}

/// Render the input area with mode indicator.
pub fn render_input(
    frame: &mut Frame,
    area: Rect,
    state: &mut InputState,
    theme: &Theme,
) {
    let mode_width: u16 = 9; // "[Build] " or "[Plan]  "

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(mode_width),
            Constraint::Min(1),
        ])
        .split(area);

    // Mode indicator
    let mode_color = match state.mode {
        AgentMode::Build => theme.mode_build,
        AgentMode::Plan => theme.mode_plan,
    };
    let mode_text = format!("[{}]", state.mode.display_name());
    let mode_widget = Paragraph::new(Span::styled(
        mode_text,
        Style::default()
            .fg(mode_color)
            .add_modifier(Modifier::BOLD),
    ))
    .block(Block::default().borders(Borders::NONE));
    frame.render_widget(mode_widget, chunks[0]);

    // Text input
    let input_block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(theme.border));
    state.textarea.set_block(input_block);
    frame.render_widget(&state.textarea, chunks[1]);
}
