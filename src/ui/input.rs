use std::path::Path;

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};
use tui_textarea::TextArea;

use super::status_line::format_tokens;
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

/// Context information displayed above the input textarea.
pub struct InputContext {
    pub working_dir: String,
    pub total_tokens: u64,
    pub context_window: u64,
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
    // Split vertically: 1 row for context line, rest for textarea
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // context line
            Constraint::Min(1),   // textarea with chevron
        ])
        .split(area);

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
    if context.context_window > 0 {
        let pct = if context.context_window == 0 {
            0u8
        } else {
            ((context.total_tokens as f64 / context.context_window as f64) * 100.0).min(100.0) as u8
        };
        let token_color = if pct >= 80 {
            theme.error
        } else if pct >= 50 {
            theme.warning
        } else {
            theme.dim
        };
        right_spans.push(Span::styled(
            format!(
                "{}/{} ({}%)",
                format_tokens(context.total_tokens),
                format_tokens(context.context_window),
                pct,
            ),
            Style::default().fg(token_color),
        ));
    } else if context.total_tokens > 0 {
        right_spans.push(Span::styled(
            format_tokens(context.total_tokens),
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
            Constraint::Length(2), // "> "
            Constraint::Min(1),   // textarea
        ])
        .split(textarea_area);

    let chevron = Paragraph::new(Span::styled(
        "> ",
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
    ));
    frame.render_widget(chevron, input_horizontal[0]);

    let input_block = Block::default().borders(Borders::NONE);
    state.textarea.set_block(input_block);
    frame.render_widget(&state.textarea, input_horizontal[1]);
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
}
