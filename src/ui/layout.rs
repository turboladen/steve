use ratatui::layout::{Constraint, Direction, Layout, Rect};

/// Computed layout regions for the app.
pub struct AppLayout {
    pub message_area: Rect,
    pub input_area: Rect,
    pub sidebar: Option<Rect>,
}

const SIDEBAR_WIDTH: u16 = 40;
const SIDEBAR_MIN_TERMINAL_WIDTH: u16 = 120;
const INPUT_HEIGHT: u16 = 3;

/// Compute the layout given the full terminal area.
pub fn compute_layout(area: Rect, show_sidebar: bool) -> AppLayout {
    let sidebar_visible = show_sidebar && area.width >= SIDEBAR_MIN_TERMINAL_WIDTH;

    if sidebar_visible {
        // Split horizontally: main content | sidebar
        let horizontal = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Min(40),
                Constraint::Length(SIDEBAR_WIDTH),
            ])
            .split(area);

        let main_area = horizontal[0];
        let sidebar = horizontal[1];

        // Split main area vertically: messages | input
        let vertical = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(INPUT_HEIGHT),
            ])
            .split(main_area);

        AppLayout {
            message_area: vertical[0],
            input_area: vertical[1],
            sidebar: Some(sidebar),
        }
    } else {
        // No sidebar: just messages | input
        let vertical = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(INPUT_HEIGHT),
            ])
            .split(area);

        AppLayout {
            message_area: vertical[0],
            input_area: vertical[1],
            sidebar: None,
        }
    }
}
