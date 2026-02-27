use ratatui::layout::{Constraint, Direction, Layout, Rect};

/// Computed layout regions for the app.
pub struct AppLayout {
    pub message_area: Rect,
    pub input_area: Rect,
    pub sidebar: Option<Rect>,
}

const SIDEBAR_WIDTH: u16 = 40;
const SIDEBAR_MIN_TERMINAL_WIDTH: u16 = 120;
/// Input height: 1 context line + 3 textarea rows = 4.
const INPUT_HEIGHT: u16 = 4;

/// Compute the layout given the full terminal area.
///
/// Layout order (top to bottom):
/// - Message area (fills remaining space)
/// - Input area (4 rows: 1 context line + 3 textarea)
///
/// Sidebar (if shown) sits to the right of messages + input.
pub fn compute_layout(area: Rect, show_sidebar: bool) -> AppLayout {
    let sidebar_visible = show_sidebar && area.width >= SIDEBAR_MIN_TERMINAL_WIDTH;

    if sidebar_visible {
        // Split horizontally: content | sidebar
        let horizontal = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Min(40),
                Constraint::Length(SIDEBAR_WIDTH),
            ])
            .split(area);

        let content_area = horizontal[0];
        let sidebar = horizontal[1];

        // Split content vertically: messages | input
        let vertical = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(INPUT_HEIGHT),
            ])
            .split(content_area);

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
        // Input at bottom, 4 rows (1 context + 3 textarea)
        assert_eq!(layout.input_area.height, INPUT_HEIGHT);
        assert_eq!(layout.input_area.y, 20); // 24 - 4(input)
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
    }

    #[test]
    fn layout_sidebar_not_shown_below_threshold() {
        let layout = compute_layout(rect(119, 24), true);
        assert!(layout.sidebar.is_none());
    }

    #[test]
    fn layout_input_height_includes_context_line() {
        let layout = compute_layout(rect(80, 24), false);
        // 4 rows: 1 for context line + 3 for textarea
        assert_eq!(layout.input_area.height, 4);
    }
}
