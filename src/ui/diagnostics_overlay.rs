//! Floating overlay for the diagnostics / health dashboard.
//!
//! Follows the `model_picker.rs` overlay pattern: guarded by `state.visible`,
//! centered in the message area, with `Clear` behind and bordered popup.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

use crate::diagnostics::{Category, DiagnosticCheck, Severity};

use super::theme::Theme;

/// State for the diagnostics overlay.
#[derive(Debug)]
pub struct DiagnosticsOverlayState {
    /// Whether the overlay is currently visible.
    pub visible: bool,
    /// The checks to display (populated on open).
    checks: Vec<DiagnosticCheck>,
    /// Current scroll offset.
    scroll_offset: usize,
}

impl Default for DiagnosticsOverlayState {
    fn default() -> Self {
        Self {
            visible: false,
            checks: Vec::new(),
            scroll_offset: 0,
        }
    }
}

impl DiagnosticsOverlayState {
    /// Open the overlay with the given checks.
    pub fn open(&mut self, checks: Vec<DiagnosticCheck>) {
        self.visible = true;
        self.checks = checks;
        self.scroll_offset = 0;
    }

    /// Close the overlay and reset state.
    pub fn close(&mut self) {
        self.visible = false;
        self.checks.clear();
        self.scroll_offset = 0;
    }

    /// Scroll up by `n` lines.
    pub fn scroll_up(&mut self, n: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
    }

    /// Scroll down by `n` lines, clamped to content length.
    pub fn scroll_down(&mut self, n: usize) {
        let max = self.checks.len().saturating_mul(4); // ~4 lines per check (generous upper bound)
        self.scroll_offset = self.scroll_offset.saturating_add(n).min(max);
    }
}

/// Severity icon character.
fn severity_icon(severity: Severity) -> &'static str {
    match severity {
        Severity::Info => "\u{2713}",    // ✓
        Severity::Warning => "\u{26a0}", // ⚠
        Severity::Error => "\u{2716}",   // ✖
    }
}

/// Build the content lines for the overlay, grouped by category.
fn build_content_lines<'a>(checks: &'a [DiagnosticCheck], theme: &'a Theme) -> Vec<Line<'a>> {
    let mut lines: Vec<Line> = Vec::new();

    // Display order for categories
    let categories = [
        Category::AiEnvironment,
        Category::LspHealth,
        Category::McpHealth,
        Category::SessionEfficiency,
    ];

    for category in categories {
        let cat_checks: Vec<&DiagnosticCheck> =
            checks.iter().filter(|c| c.category == category).collect();

        if cat_checks.is_empty() {
            continue;
        }

        // Category header
        lines.push(Line::from(Span::styled(
            category.label(),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )));

        for check in &cat_checks {
            let icon = severity_icon(check.severity);
            let icon_color = match check.severity {
                Severity::Info => theme.success,
                Severity::Warning => theme.warning,
                Severity::Error => theme.error,
            };

            // Icon + label line
            lines.push(Line::from(vec![
                Span::styled(format!("  {icon} "), Style::default().fg(icon_color)),
                Span::styled(&*check.label, Style::default().fg(theme.fg)),
            ]));

            // Detail line
            lines.push(Line::from(Span::styled(
                format!("    {}", check.detail),
                Style::default().fg(theme.dim),
            )));

            // Recommendation line (if present)
            if let Some(rec) = &check.recommendation {
                lines.push(Line::from(Span::styled(
                    format!("    \u{2192} {rec}"),
                    Style::default().fg(theme.dim),
                )));
            }
        }

        // Blank line between categories
        lines.push(Line::from(""));
    }

    // Remove trailing blank line
    if lines.last().is_some_and(|l| l.spans.is_empty()) {
        lines.pop();
    }

    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "  All checks passed \u{2713}",
            Style::default().fg(theme.success),
        )));
    }

    lines
}

/// Render the diagnostics overlay centered in the message area.
pub fn render_diagnostics_overlay(
    frame: &mut Frame,
    message_area: Rect,
    state: &DiagnosticsOverlayState,
    theme: &Theme,
    context_pct: u8,
) {
    if !state.visible {
        return;
    }

    let lines = build_content_lines(&state.checks, theme);

    // Calculate popup dimensions
    let content_height = lines.len() as u16;
    let popup_height = (content_height + 4) // +2 border +2 padding
        .min(message_area.height.saturating_sub(2))
        .max(8);
    let popup_width = 60u16.min(message_area.width.saturating_sub(4));

    if popup_width < 20 || popup_height < 5 {
        return;
    }

    // Center in message area
    let popup_x = message_area.x + (message_area.width.saturating_sub(popup_width)) / 2;
    let popup_y = message_area.y + (message_area.height.saturating_sub(popup_height)) / 2;

    let popup_area = Rect {
        x: popup_x,
        y: popup_y,
        width: popup_width,
        height: popup_height,
    };

    // Clear behind the popup
    frame.render_widget(Clear, popup_area);

    // Border block with title and hints
    let border_style = Style::default().fg(theme.border_color(context_pct));
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(Line::from(vec![Span::styled(
            " Health Dashboard ",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )]))
        .title_bottom(Line::from(vec![Span::styled(
            " \u{2191}\u{2193} scroll  Esc close ",
            Style::default().fg(theme.dim),
        )]));

    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    // Render content with scroll
    let paragraph = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((state.scroll_offset as u16, 0));

    frame.render_widget(paragraph, inner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::{DiagnosticCheck, Severity, Category};
    use ratatui::layout::Rect;

    fn sample_checks() -> Vec<DiagnosticCheck> {
        vec![
            DiagnosticCheck {
                severity: Severity::Warning,
                category: Category::AiEnvironment,
                label: "No AGENTS.md".into(),
                detail: "Missing project instructions".into(),
                recommendation: Some("Run /init".into()),
            },
            DiagnosticCheck {
                severity: Severity::Error,
                category: Category::LspHealth,
                label: "No LSP servers running".into(),
                detail: "Code intelligence unavailable".into(),
                recommendation: Some("Install language servers".into()),
            },
            DiagnosticCheck {
                severity: Severity::Info,
                category: Category::SessionEfficiency,
                label: "$0.0200/exchange".into(),
                detail: "Session total: $0.08".into(),
                recommendation: None,
            },
        ]
    }

    // ─── State tests ───

    #[test]
    fn default_not_visible() {
        let state = DiagnosticsOverlayState::default();
        assert!(!state.visible);
        assert!(state.checks.is_empty());
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn open_sets_visible_and_checks() {
        let mut state = DiagnosticsOverlayState::default();
        state.open(sample_checks());
        assert!(state.visible);
        assert_eq!(state.checks.len(), 3);
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn close_resets_all_state() {
        let mut state = DiagnosticsOverlayState::default();
        state.open(sample_checks());
        state.scroll_down(5);
        state.close();
        assert!(!state.visible);
        assert!(state.checks.is_empty());
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn scroll_up_clamps_at_zero() {
        let mut state = DiagnosticsOverlayState::default();
        state.open(sample_checks());
        state.scroll_up(10);
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn scroll_down_increments() {
        let mut state = DiagnosticsOverlayState::default();
        state.open(sample_checks());
        state.scroll_down(3);
        assert_eq!(state.scroll_offset, 3);
    }

    #[test]
    fn scroll_round_trip() {
        let mut state = DiagnosticsOverlayState::default();
        state.open(sample_checks());
        state.scroll_down(5);
        state.scroll_up(2);
        assert_eq!(state.scroll_offset, 3);
    }

    // ─── Render tests ───

    /// Helper: render overlay into a buffer and return text.
    fn render_overlay_to_string(
        width: u16,
        height: u16,
        state: &DiagnosticsOverlayState,
        message_area: Rect,
    ) -> String {
        let theme = Theme::default();
        let buf = super::super::render_to_buffer(width, height, |frame| {
            render_diagnostics_overlay(frame, message_area, state, &theme, 0);
        });
        let mut text = String::new();
        for y in 0..height {
            for x in 0..width {
                let cell = &buf[(x, y)];
                text.push_str(cell.symbol());
            }
            text.push('\n');
        }
        text
    }

    #[test]
    fn render_hidden_shows_nothing() {
        let state = DiagnosticsOverlayState::default();
        let area = Rect::new(0, 0, 80, 20);
        let text = render_overlay_to_string(80, 20, &state, area);
        let non_space: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(
            non_space.is_empty(),
            "hidden overlay should render nothing, got: '{non_space}'"
        );
    }

    #[test]
    fn render_shows_title() {
        let mut state = DiagnosticsOverlayState::default();
        state.open(sample_checks());
        let area = Rect::new(0, 0, 80, 24);
        let text = render_overlay_to_string(80, 24, &state, area);
        assert!(
            text.contains("Health Dashboard"),
            "should show title, got:\n{text}"
        );
    }

    #[test]
    fn render_shows_category_headers() {
        let mut state = DiagnosticsOverlayState::default();
        state.open(sample_checks());
        let area = Rect::new(0, 0, 80, 30);
        let text = render_overlay_to_string(80, 30, &state, area);
        assert!(text.contains("AI Environment"), "should show AI Environment header");
        assert!(text.contains("LSP Health"), "should show LSP Health header");
        assert!(text.contains("Session Efficiency"), "should show Session Efficiency header");
    }

    #[test]
    fn render_shows_check_labels() {
        let mut state = DiagnosticsOverlayState::default();
        state.open(sample_checks());
        let area = Rect::new(0, 0, 80, 30);
        let text = render_overlay_to_string(80, 30, &state, area);
        assert!(text.contains("No AGENTS.md"), "should show check label");
        assert!(text.contains("No LSP servers running"), "should show LSP check");
    }

    #[test]
    fn render_shows_severity_icons() {
        let mut state = DiagnosticsOverlayState::default();
        state.open(sample_checks());
        let area = Rect::new(0, 0, 80, 30);
        let text = render_overlay_to_string(80, 30, &state, area);
        assert!(text.contains("\u{26a0}"), "should show warning icon ⚠");
        assert!(text.contains("\u{2716}"), "should show error icon ✖");
        assert!(text.contains("\u{2713}"), "should show info icon ✓");
    }

    #[test]
    fn render_empty_checks_shows_all_clear() {
        let mut state = DiagnosticsOverlayState::default();
        state.open(vec![]);
        let area = Rect::new(0, 0, 80, 24);
        let text = render_overlay_to_string(80, 24, &state, area);
        assert!(
            text.contains("All checks passed"),
            "empty checks should show 'All checks passed', got:\n{text}"
        );
    }

    #[test]
    fn render_shows_hints() {
        let mut state = DiagnosticsOverlayState::default();
        state.open(sample_checks());
        let area = Rect::new(0, 0, 80, 30);
        let text = render_overlay_to_string(80, 30, &state, area);
        assert!(text.contains("Esc close"), "should show key hints");
    }
}
