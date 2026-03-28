use ratatui::layout::{Constraint, Direction, Layout, Rect};

/// Computed layout regions for the app.
pub struct AppLayout {
    pub message_area: Rect,
    pub input_area: Rect,
    pub sidebar: Option<Rect>,
    /// 1-column gap between content and sidebar (visual separator without border chars).
    pub sidebar_separator: Option<Rect>,
}

const SIDEBAR_MIN_TERMINAL_WIDTH: u16 = 120;

/// Responsive sidebar width: wider at large terminals.
pub(super) fn sidebar_width(terminal_width: u16) -> u16 {
    if terminal_width >= 160 { 44 } else { 36 }
}

/// Compute the layout given the full terminal area and dynamic input height.
///
/// Layout order (top to bottom):
/// - Message area (fills remaining space)
/// - Input area (`input_height` rows)
///
/// Sidebar (if shown) sits to the right of messages + input.
pub fn compute_layout(area: Rect, show_sidebar: bool, input_height: u16) -> AppLayout {
    let sidebar_visible = show_sidebar && area.width >= SIDEBAR_MIN_TERMINAL_WIDTH;

    if sidebar_visible {
        let sb_width = sidebar_width(area.width);
        // Split horizontally: content | 1-col gap | sidebar
        // The gap prevents the sidebar border character from appearing in copied text.
        let horizontal = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Min(40),
                Constraint::Length(1), // visual separator (empty column)
                Constraint::Length(sb_width),
            ])
            .split(area);

        let content_area = horizontal[0];
        // horizontal[1] is the gap — nothing renders there
        let sidebar = horizontal[2];

        // Split content vertically: messages | input
        let vertical = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(input_height)])
            .split(content_area);

        AppLayout {
            message_area: vertical[0],
            input_area: vertical[1],
            sidebar: Some(sidebar),
            sidebar_separator: Some(horizontal[1]),
        }
    } else {
        // No sidebar: just messages | input
        let vertical = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(input_height)])
            .split(area);

        AppLayout {
            message_area: vertical[0],
            input_area: vertical[1],
            sidebar: None,
            sidebar_separator: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::input::MIN_INPUT_HEIGHT;

    fn rect(width: u16, height: u16) -> Rect {
        Rect::new(0, 0, width, height)
    }

    #[test]
    fn layout_without_sidebar() {
        let layout = compute_layout(rect(80, 24), false, MIN_INPUT_HEIGHT);
        assert!(layout.sidebar.is_none());
        // Input at bottom, 5 rows (1 border + 1 context + 3 textarea)
        assert_eq!(layout.input_area.height, MIN_INPUT_HEIGHT);
        assert_eq!(layout.input_area.y, 19); // 24 - 5(input)
        // Messages fill the rest
        assert_eq!(layout.message_area.y, 0);
        assert_eq!(layout.message_area.height, 19);
    }

    #[test]
    fn layout_with_sidebar() {
        let layout = compute_layout(rect(120, 24), true, MIN_INPUT_HEIGHT);
        assert!(layout.sidebar.is_some());
        let sidebar = layout.sidebar.unwrap();
        let sb_w = sidebar_width(120); // 36 at 120 cols
        assert_eq!(sidebar.width, sb_w);
        // Message area width = 120 - 1(sep) - sb_w(sidebar)
        assert_eq!(layout.message_area.width, 120 - 1 - sb_w);
        // Separator is 1 column between content and sidebar
        assert!(layout.sidebar_separator.is_some());
        assert_eq!(layout.sidebar_separator.unwrap().width, 1);
    }

    #[test]
    fn layout_wide_sidebar() {
        let layout = compute_layout(rect(160, 24), true, MIN_INPUT_HEIGHT);
        assert!(layout.sidebar.is_some());
        let sidebar = layout.sidebar.unwrap();
        let sb_w = sidebar_width(160); // 44 at 160 cols
        assert_eq!(sidebar.width, sb_w);
        assert_eq!(layout.message_area.width, 160 - 1 - sb_w);
    }

    #[test]
    fn layout_sidebar_not_shown_below_threshold() {
        let layout = compute_layout(rect(119, 24), true, MIN_INPUT_HEIGHT);
        assert!(layout.sidebar.is_none());
        assert!(layout.sidebar_separator.is_none());
    }

    #[test]
    fn layout_input_height_includes_context_line() {
        let layout = compute_layout(rect(80, 24), false, MIN_INPUT_HEIGHT);
        // 5 rows: 1 border + 1 context line + 3 textarea
        assert_eq!(layout.input_area.height, 5);
    }

    #[test]
    fn layout_expanded_input_reduces_message_area() {
        let layout = compute_layout(rect(80, 24), false, 10);
        assert_eq!(layout.input_area.height, 10);
        assert_eq!(layout.message_area.height, 14); // 24 - 10
        assert_eq!(layout.input_area.y, 14);
    }
}
