use ratatui::layout::{Constraint, Direction, Layout, Rect};

/// Computed layout regions for the app.
pub struct AppLayout {
    pub message_area: Rect,
    pub input_area: Rect,
    pub status_line: Rect,
    pub sidebar: Option<Rect>,
}

const SIDEBAR_WIDTH: u16 = 40;
const SIDEBAR_MIN_TERMINAL_WIDTH: u16 = 120;
const INPUT_HEIGHT: u16 = 3;
const STATUS_HEIGHT: u16 = 1;

/// Compute the layout given the full terminal area.
///
/// Layout order (top to bottom):
/// - Message area (fills remaining space)
/// - Input area (3 rows, adjacent to messages)
/// - Status line (1 row footer, spans full width including sidebar)
///
/// Sidebar (if shown) sits to the right of messages + input, but status line
/// spans below everything.
pub fn compute_layout(area: Rect, show_sidebar: bool) -> AppLayout {
    let sidebar_visible = show_sidebar && area.width >= SIDEBAR_MIN_TERMINAL_WIDTH;

    // First, split off the status line at the bottom (full width)
    let vertical_outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),              // main content
            Constraint::Length(STATUS_HEIGHT), // status line
        ])
        .split(area);

    let main_area = vertical_outer[0];
    let status_line = vertical_outer[1];

    if sidebar_visible {
        // Split main area horizontally: content | sidebar
        let horizontal = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Min(40),
                Constraint::Length(SIDEBAR_WIDTH),
            ])
            .split(main_area);

        let content_area = horizontal[0];
        let sidebar = horizontal[1];

        // Split content vertically: messages | input
        let vertical_inner = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(INPUT_HEIGHT),
            ])
            .split(content_area);

        AppLayout {
            message_area: vertical_inner[0],
            input_area: vertical_inner[1],
            status_line,
            sidebar: Some(sidebar),
        }
    } else {
        // No sidebar: just messages | input above status line
        let vertical_inner = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(INPUT_HEIGHT),
            ])
            .split(main_area);

        AppLayout {
            message_area: vertical_inner[0],
            input_area: vertical_inner[1],
            status_line,
            sidebar: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(width: u16, height: u16) -> Rect {
        Rect::new(0, 0, width, height)
    }

    #[test]
    fn layout_without_sidebar() {
        let layout = compute_layout(rect(80, 24), false);
        assert!(layout.sidebar.is_none());
        // Status line at bottom, 1 row
        assert_eq!(layout.status_line.height, STATUS_HEIGHT);
        assert_eq!(layout.status_line.y, 23); // last row of 24
        // Input above status
        assert_eq!(layout.input_area.height, INPUT_HEIGHT);
        assert_eq!(layout.input_area.y, 20); // 24 - 1(status) - 3(input)
        // Messages fill the rest
        assert_eq!(layout.message_area.y, 0);
        assert_eq!(layout.message_area.height, 20);
    }

    #[test]
    fn layout_with_sidebar() {
        let layout = compute_layout(rect(120, 24), true);
        assert!(layout.sidebar.is_some());
        let sidebar = layout.sidebar.unwrap();
        assert_eq!(sidebar.width, SIDEBAR_WIDTH);
        // Message area width = 120 - 40 = 80
        assert_eq!(layout.message_area.width, 80);
        // Status line spans full width
        assert_eq!(layout.status_line.width, 120);
    }

    #[test]
    fn layout_sidebar_not_shown_below_threshold() {
        let layout = compute_layout(rect(119, 24), true);
        assert!(layout.sidebar.is_none());
    }

    #[test]
    fn layout_status_line_always_full_width() {
        let layout = compute_layout(rect(150, 30), true);
        assert_eq!(layout.status_line.width, 150);
    }
}
