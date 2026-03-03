pub mod autocomplete;
pub mod input;
pub mod layout;
pub mod message_area;
pub mod message_block;
pub mod sidebar;
pub mod status_line;
pub mod theme;

use std::io::{self, Stdout};

use anyhow::Result;
use crossterm::{
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
    event::{EnableMouseCapture, DisableMouseCapture, EnableBracketedPaste, DisableBracketedPaste},
};
use ratatui::{Frame, Terminal, backend::CrosstermBackend, style::Style, widgets::Block};

use crate::app::App;
use layout::compute_layout;
use message_area::render_message_blocks;
use autocomplete::render_autocomplete;
use input::{render_input, abbreviate_path, InputContext};
use sidebar::render_sidebar;
use status_line::Activity;

pub type Tui = Terminal<CrosstermBackend<Stdout>>;

pub fn setup_terminal() -> Result<Tui> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture, EnableBracketedPaste)?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

pub fn restore_terminal(terminal: &mut Tui) -> Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        DisableBracketedPaste
    )?;
    terminal.show_cursor()?;
    Ok(())
}

/// Render a widget into a headless test buffer. Used by rendering tests.
#[cfg(test)]
pub(crate) fn render_to_buffer(
    width: u16,
    height: u16,
    draw: impl FnOnce(&mut Frame),
) -> ratatui::buffer::Buffer {
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| draw(f)).unwrap();
    terminal.backend().buffer().clone()
}

pub fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let show_sidebar = app.should_show_sidebar(area.width);
    let layout = compute_layout(area, show_sidebar);

    // Context pressure percentage — drives ambient border color shifts
    let pct = app.status_line_state.context_usage_pct();

    // Build activity info for inline display in message area
    let activity = if app.is_loading {
        let state = &app.status_line_state;
        if state.activity != Activity::Idle {
            state.spinner_char().map(|ch| (ch, state.activity_text()))
        } else {
            None
        }
    } else {
        None
    };

    render_message_blocks(
        frame,
        layout.message_area,
        &app.messages,
        &mut app.message_area_state,
        &app.theme,
        activity,
        pct,
    );

    if let Some(sep_area) = layout.sidebar_separator {
        // Render a thin colored column as visual separator — copies as a space, not │
        let sep = Block::default().style(Style::default().bg(app.theme.border_color(pct)));
        frame.render_widget(sep, sep_area);
    }

    if let Some(sidebar_area) = layout.sidebar {
        render_sidebar(
            frame,
            sidebar_area,
            &app.sidebar_state,
            &app.theme,
        );
    }

    let context = InputContext {
        working_dir: abbreviate_path(&app.project.root),
        last_prompt_tokens: app.status_line_state.last_prompt_tokens,
        context_window: app.status_line_state.context_window,
        context_usage_pct: app.status_line_state.context_usage_pct(),
    };

    render_input(
        frame,
        layout.input_area,
        &mut app.input,
        &app.theme,
        &context,
    );

    render_autocomplete(
        frame,
        layout.input_area,
        &app.autocomplete_state,
        &app.theme,
        pct,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: render the full app into a buffer and return text.
    fn render_app_to_parts(width: u16, height: u16) -> (ratatui::buffer::Buffer, String) {
        let mut app = crate::app::tests::make_test_app();
        let buf = render_to_buffer(width, height, |frame| {
            render(frame, &mut app);
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
    fn layout_80x24_no_sidebar() {
        let (_buf, text) = render_app_to_parts(80, 24);
        // At 80 columns, sidebar should NOT be visible
        assert!(!text.contains("Session"), "sidebar should not be visible at 80 cols");
        // Input area should be present (the chevron ">")
        assert!(text.contains(">"), "input chevron should be visible");
    }

    #[test]
    fn layout_120x24_with_sidebar() {
        let mut app = crate::app::tests::make_test_app();
        app.sidebar_state.model_name = "gpt-4o".to_string();
        let buf = render_to_buffer(120, 24, |frame| {
            render(frame, &mut app);
        });
        let mut text = String::new();
        for y in 0..24 {
            for x in 0..120 {
                let cell = &buf[(x, y)];
                text.push_str(cell.symbol());
            }
            text.push('\n');
        }
        assert!(text.contains("Session"), "sidebar 'Session' header should be visible at 120 cols");
        assert!(text.contains("gpt-4o"), "sidebar should show model name");
    }

    #[test]
    fn layout_separator_column_has_border_bg() {
        let mut app = crate::app::tests::make_test_app();
        let buf = render_to_buffer(120, 24, |frame| {
            render(frame, &mut app);
        });
        // The separator is 1 column wide, at x = 120 - 1(sep) - 40(sidebar) = 79
        let sep_x = 79;
        let cell = &buf[(sep_x, 0)];
        assert_eq!(
            cell.bg, app.theme.border_color(0),
            "separator column should have theme.border_color background"
        );
    }
}
