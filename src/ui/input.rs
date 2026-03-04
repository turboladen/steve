use std::path::Path;

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};
use ratatui_textarea::TextArea;

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
    pub last_prompt_tokens: u64,
    pub context_window: u64,
    pub context_usage_pct: u8,
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
            mode: AgentMode::Plan,
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
    if context.context_window > 0 {
        let pct = context.context_usage_pct;
        let token_color = if pct >= 80 {
            theme.error
        } else if pct >= 60 {
            theme.warning
        } else if pct >= 40 {
            theme.context_amber
        } else {
            theme.dim
        };
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
}
