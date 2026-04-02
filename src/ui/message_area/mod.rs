mod render;
pub use render::render_message_blocks;

use ratatui::{
    style::Style,
    text::{Line, Span},
};

use super::{selection::ContentMap, theme::Theme};
use crate::tool::{ToolName, ToolVisualCategory};

/// State for the scrollable message area.
///
/// Coordinate system: `scroll_offset = 0` means top of content.
/// Auto-scroll sets `scroll_offset = max_scroll` (bottom of content).
/// This aligns with ratatui's `Paragraph::scroll((row, 0))` API.
pub struct MessageAreaState {
    /// Current scroll position (0 = top of content).
    pub scroll_offset: u16,
    /// Whether to automatically scroll to follow new content.
    pub auto_scroll: bool,
    /// Total content height from last render (used for clamping).
    content_height: u16,
    /// Visible area height from last render.
    visible_height: u16,
    /// Content map built during last render (for coordinate mapping).
    pub content_map: Option<ContentMap>,
}

impl Default for MessageAreaState {
    fn default() -> Self {
        Self {
            scroll_offset: 0,
            auto_scroll: true,
            content_height: 0,
            visible_height: 0,
            content_map: None,
        }
    }
}

impl MessageAreaState {
    /// Maximum scroll offset (0 if content fits in view).
    pub fn max_scroll(&self) -> u16 {
        self.content_height.saturating_sub(self.visible_height)
    }

    /// Scroll toward older content (up). Disables auto-scroll.
    pub fn scroll_up(&mut self, amount: u16) {
        self.scroll_offset = self.scroll_offset.saturating_sub(amount);
        self.auto_scroll = false;
    }

    /// Scroll toward newer content (down). Re-enables auto-scroll at bottom.
    pub fn scroll_down(&mut self, amount: u16) {
        let max = self.max_scroll();
        self.scroll_offset = self.scroll_offset.saturating_add(amount).min(max);
        if self.scroll_offset >= max {
            self.auto_scroll = true;
        }
    }

    /// Visible area height (for page-size scrolling).
    pub fn visible_height(&self) -> u16 {
        self.visible_height
    }

    /// Jump to the bottom (newest content). Re-enables auto-scroll.
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = self.max_scroll();
        self.auto_scroll = true;
    }

    /// Update dimensions from render. If auto-scroll, jump to bottom.
    /// Clamp offset to valid range.
    pub fn update_dimensions(&mut self, content_height: u16, visible_height: u16) {
        self.content_height = content_height;
        self.visible_height = visible_height;
        let max = self.max_scroll();
        if self.auto_scroll {
            self.scroll_offset = max;
        } else {
            self.scroll_offset = self.scroll_offset.min(max);
        }
    }
}

/// Width of the activity rail gutter in columns: marker (1) + space (1) + separator (1).
pub(super) const GUTTER_WIDTH: usize = 3;

/// What to show in the left gutter for a given line.
#[derive(Debug, Clone, Copy)]
pub(super) enum GutterMark {
    /// Empty gutter — text, user, system, error, blanks.
    Empty,
    /// Tool header line — shows the tool's marker character.
    ToolMarker(ToolName),
    /// Continuation line (expanded diff/output) — shows dim pipe.
    Continuation(ToolName),
    /// Intent indicator line — shows dim dash.
    Intent,
}

/// Resolve the UI color for a tool name via `ToolVisualCategory`.
pub(super) fn tool_color(name: ToolName, theme: &Theme) -> ratatui::style::Color {
    match name.visual_category() {
        ToolVisualCategory::Read => theme.tool_read,
        ToolVisualCategory::Write => theme.tool_write,
        ToolVisualCategory::Accent => theme.accent,
    }
}

/// Return a guaranteed-1-column marker character for the gutter.
/// Delegates to `ToolName::gutter_char()`.
fn gutter_marker(name: ToolName) -> &'static str {
    name.gutter_char()
}

/// Build gutter spans for a line based on its mark type.
fn gutter_spans(mark: GutterMark, theme: &Theme) -> Vec<Span<'static>> {
    match mark {
        GutterMark::Empty => vec![Span::raw("   ")],
        GutterMark::ToolMarker(name) => vec![
            Span::styled(
                gutter_marker(name).to_string(),
                Style::default().fg(tool_color(name, theme)),
            ),
            Span::styled(" \u{2502}", Style::default().fg(theme.dim)),
        ],
        GutterMark::Continuation(_) => vec![Span::styled(
            "\u{2502} \u{2502}",
            Style::default().fg(theme.dim),
        )],
        GutterMark::Intent => vec![Span::styled(
            "\u{2500} \u{2502}",
            Style::default().fg(theme.dim),
        )],
    }
}

/// Prepend gutter spans to a line, preserving its existing style.
fn prepend_gutter<'a>(line: Line<'a>, mark: GutterMark, theme: &Theme) -> Line<'a> {
    let gutter = gutter_spans(mark, theme);
    let mut spans: Vec<Span<'a>> = Vec::with_capacity(gutter.len() + line.spans.len());
    for s in gutter {
        spans.push(s);
    }
    spans.extend(line.spans);
    Line::from(spans).style(line.style)
}

/// Wrapper around `Vec<Line>` that auto-prepends gutter marks to every line
/// and tracks parallel plain text + raw markdown for ContentMap building.
pub(super) struct GutteredLines<'a> {
    lines: Vec<Line<'a>>,
    /// Plain text for each line (gutter-stripped), parallel to `lines`.
    /// Used for display-width wrapping calculations.
    texts: Vec<String>,
    /// Raw markdown source for each line, parallel to `texts`.
    /// Used for clipboard copy — preserves original markdown syntax.
    raws: Vec<String>,
    theme: &'a Theme,
}

impl<'a> GutteredLines<'a> {
    fn new(theme: &'a Theme) -> Self {
        Self {
            lines: Vec::new(),
            texts: Vec::new(),
            raws: Vec::new(),
            theme,
        }
    }

    fn push(&mut self, line: Line<'a>, mark: GutterMark) {
        let plain = extract_plain_text(&line);
        self.lines.push(prepend_gutter(line, mark, self.theme));
        self.raws.push(plain.clone());
        self.texts.push(plain);
    }

    /// Push a line with explicit plain text and raw markdown overrides.
    /// Use when the line contains decoration spans (e.g. `│ `) that shouldn't
    /// appear in clipboard text, or when raw markdown differs from plain text.
    fn push_with_text(&mut self, line: Line<'a>, mark: GutterMark, plain: String, raw: String) {
        self.lines.push(prepend_gutter(line, mark, self.theme));
        self.texts.push(plain);
        self.raws.push(raw);
    }

    /// Extend with lines from a helper function, applying the same mark to all.
    fn extend(&mut self, new_lines: Vec<Line<'a>>, mark: GutterMark) {
        for line in new_lines {
            let plain = extract_plain_text(&line);
            self.lines.push(prepend_gutter(line, mark, self.theme));
            self.raws.push(plain.clone());
            self.texts.push(plain);
        }
    }

    fn into_parts(self) -> (Vec<Line<'a>>, Vec<String>, Vec<String>) {
        (self.lines, self.texts, self.raws)
    }
}

/// Extract plain text content from a Line's spans (before gutter prepend).
pub(super) fn extract_plain_text(line: &Line<'_>) -> String {
    let mut text = String::new();
    for span in &line.spans {
        text.push_str(&span.content);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_starts_at_zero_with_auto_scroll() {
        let state = MessageAreaState::default();
        assert_eq!(state.scroll_offset, 0);
        assert!(state.auto_scroll);
    }

    #[test]
    fn update_dimensions_auto_scrolls_to_bottom() {
        let mut state = MessageAreaState::default();
        state.update_dimensions(100, 20);
        assert_eq!(state.scroll_offset, 80); // max_scroll = 100 - 20
        assert!(state.auto_scroll);
    }

    #[test]
    fn scroll_up_disables_auto_scroll() {
        let mut state = MessageAreaState::default();
        state.update_dimensions(100, 20);
        assert_eq!(state.scroll_offset, 80);
        state.scroll_up(5);
        assert_eq!(state.scroll_offset, 75);
        assert!(!state.auto_scroll);
    }

    #[test]
    fn scroll_up_clamps_at_zero() {
        let mut state = MessageAreaState::default();
        state.update_dimensions(100, 20);
        state.scroll_up(200);
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn scroll_down_to_bottom_re_enables_auto_scroll() {
        let mut state = MessageAreaState::default();
        state.update_dimensions(100, 20);
        state.scroll_up(30); // at 50 now
        assert!(!state.auto_scroll);
        state.scroll_down(30); // at 80 = max_scroll
        assert_eq!(state.scroll_offset, 80);
        assert!(state.auto_scroll);
    }

    #[test]
    fn scroll_down_clamps_at_max() {
        let mut state = MessageAreaState::default();
        state.update_dimensions(100, 20);
        state.scroll_up(10); // at 70
        state.scroll_down(200); // should clamp to 80
        assert_eq!(state.scroll_offset, 80);
    }

    #[test]
    fn update_dimensions_clamps_when_not_auto_scrolling() {
        let mut state = MessageAreaState::default();
        state.update_dimensions(100, 20);
        state.scroll_up(10); // at 70, auto_scroll = false
        // Content shrinks (e.g., after compact)
        state.update_dimensions(50, 20);
        // max_scroll = 30, so offset should clamp from 70 to 30
        assert_eq!(state.scroll_offset, 30);
        assert!(!state.auto_scroll);
    }

    #[test]
    fn max_scroll_zero_when_content_fits() {
        let mut state = MessageAreaState::default();
        state.update_dimensions(10, 20);
        assert_eq!(state.max_scroll(), 0);
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn scroll_to_bottom_works() {
        let mut state = MessageAreaState::default();
        state.update_dimensions(100, 20);
        state.scroll_up(50); // at 30
        state.scroll_to_bottom();
        assert_eq!(state.scroll_offset, 80);
        assert!(state.auto_scroll);
    }

    // -- Gutter tests --

    use strum::IntoEnumIterator;

    #[test]
    fn tool_color_exhaustive() {
        let theme = Theme::default();
        for t in ToolName::iter() {
            let _color = tool_color(t, &theme);
            // Every variant returns a color without panicking.
        }
    }

    #[test]
    fn gutter_empty_for_text_lines() {
        let theme = Theme::default();
        let line = Line::from("hello");
        let guttered = prepend_gutter(line, GutterMark::Empty, &theme);
        let text: String = guttered.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            text.starts_with("   "),
            "empty gutter should be 3 spaces, got: {text}"
        );
        assert!(text.ends_with("hello"), "content should be preserved");
    }

    #[test]
    fn gutter_marker_for_read_tools() {
        let theme = Theme::default();
        let line = Line::from("read(src/main.rs)");
        let guttered = prepend_gutter(line, GutterMark::ToolMarker(ToolName::Read), &theme);
        let text: String = guttered.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            text.starts_with("\u{00b7}"),
            "read gutter should start with · marker, got: {text}"
        );
        assert!(
            text.contains("\u{2502}"),
            "gutter should contain │ separator"
        );
        assert!(
            text.ends_with("read(src/main.rs)"),
            "content should be preserved"
        );
        // Marker span should have tool_read color
        assert_eq!(guttered.spans[0].style.fg, Some(theme.tool_read));
    }

    #[test]
    fn gutter_marker_for_write_tools() {
        let theme = Theme::default();
        let line = Line::from("edit(src/main.rs)");
        let guttered = prepend_gutter(line, GutterMark::ToolMarker(ToolName::Edit), &theme);
        let text: String = guttered.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            text.starts_with("\u{270e}"),
            "write gutter should start with ✎ marker, got: {text}"
        );
        // Marker span should have tool_write color
        assert_eq!(guttered.spans[0].style.fg, Some(theme.tool_write));
    }

    #[test]
    fn gutter_continuation_for_expanded() {
        let theme = Theme::default();
        let line = Line::from("  +new_code");
        let guttered = prepend_gutter(line, GutterMark::Continuation(ToolName::Edit), &theme);
        let text: String = guttered.spans.iter().map(|s| s.content.as_ref()).collect();
        // Continuation: "│ │" (two pipes separated by space)
        assert!(
            text.starts_with("\u{2502} \u{2502}"),
            "continuation gutter should be │ │, got: {text}"
        );
        assert!(text.ends_with("+new_code"), "content preserved");
        // Continuation uses dim color
        assert_eq!(guttered.spans[0].style.fg, Some(theme.dim));
    }

    #[test]
    fn gutter_intent_line() {
        let theme = Theme::default();
        let line = Line::from("── exploring ──");
        let guttered = prepend_gutter(line, GutterMark::Intent, &theme);
        let text: String = guttered.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            text.starts_with("\u{2500} \u{2502}"),
            "intent gutter should be ─ │, got: {text}"
        );
        assert!(text.ends_with("exploring ──"), "content preserved");
    }

    #[test]
    fn prepend_gutter_preserves_content() {
        let theme = Theme::default();
        // Multi-span line with a style
        let line = Line::from(vec![
            Span::styled("hello ", Style::default().fg(theme.user_msg)),
            Span::styled("world", Style::default().fg(theme.accent)),
        ]);
        let guttered = prepend_gutter(line, GutterMark::Empty, &theme);
        // Should have gutter span(s) + original 2 spans
        let text: String = guttered.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "   hello world", "gutter + original content");
        // Original spans should be at the end
        let last_two = &guttered.spans[guttered.spans.len() - 2..];
        assert_eq!(last_two[0].content.as_ref(), "hello ");
        assert_eq!(last_two[1].content.as_ref(), "world");
    }

    #[test]
    fn prepend_gutter_preserves_line_style() {
        let theme = Theme::default();
        let line = Line::from("code").style(Style::default().bg(theme.code_bg));
        let guttered = prepend_gutter(line, GutterMark::Empty, &theme);
        assert_eq!(
            guttered.style.bg,
            Some(theme.code_bg),
            "line style should be preserved"
        );
    }

    #[test]
    fn gutter_marker_exhaustive() {
        // Every ToolName variant must return a non-empty 1-column marker.
        for t in ToolName::iter() {
            let m = gutter_marker(t);
            assert!(!m.is_empty(), "{t} gutter marker should be non-empty");
            assert_eq!(
                m.chars().count(),
                1,
                "{t} gutter marker should be exactly 1 char, got '{m}'"
            );
        }
    }

    #[test]
    fn gutter_width_is_three_chars() {
        let theme = Theme::default();
        // All mark types should produce exactly GUTTER_WIDTH (3) chars
        let marks = [
            GutterMark::Empty,
            GutterMark::ToolMarker(ToolName::Read),
            GutterMark::ToolMarker(ToolName::Edit),
            GutterMark::ToolMarker(ToolName::Bash),
            GutterMark::ToolMarker(ToolName::Question),
            GutterMark::Continuation(ToolName::Read),
            GutterMark::Intent,
        ];
        for mark in marks {
            let spans = gutter_spans(mark, &theme);
            let width: usize = spans.iter().map(|s| s.content.chars().count()).sum();
            assert_eq!(
                width, GUTTER_WIDTH,
                "gutter mark {mark:?} should be {GUTTER_WIDTH} chars, got {width}"
            );
        }
    }
}
