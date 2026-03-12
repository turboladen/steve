use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

use super::theme::Theme;

/// A horizontal rule: `─` repeated to fill `width`, styled with `color`.
pub fn horizontal_rule(width: usize, color: ratatui::style::Color) -> Line<'static> {
    Line::from(Span::styled(
        "\u{2500}".repeat(width),
        Style::default().fg(color),
    ))
}

/// Diff box top border: `  ┌` followed by `─` repeated to fill remaining width.
pub fn diff_border_top(width: usize, color: ratatui::style::Color) -> Line<'static> {
    let prefix = "  \u{250c}";
    let dash_count = width.saturating_sub(1); // subtract the ┌
    Line::from(Span::styled(
        format!("{prefix}{}", "\u{2500}".repeat(dash_count)),
        Style::default().fg(color),
    ))
}

/// Diff box bottom border: `  └` followed by `─` repeated to fill remaining width.
pub fn diff_border_bottom(width: usize, color: ratatui::style::Color) -> Line<'static> {
    let prefix = "  \u{2514}";
    let dash_count = width.saturating_sub(1); // subtract the └
    Line::from(Span::styled(
        format!("{prefix}{}", "\u{2500}".repeat(dash_count)),
        Style::default().fg(color),
    ))
}

/// Section header: bold title in the given color.
pub fn section_header(title: &str, color: ratatui::style::Color) -> Line<'static> {
    Line::from(Span::styled(
        title.to_string(),
        Style::default()
            .fg(color)
            .add_modifier(Modifier::BOLD),
    ))
}

/// Thin dim separator between sidebar sections: `─` repeated to fill `width`.
pub fn section_separator(width: usize, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(
        "\u{2500}".repeat(width),
        Style::default().fg(theme.border),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn horizontal_rule_width_matches() {
        let theme = Theme::dark();
        let line = horizontal_rule(50, theme.permission);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text.chars().count(), 50);
    }

    #[test]
    fn horizontal_rule_zero_width() {
        let theme = Theme::dark();
        let line = horizontal_rule(0, theme.dim);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.is_empty());
    }

    #[test]
    fn diff_border_top_starts_with_corner() {
        let theme = Theme::dark();
        let line = diff_border_top(30, theme.border);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.starts_with("  \u{250c}"), "should start with '  ┌', got: {text}");
    }

    #[test]
    fn diff_border_bottom_starts_with_corner() {
        let theme = Theme::dark();
        let line = diff_border_bottom(30, theme.border);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.starts_with("  \u{2514}"), "should start with '  └', got: {text}");
    }

    #[test]
    fn diff_border_symmetry() {
        let theme = Theme::dark();
        let top = diff_border_top(40, theme.border);
        let bottom = diff_border_bottom(40, theme.border);
        let top_text: String = top.spans.iter().map(|s| s.content.as_ref()).collect();
        let bottom_text: String = bottom.spans.iter().map(|s| s.content.as_ref()).collect();
        // Same length (corners + dashes)
        assert_eq!(top_text.chars().count(), bottom_text.chars().count(),
            "top and bottom borders should have same char count");
    }

    #[test]
    fn section_header_is_bold() {
        let theme = Theme::dark();
        let line = section_header("Session", theme.accent);
        assert_eq!(line.spans.len(), 1);
        assert!(line.spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(line.spans[0].content.as_ref(), "Session");
    }

    #[test]
    fn section_separator_width_matches() {
        let theme = Theme::dark();
        let line = section_separator(36, &theme);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text.chars().count(), 36);
        assert_eq!(line.spans[0].style.fg, Some(theme.border));
    }
}
