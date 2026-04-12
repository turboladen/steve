//! Floating overlay for browsing LSP diagnostics across the project.
//!
//! Follows the `diagnostics_overlay.rs` pattern: guarded by `state.visible`,
//! centered in the message area, with `Clear` behind and bordered popup.
//! Displays diagnostics grouped by file, sorted by severity then line.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

use super::theme::Theme;

// ─── Snapshot types ────────────────────────────────────────

/// LSP diagnostic severity, ordered for sorting (most severe first).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LspSeverity {
    Error = 0,
    Warning = 1,
    Information = 2,
    Hint = 3,
}

impl LspSeverity {
    /// Convert from the LSP protocol severity. `None` defaults to `Hint`.
    pub fn from_lsp(severity: Option<async_lsp::lsp_types::DiagnosticSeverity>) -> Self {
        use async_lsp::lsp_types::DiagnosticSeverity;
        match severity {
            Some(DiagnosticSeverity::ERROR) => Self::Error,
            Some(DiagnosticSeverity::WARNING) => Self::Warning,
            Some(DiagnosticSeverity::INFORMATION) => Self::Information,
            Some(DiagnosticSeverity::HINT) => Self::Hint,
            _ => Self::Hint,
        }
    }

    /// Distinct shape icon for each severity (colorblind-safe).
    pub fn icon(self) -> &'static str {
        match self {
            Self::Error => "\u{2716}",       // ✖
            Self::Warning => "\u{26a0}",     // ⚠
            Self::Information => "\u{2139}", // ℹ
            Self::Hint => "\u{2022}",        // •
        }
    }
}

/// A single diagnostic entry, pre-processed for display.
#[derive(Debug, Clone)]
pub struct LspDiagnosticEntry {
    pub severity: LspSeverity,
    /// 0-indexed line number (displayed as 1-indexed).
    pub line: u32,
    /// 0-indexed column.
    pub column: u32,
    pub message: String,
    /// LSP server source (e.g. "rust-analyzer").
    pub source: Option<String>,
}

/// Diagnostics for a single file.
#[derive(Debug, Clone)]
pub struct FileDiagnostics {
    pub relative_path: String,
    pub entries: Vec<LspDiagnosticEntry>,
}

/// Owned snapshot of all LSP diagnostics, taken at overlay-open time.
#[derive(Debug, Clone, Default)]
pub struct LspDiagnosticsSnapshot {
    pub files: Vec<FileDiagnostics>,
    pub total_errors: usize,
    pub total_warnings: usize,
    pub total_other: usize,
}

// ─── Overlay state ─────────────────────────────────────────

/// State for the LSP diagnostics overlay.
#[derive(Debug, Default)]
pub struct LspDiagnosticsOverlayState {
    pub visible: bool,
    snapshot: LspDiagnosticsSnapshot,
    scroll_offset: usize,
}

impl LspDiagnosticsOverlayState {
    pub fn open(&mut self, snapshot: LspDiagnosticsSnapshot) {
        self.visible = true;
        self.snapshot = snapshot;
        self.scroll_offset = 0;
    }

    pub fn close(&mut self) {
        self.visible = false;
        self.snapshot = LspDiagnosticsSnapshot::default();
        self.scroll_offset = 0;
    }

    pub fn scroll_up(&mut self, n: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
    }

    pub fn scroll_down(&mut self, n: usize) {
        // Generous upper bound: ~3 lines per entry + 2 per file header
        let max = self
            .snapshot
            .files
            .iter()
            .map(|f| 2 + f.entries.len() * 3)
            .sum::<usize>();
        self.scroll_offset = self.scroll_offset.saturating_add(n).min(max);
    }
}

// ─── Content builder ───────────────────────────────────────

/// Max display width for content inside the popup (70 - 2 borders).
const INNER_WIDTH: usize = 68;

fn build_content_lines<'a>(
    snapshot: &'a LspDiagnosticsSnapshot,
    theme: &'a Theme,
) -> Vec<Line<'a>> {
    let mut lines: Vec<Line> = Vec::new();

    if snapshot.files.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No diagnostics \u{2713}",
            Style::default().fg(theme.success),
        )));
        return lines;
    }

    // Summary line
    let summary = format!(
        "  \u{2716} {} errors  \u{26a0} {} warnings  \u{2139} {} info/hints",
        snapshot.total_errors, snapshot.total_warnings, snapshot.total_other,
    );
    lines.push(Line::from(Span::styled(
        summary,
        Style::default().fg(theme.dim),
    )));
    lines.push(Line::from(""));

    for (i, file) in snapshot.files.iter().enumerate() {
        // File header (truncated to fit popup width)
        let header = crate::truncate_chars(&format!("  {}", file.relative_path), INNER_WIDTH);
        lines.push(Line::from(Span::styled(
            header,
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )));

        for entry in &file.entries {
            let icon = entry.severity.icon();
            let icon_color = match entry.severity {
                LspSeverity::Error => theme.error,
                LspSeverity::Warning => theme.warning,
                LspSeverity::Information => theme.accent,
                LspSeverity::Hint => theme.dim,
            };

            // Prefix: "    {icon} L{line}:{col}  " — truncate message to remaining width
            let loc = format!("L{}:{}", entry.line + 1, entry.column + 1);
            let prefix_len = 4 + icon.chars().count() + 1 + loc.len() + 2; // indent + icon + space + loc + gap
            let source_len = entry
                .source
                .as_ref()
                .map(|s| s.chars().count() + 4) // "  [{src}]"
                .unwrap_or(0);
            let msg_budget = INNER_WIDTH.saturating_sub(prefix_len + source_len);
            let msg = crate::truncate_chars(&entry.message, msg_budget);

            let mut spans = vec![
                Span::styled(format!("    {icon} "), Style::default().fg(icon_color)),
                Span::styled(loc, Style::default().fg(theme.dim)),
                Span::styled("  ", Style::default()),
                Span::styled(msg, Style::default().fg(theme.fg)),
            ];

            if let Some(src) = &entry.source {
                spans.push(Span::styled(
                    format!("  [{src}]"),
                    Style::default().fg(theme.dim),
                ));
            }

            lines.push(Line::from(spans));
        }

        // Blank line between files (not after the last)
        if i + 1 < snapshot.files.len() {
            lines.push(Line::from(""));
        }
    }

    lines
}

// ─── Render ────────────────────────────────────────────────

/// Render the LSP diagnostics overlay centered in the message area.
pub fn render_lsp_diagnostics_overlay(
    frame: &mut Frame,
    message_area: Rect,
    state: &LspDiagnosticsOverlayState,
    theme: &Theme,
    context_pct: u8,
) {
    if !state.visible {
        return;
    }

    let lines = build_content_lines(&state.snapshot, theme);

    let content_height = lines.len() as u16;
    let popup_height = (content_height + 4)
        .min(message_area.height.saturating_sub(2))
        .max(8);
    let popup_width = 70u16.min(message_area.width.saturating_sub(4));

    if popup_width < 20 || popup_height < 5 {
        return;
    }

    let popup_x = message_area.x + (message_area.width.saturating_sub(popup_width)) / 2;
    let popup_y = message_area.y + (message_area.height.saturating_sub(popup_height)) / 2;

    let popup_area = Rect {
        x: popup_x,
        y: popup_y,
        width: popup_width,
        height: popup_height,
    };

    frame.render_widget(Clear, popup_area);

    let border_style = Style::default().fg(theme.border_color(context_pct));
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(Line::from(vec![Span::styled(
            " LSP Diagnostics ",
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

    let paragraph = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((state.scroll_offset as u16, 0));

    frame.render_widget(paragraph, inner);
}

// ─── Tests ─────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use async_lsp::lsp_types::DiagnosticSeverity;
    use ratatui::layout::Rect;

    fn sample_snapshot() -> LspDiagnosticsSnapshot {
        LspDiagnosticsSnapshot {
            files: vec![
                FileDiagnostics {
                    relative_path: "src/main.rs".into(),
                    entries: vec![
                        LspDiagnosticEntry {
                            severity: LspSeverity::Error,
                            line: 10,
                            column: 4,
                            message: "cannot find value `foo`".into(),
                            source: Some("rust-analyzer".into()),
                        },
                        LspDiagnosticEntry {
                            severity: LspSeverity::Warning,
                            line: 20,
                            column: 0,
                            message: "unused variable `bar`".into(),
                            source: Some("rust-analyzer".into()),
                        },
                    ],
                },
                FileDiagnostics {
                    relative_path: "src/lib.rs".into(),
                    entries: vec![LspDiagnosticEntry {
                        severity: LspSeverity::Hint,
                        line: 5,
                        column: 8,
                        message: "consider using `let`".into(),
                        source: None,
                    }],
                },
            ],
            total_errors: 1,
            total_warnings: 1,
            total_other: 1,
        }
    }

    // ─── LspSeverity tests ───

    #[test]
    fn severity_from_lsp_all_variants() {
        assert_eq!(
            LspSeverity::from_lsp(Some(DiagnosticSeverity::ERROR)),
            LspSeverity::Error
        );
        assert_eq!(
            LspSeverity::from_lsp(Some(DiagnosticSeverity::WARNING)),
            LspSeverity::Warning
        );
        assert_eq!(
            LspSeverity::from_lsp(Some(DiagnosticSeverity::INFORMATION)),
            LspSeverity::Information
        );
        assert_eq!(
            LspSeverity::from_lsp(Some(DiagnosticSeverity::HINT)),
            LspSeverity::Hint
        );
        assert_eq!(LspSeverity::from_lsp(None), LspSeverity::Hint);
    }

    #[test]
    fn severity_ordering() {
        assert!(LspSeverity::Error < LspSeverity::Warning);
        assert!(LspSeverity::Warning < LspSeverity::Information);
        assert!(LspSeverity::Information < LspSeverity::Hint);
    }

    #[test]
    fn severity_icons_distinct() {
        let icons: Vec<&str> = [
            LspSeverity::Error,
            LspSeverity::Warning,
            LspSeverity::Information,
            LspSeverity::Hint,
        ]
        .iter()
        .map(|s| s.icon())
        .collect();
        // All icons should be unique
        for (i, a) in icons.iter().enumerate() {
            for (j, b) in icons.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b, "icons at {i} and {j} should be distinct");
                }
            }
        }
    }

    // ─── State tests ───

    #[test]
    fn default_not_visible() {
        let state = LspDiagnosticsOverlayState::default();
        assert!(!state.visible);
        assert!(state.snapshot.files.is_empty());
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn open_sets_visible_and_snapshot() {
        let mut state = LspDiagnosticsOverlayState::default();
        state.open(sample_snapshot());
        assert!(state.visible);
        assert_eq!(state.snapshot.files.len(), 2);
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn close_resets_all_state() {
        let mut state = LspDiagnosticsOverlayState::default();
        state.open(sample_snapshot());
        state.scroll_down(5);
        state.close();
        assert!(!state.visible);
        assert!(state.snapshot.files.is_empty());
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn scroll_up_clamps_at_zero() {
        let mut state = LspDiagnosticsOverlayState::default();
        state.open(sample_snapshot());
        state.scroll_up(10);
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn scroll_down_increments() {
        let mut state = LspDiagnosticsOverlayState::default();
        state.open(sample_snapshot());
        state.scroll_down(3);
        assert_eq!(state.scroll_offset, 3);
    }

    #[test]
    fn scroll_round_trip() {
        let mut state = LspDiagnosticsOverlayState::default();
        state.open(sample_snapshot());
        state.scroll_down(5);
        state.scroll_up(2);
        assert_eq!(state.scroll_offset, 3);
    }

    // ─── Render tests ───

    fn render_overlay_to_string(
        width: u16,
        height: u16,
        state: &LspDiagnosticsOverlayState,
        message_area: Rect,
    ) -> String {
        let theme = Theme::default();
        let buf = super::super::render_to_buffer(width, height, |frame| {
            render_lsp_diagnostics_overlay(frame, message_area, state, &theme, 0);
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
        let state = LspDiagnosticsOverlayState::default();
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
        let mut state = LspDiagnosticsOverlayState::default();
        state.open(sample_snapshot());
        let area = Rect::new(0, 0, 80, 24);
        let text = render_overlay_to_string(80, 24, &state, area);
        assert!(
            text.contains("LSP Diagnostics"),
            "should show title, got:\n{text}"
        );
    }

    #[test]
    fn render_shows_file_paths() {
        let mut state = LspDiagnosticsOverlayState::default();
        state.open(sample_snapshot());
        let area = Rect::new(0, 0, 80, 30);
        let text = render_overlay_to_string(80, 30, &state, area);
        assert!(text.contains("src/main.rs"), "should show first file path");
        assert!(text.contains("src/lib.rs"), "should show second file path");
    }

    #[test]
    fn render_shows_diagnostic_messages() {
        let mut state = LspDiagnosticsOverlayState::default();
        state.open(sample_snapshot());
        let area = Rect::new(0, 0, 80, 30);
        let text = render_overlay_to_string(80, 30, &state, area);
        assert!(
            text.contains("cannot find value"),
            "should show error message"
        );
        assert!(text.contains("unused variable"), "should show warning");
    }

    #[test]
    fn render_shows_severity_icons() {
        let mut state = LspDiagnosticsOverlayState::default();
        state.open(sample_snapshot());
        let area = Rect::new(0, 0, 80, 30);
        let text = render_overlay_to_string(80, 30, &state, area);
        assert!(text.contains("\u{2716}"), "should show error icon ✖");
        assert!(text.contains("\u{26a0}"), "should show warning icon ⚠");
        assert!(text.contains("\u{2022}"), "should show hint icon •");
    }

    #[test]
    fn render_shows_line_numbers() {
        let mut state = LspDiagnosticsOverlayState::default();
        state.open(sample_snapshot());
        let area = Rect::new(0, 0, 80, 30);
        let text = render_overlay_to_string(80, 30, &state, area);
        // Lines are 0-indexed internally, displayed as 1-indexed
        assert!(text.contains("L11:5"), "should show 1-indexed line:col");
    }

    #[test]
    fn render_shows_source() {
        let mut state = LspDiagnosticsOverlayState::default();
        state.open(sample_snapshot());
        let area = Rect::new(0, 0, 80, 30);
        let text = render_overlay_to_string(80, 30, &state, area);
        assert!(
            text.contains("[rust-analyzer]"),
            "should show source, got:\n{text}"
        );
    }

    #[test]
    fn render_empty_snapshot_shows_no_diagnostics() {
        let mut state = LspDiagnosticsOverlayState::default();
        state.open(LspDiagnosticsSnapshot::default());
        let area = Rect::new(0, 0, 80, 24);
        let text = render_overlay_to_string(80, 24, &state, area);
        assert!(
            text.contains("No diagnostics"),
            "empty snapshot should show 'No diagnostics', got:\n{text}"
        );
    }

    #[test]
    fn render_shows_summary() {
        let mut state = LspDiagnosticsOverlayState::default();
        state.open(sample_snapshot());
        let area = Rect::new(0, 0, 80, 30);
        let text = render_overlay_to_string(80, 30, &state, area);
        assert!(text.contains("1 errors"), "should show error count");
        assert!(text.contains("1 warnings"), "should show warning count");
    }

    #[test]
    fn render_shows_hints() {
        let mut state = LspDiagnosticsOverlayState::default();
        state.open(sample_snapshot());
        let area = Rect::new(0, 0, 80, 30);
        let text = render_overlay_to_string(80, 30, &state, area);
        assert!(text.contains("Esc close"), "should show key hints");
    }
}
