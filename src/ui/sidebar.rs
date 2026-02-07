use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use super::theme::Theme;

/// State for the sidebar panel.
pub struct SidebarState {
    pub session_title: String,
    pub model_name: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub todos: Vec<TodoItem>,
}

/// A todo item displayed in the sidebar.
#[derive(Debug, Clone)]
pub struct TodoItem {
    pub text: String,
    pub done: bool,
}

impl Default for SidebarState {
    fn default() -> Self {
        Self {
            session_title: String::new(),
            model_name: String::new(),
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
            todos: Vec::new(),
        }
    }
}

/// Render the sidebar into the given area.
pub fn render_sidebar(
    frame: &mut Frame,
    area: Rect,
    state: &SidebarState,
    theme: &Theme,
) {
    let mut lines: Vec<Line> = Vec::new();

    // Session title
    lines.push(Line::from(Span::styled(
        "Session",
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
    )));
    let title = if state.session_title.is_empty() {
        "(untitled)"
    } else {
        &state.session_title
    };
    lines.push(Line::from(Span::styled(
        format!(" {title}"),
        Style::default().fg(theme.fg),
    )));
    lines.push(Line::from(""));

    // Model
    lines.push(Line::from(Span::styled(
        "Model",
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
    )));
    let model = if state.model_name.is_empty() {
        "(none)"
    } else {
        &state.model_name
    };
    lines.push(Line::from(Span::styled(
        format!(" {model}"),
        Style::default().fg(theme.fg),
    )));
    lines.push(Line::from(""));

    // Token usage
    lines.push(Line::from(Span::styled(
        "Tokens",
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(Span::styled(
        format!(" in: {}  out: {}", format_tokens(state.prompt_tokens), format_tokens(state.completion_tokens)),
        Style::default().fg(theme.dim),
    )));
    lines.push(Line::from(Span::styled(
        format!(" total: {}", format_tokens(state.total_tokens)),
        Style::default().fg(theme.dim),
    )));
    lines.push(Line::from(""));

    // Todos
    if !state.todos.is_empty() {
        lines.push(Line::from(Span::styled(
            "Todos",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )));
        for todo in &state.todos {
            let marker = if todo.done { "✓" } else { "○" };
            let style = if todo.done {
                Style::default().fg(theme.dim)
            } else {
                Style::default().fg(theme.fg)
            };
            lines.push(Line::from(Span::styled(
                format!(" {marker} {}", todo.text),
                style,
            )));
        }
    }

    let block = Block::default()
        .borders(Borders::LEFT)
        .border_style(Style::default().fg(theme.border))
        .title(" Info ")
        .title_style(Style::default().fg(theme.accent).add_modifier(Modifier::BOLD));

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: true });

    frame.render_widget(paragraph, area);
}

fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}
