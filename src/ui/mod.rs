pub mod input;
pub mod layout;
pub mod message_area;
pub mod sidebar;
pub mod theme;

use std::io::{self, Stdout};

use anyhow::Result;
use crossterm::{
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
    event::{EnableMouseCapture, DisableMouseCapture},
};
use ratatui::{Frame, Terminal, backend::CrosstermBackend};

use crate::app::App;
use layout::compute_layout;
use message_area::render_messages;
use input::render_input;
use sidebar::render_sidebar;

pub type Tui = Terminal<CrosstermBackend<Stdout>>;

pub fn setup_terminal() -> Result<Tui> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

pub fn restore_terminal(terminal: &mut Tui) -> Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
}

pub fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let show_sidebar = area.width >= 120;
    let layout = compute_layout(area, show_sidebar);

    render_messages(
        frame,
        layout.message_area,
        &app.messages,
        &mut app.message_area_state,
        &app.theme,
        app.is_loading,
    );

    if let Some(sidebar_area) = layout.sidebar {
        render_sidebar(
            frame,
            sidebar_area,
            &app.sidebar_state,
            &app.theme,
        );
    }

    render_input(
        frame,
        layout.input_area,
        &mut app.input,
        &app.theme,
    );
}
