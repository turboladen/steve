//! Floating overlay for interactive session selection.
//!
//! Opened by `/sessions`, allows type-to-filter, arrow navigation, and Enter to switch.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem},
};

use crate::session::types::SessionInfo;

use super::theme::Theme;

/// A single session entry for display in the picker.
#[derive(Debug, Clone)]
struct SessionEntry {
    /// Pre-formatted date string ("MM/DD HH:MM").
    date: String,
    /// Session title.
    title: String,
    /// Model reference (e.g. "openai/gpt-4o").
    model_ref: String,
    /// Whether this is the currently active session.
    is_current: bool,
}

/// State for the session picker overlay.
#[derive(Debug, Default)]
pub struct SessionPickerState {
    /// Whether the overlay is currently visible.
    pub visible: bool,
    /// Current filter text typed by the user.
    filter: String,
    /// All available sessions (sorted by updated_at desc, as provided).
    all_sessions: Vec<SessionEntry>,
    /// Original `SessionInfo` values (parallel to `all_sessions`).
    source_sessions: Vec<SessionInfo>,
    /// Indices into `all_sessions` matching the current filter.
    filtered: Vec<usize>,
    /// Index into `filtered` for the currently highlighted item.
    selected: usize,
}

impl SessionPickerState {
    /// Open the picker with the given sessions and current session ID.
    ///
    /// Sessions are displayed in the order provided (caller sorts by `updated_at` desc).
    pub fn open(&mut self, sessions: &[SessionInfo], current_id: Option<&str>) {
        self.visible = true;
        self.filter.clear();

        self.all_sessions = sessions
            .iter()
            .map(|s| SessionEntry {
                date: s.updated_at.format("%m/%d %H:%M").to_string(),
                title: s.title.clone(),
                model_ref: s.model_ref.clone(),
                is_current: current_id.is_some_and(|id| id == s.id),
            })
            .collect();
        self.source_sessions = sessions.to_vec();
        self.apply_filter();
    }

    /// Close the picker and reset state.
    pub fn close(&mut self) {
        self.visible = false;
        self.filter.clear();
        self.all_sessions.clear();
        self.source_sessions.clear();
        self.filtered.clear();
        self.selected = 0;
    }

    /// Add a character to the filter and recompute matches.
    pub fn type_char(&mut self, c: char) {
        self.filter.push(c);
        self.apply_filter();
    }

    /// Remove the last character from the filter and recompute matches.
    pub fn backspace(&mut self) {
        self.filter.pop();
        self.apply_filter();
    }

    /// Recompute filtered indices from the current filter text.
    fn apply_filter(&mut self) {
        let needle = self.filter.to_lowercase();
        self.filtered = self
            .all_sessions
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                if needle.is_empty() {
                    return true;
                }
                entry.title.to_lowercase().contains(&needle)
                    || entry.model_ref.to_lowercase().contains(&needle)
            })
            .map(|(i, _)| i)
            .collect();

        // Clamp selection
        if self.filtered.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len() - 1;
        }
    }

    /// Move selection down (wraps).
    pub fn next(&mut self) {
        let len = self.filtered.len();
        if len > 0 {
            self.selected = (self.selected + 1) % len;
        }
    }

    /// Move selection up (wraps).
    pub fn prev(&mut self) {
        let len = self.filtered.len();
        if len > 0 {
            self.selected = if self.selected == 0 {
                len - 1
            } else {
                self.selected - 1
            };
        }
    }

    /// Get the `SessionInfo` of the currently selected session, if any.
    pub fn selected_session(&self) -> Option<SessionInfo> {
        self.filtered
            .get(self.selected)
            .and_then(|&idx| self.source_sessions.get(idx))
            .cloned()
    }

    /// Get the current filter text.
    pub fn filter_text(&self) -> &str {
        &self.filter
    }

    /// Get the list of filtered session entries as (date, title, model_ref, is_current).
    pub fn filtered_sessions(&self) -> Vec<(&str, &str, &str, bool)> {
        self.filtered
            .iter()
            .map(|&idx| {
                let entry = &self.all_sessions[idx];
                (
                    entry.date.as_str(),
                    entry.title.as_str(),
                    entry.model_ref.as_str(),
                    entry.is_current,
                )
            })
            .collect()
    }
}

/// Maximum number of visible items in the picker list.
const MAX_VISIBLE_ITEMS: u16 = 12;

/// Render the session picker overlay centered in the message area.
pub fn render_session_picker(
    frame: &mut Frame,
    message_area: Rect,
    state: &SessionPickerState,
    theme: &Theme,
    context_pct: u8,
) {
    if !state.visible {
        return;
    }

    let sessions = state.filtered_sessions();

    // Calculate popup dimensions
    // Height: 2 (border) + 1 (filter line) + 1 (separator) + items
    let item_count = (sessions.len() as u16).min(MAX_VISIBLE_ITEMS);
    let inner_height = 2 + item_count; // filter + separator + items
    let popup_height = (inner_height + 2).min(message_area.height.saturating_sub(2)); // +2 for borders
    let popup_width = 70u16.min(message_area.width.saturating_sub(4));

    if popup_width < 20 || popup_height < 5 {
        return; // Too small to render meaningfully
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

    // Border block with title
    let border_style = Style::default().fg(theme.border_color(context_pct));
    let title_left = " Switch Session ";
    let hints_text = " ↑↓ navigate · Enter select · Esc close ";
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(Line::from(vec![Span::styled(
            title_left,
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )]))
        .title_bottom(Line::from(vec![Span::styled(
            hints_text,
            Style::default().fg(theme.dim),
        )]));

    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    // Filter input line: "> filter▌"
    let filter_display = format!("> {}\u{258c}", state.filter_text());
    let filter_line = Line::from(Span::styled(filter_display, Style::default().fg(theme.fg)));
    let filter_area = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: 1,
    };
    frame.render_widget(ratatui::widgets::Paragraph::new(filter_line), filter_area);

    // Separator line
    if inner.height < 2 {
        return;
    }
    let sep_text = "\u{2500}".repeat(inner.width as usize);
    let sep_line = Line::from(Span::styled(&*sep_text, Style::default().fg(theme.dim)));
    let sep_area = Rect {
        x: inner.x,
        y: inner.y + 1,
        width: inner.width,
        height: 1,
    };
    frame.render_widget(ratatui::widgets::Paragraph::new(sep_line), sep_area);

    // Session list
    let list_height = inner.height.saturating_sub(2);
    if list_height == 0 {
        return;
    }

    // Compute scroll offset to keep selected item visible
    let scroll_offset = if state.selected as u16 >= list_height {
        (state.selected as u16) - list_height + 1
    } else {
        0
    };

    let items: Vec<ListItem> = sessions
        .iter()
        .enumerate()
        .skip(scroll_offset as usize)
        .take(list_height as usize)
        .map(|(i, (date, title, model_ref, is_current))| {
            let is_selected = i == state.selected;
            let marker = if *is_current { "\u{25cf} " } else { "  " };

            let title_style = if is_selected {
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.fg)
            };

            let date_style = if is_selected {
                Style::default().fg(theme.accent)
            } else {
                Style::default().fg(theme.dim)
            };

            let marker_style = if *is_current {
                Style::default().fg(theme.accent)
            } else {
                Style::default().fg(theme.fg)
            };

            // Format: "● MM/DD HH:MM — Title  model_ref"
            // Truncate title if needed to fit model_ref suffix
            let prefix_len = marker.chars().count() + date.chars().count() + 3; // " — "
            let available = (inner.width as usize).saturating_sub(prefix_len);
            let model_suffix = format!("  {model_ref}");
            let title_max = available.saturating_sub(model_suffix.chars().count());

            let display_title = if title.chars().count() > title_max && title_max > 3 {
                let truncated: String = title.chars().take(title_max - 1).collect();
                format!("{truncated}…")
            } else {
                title.to_string()
            };

            ListItem::new(Line::from(vec![
                Span::styled(marker, marker_style),
                Span::styled(*date, date_style),
                Span::styled(" \u{2014} ", Style::default().fg(theme.dim)),
                Span::styled(display_title, title_style),
                Span::styled(model_suffix, Style::default().fg(theme.dim)),
            ]))
        })
        .collect();

    let list_area = Rect {
        x: inner.x,
        y: inner.y + 2,
        width: inner.width,
        height: list_height,
    };

    let list = List::new(items);
    frame.render_widget(list, list_area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::types::TokenUsage;
    use chrono::{TimeZone, Utc};

    fn sample_sessions() -> Vec<SessionInfo> {
        vec![
            SessionInfo {
                id: "s1".into(),
                project_id: "p1".into(),
                title: "Fix login bug".into(),
                model_ref: "openai/gpt-4o".into(),
                created_at: Utc.with_ymd_and_hms(2026, 3, 15, 10, 0, 0).unwrap(),
                updated_at: Utc.with_ymd_and_hms(2026, 3, 15, 14, 30, 0).unwrap(),
                token_usage: TokenUsage::default(),
            },
            SessionInfo {
                id: "s2".into(),
                project_id: "p1".into(),
                title: "Add session picker".into(),
                model_ref: "anthropic/claude-3-opus".into(),
                created_at: Utc.with_ymd_and_hms(2026, 3, 14, 9, 0, 0).unwrap(),
                updated_at: Utc.with_ymd_and_hms(2026, 3, 15, 12, 0, 0).unwrap(),
                token_usage: TokenUsage::default(),
            },
            SessionInfo {
                id: "s3".into(),
                project_id: "p1".into(),
                title: "Refactor tool system".into(),
                model_ref: "openai/gpt-4o-mini".into(),
                created_at: Utc.with_ymd_and_hms(2026, 3, 13, 8, 0, 0).unwrap(),
                updated_at: Utc.with_ymd_and_hms(2026, 3, 14, 16, 0, 0).unwrap(),
                token_usage: TokenUsage::default(),
            },
            SessionInfo {
                id: "s4".into(),
                project_id: "p1".into(),
                title: "Debug streaming issue".into(),
                model_ref: "anthropic/claude-3-sonnet".into(),
                created_at: Utc.with_ymd_and_hms(2026, 3, 12, 7, 0, 0).unwrap(),
                updated_at: Utc.with_ymd_and_hms(2026, 3, 13, 11, 0, 0).unwrap(),
                token_usage: TokenUsage::default(),
            },
        ]
    }

    // ─── State tests ───

    #[test]
    fn open_sets_visible_and_preserves_order() {
        let mut state = SessionPickerState::default();
        state.open(&sample_sessions(), Some("s1"));

        assert!(state.visible);
        assert_eq!(state.all_sessions.len(), 4);
        // Sessions should preserve input order (caller sorts by updated_at desc)
        assert_eq!(state.all_sessions[0].title, "Fix login bug");
        assert_eq!(state.all_sessions[1].title, "Add session picker");
        assert_eq!(state.all_sessions[2].title, "Refactor tool system");
        assert_eq!(state.all_sessions[3].title, "Debug streaming issue");
    }

    #[test]
    fn close_resets_all_state() {
        let mut state = SessionPickerState::default();
        state.open(&sample_sessions(), None);
        assert!(state.visible);

        state.close();
        assert!(!state.visible);
        assert!(state.all_sessions.is_empty());
        assert!(state.source_sessions.is_empty());
        assert!(state.filtered.is_empty());
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn empty_filter_shows_all() {
        let mut state = SessionPickerState::default();
        state.open(&sample_sessions(), None);
        assert_eq!(state.filtered.len(), 4);
    }

    #[test]
    fn filter_narrows_by_title() {
        let mut state = SessionPickerState::default();
        state.open(&sample_sessions(), None);

        for c in "login".chars() {
            state.type_char(c);
        }

        assert_eq!(state.filtered.len(), 1);
        let selected = state.selected_session().unwrap();
        assert_eq!(selected.title, "Fix login bug");
    }

    #[test]
    fn filter_narrows_by_model_ref() {
        let mut state = SessionPickerState::default();
        state.open(&sample_sessions(), None);

        for c in "claude".chars() {
            state.type_char(c);
        }

        // Should match anthropic/claude-3-opus and anthropic/claude-3-sonnet
        assert_eq!(state.filtered.len(), 2);
    }

    #[test]
    fn filter_case_insensitive() {
        let mut state = SessionPickerState::default();
        state.open(&sample_sessions(), None);

        for c in "FIX".chars() {
            state.type_char(c);
        }

        assert_eq!(state.filtered.len(), 1);
        let selected = state.selected_session().unwrap();
        assert_eq!(selected.title, "Fix login bug");
    }

    #[test]
    fn backspace_expands_results() {
        let mut state = SessionPickerState::default();
        state.open(&sample_sessions(), None);

        for c in "login".chars() {
            state.type_char(c);
        }
        assert_eq!(state.filtered.len(), 1);

        for _ in 0..5 {
            state.backspace();
        }
        assert_eq!(state.filtered.len(), 4);
    }

    #[test]
    fn navigation_wraps_forward() {
        let mut state = SessionPickerState::default();
        state.open(&sample_sessions(), None);

        for _ in 0..4 {
            state.next();
        }
        assert_eq!(state.selected, 0); // Wrapped back to start
    }

    #[test]
    fn navigation_wraps_backward() {
        let mut state = SessionPickerState::default();
        state.open(&sample_sessions(), None);

        state.prev();
        assert_eq!(state.selected, 3); // Wrapped to last
    }

    #[test]
    fn selected_session_correctness() {
        let mut state = SessionPickerState::default();
        state.open(&sample_sessions(), None);

        // First item
        let s = state.selected_session().unwrap();
        assert_eq!(s.id, "s1");

        state.next();
        let s = state.selected_session().unwrap();
        assert_eq!(s.id, "s2");
    }

    #[test]
    fn selected_session_none_when_no_matches() {
        let mut state = SessionPickerState::default();
        state.open(&sample_sessions(), None);

        for c in "zzzzz".chars() {
            state.type_char(c);
        }

        assert!(state.filtered.is_empty());
        assert!(state.selected_session().is_none());
    }

    #[test]
    fn filtered_sessions_marks_current() {
        let mut state = SessionPickerState::default();
        state.open(&sample_sessions(), Some("s2"));

        let sessions = state.filtered_sessions();
        let current = sessions
            .iter()
            .find(|(_, title, _, _)| *title == "Add session picker");
        assert!(current.is_some());
        assert!(current.unwrap().3); // is_current = true

        let other = sessions
            .iter()
            .find(|(_, title, _, _)| *title == "Fix login bug");
        assert!(other.is_some());
        assert!(!other.unwrap().3); // is_current = false
    }

    #[test]
    fn filter_clamps_selected_index() {
        let mut state = SessionPickerState::default();
        state.open(&sample_sessions(), None);

        // Select the last item
        state.selected = 3;

        // Filter to only 1 result
        for c in "login".chars() {
            state.type_char(c);
        }
        assert!(state.selected < state.filtered.len());
    }

    #[test]
    fn open_with_no_sessions() {
        let mut state = SessionPickerState::default();
        state.open(&[], None);

        assert!(state.visible);
        assert!(state.all_sessions.is_empty());
        assert!(state.filtered.is_empty());
        assert!(state.selected_session().is_none());
    }

    #[test]
    fn filter_text_returns_current_filter() {
        let mut state = SessionPickerState::default();
        state.open(&sample_sessions(), None);

        state.type_char('a');
        state.type_char('b');
        assert_eq!(state.filter_text(), "ab");

        state.backspace();
        assert_eq!(state.filter_text(), "a");
    }

    #[test]
    fn date_formatting() {
        let mut state = SessionPickerState::default();
        state.open(&sample_sessions(), None);

        // Check that dates are pre-formatted
        assert_eq!(state.all_sessions[0].date, "03/15 14:30");
    }

    // ─── Render tests ───

    use ratatui::layout::Rect;

    /// Helper: render session picker and return buffer text.
    fn render_picker_to_string(
        width: u16,
        height: u16,
        state: &SessionPickerState,
        message_area: Rect,
    ) -> String {
        let theme = Theme::default();
        let buf = super::super::render_to_buffer(width, height, |frame| {
            render_session_picker(frame, message_area, state, &theme, 0);
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
        let state = SessionPickerState::default();
        let area = Rect::new(0, 0, 80, 20);
        let text = render_picker_to_string(80, 20, &state, area);
        let non_space: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(
            non_space.is_empty(),
            "hidden picker should render nothing, got: '{non_space}'"
        );
    }

    #[test]
    fn render_shows_session_titles() {
        let mut state = SessionPickerState::default();
        state.open(&sample_sessions(), None);
        let area = Rect::new(0, 0, 80, 24);
        let text = render_picker_to_string(80, 24, &state, area);

        assert!(
            text.contains("Fix login bug"),
            "should show session title, got:\n{text}"
        );
        assert!(
            text.contains("Add session picker"),
            "should show session title, got:\n{text}"
        );
    }

    #[test]
    fn render_shows_dates() {
        let mut state = SessionPickerState::default();
        state.open(&sample_sessions(), None);
        let area = Rect::new(0, 0, 80, 24);
        let text = render_picker_to_string(80, 24, &state, area);

        assert!(
            text.contains("03/15 14:30"),
            "should show date, got:\n{text}"
        );
    }

    #[test]
    fn render_shows_title() {
        let mut state = SessionPickerState::default();
        state.open(&sample_sessions(), None);
        let area = Rect::new(0, 0, 80, 24);
        let text = render_picker_to_string(80, 24, &state, area);

        assert!(
            text.contains("Switch Session"),
            "should show title, got:\n{text}"
        );
    }

    #[test]
    fn render_highlights_selected_with_accent() {
        let mut state = SessionPickerState::default();
        state.open(&sample_sessions(), None);
        let area = Rect::new(0, 0, 80, 24);
        let theme = Theme::default();
        let buf = super::super::render_to_buffer(80, 24, |frame| {
            render_session_picker(frame, area, &state, &theme, 0);
        });

        // The selected item (first = "Fix login bug") should have accent color
        let mut found_accent = false;
        for y in 0..24 {
            for x in 0..80 {
                let cell = &buf[(x, y)];
                if cell.symbol() == "F" && cell.fg == theme.accent {
                    // "F" from "Fix login bug"
                    found_accent = true;
                    break;
                }
            }
            if found_accent {
                break;
            }
        }
        assert!(
            found_accent,
            "selected item should be rendered with accent color"
        );
    }

    #[test]
    fn render_shows_current_marker() {
        let mut state = SessionPickerState::default();
        state.open(&sample_sessions(), Some("s1"));
        let area = Rect::new(0, 0, 80, 24);
        let text = render_picker_to_string(80, 24, &state, area);

        assert!(
            text.contains("\u{25cf}"),
            "should show ● marker for current session, got:\n{text}"
        );
    }

    #[test]
    fn render_shows_filter_text() {
        let mut state = SessionPickerState::default();
        state.open(&sample_sessions(), None);
        state.type_char('l');
        state.type_char('o');
        let area = Rect::new(0, 0, 80, 24);
        let text = render_picker_to_string(80, 24, &state, area);

        assert!(
            text.contains("> lo"),
            "should show filter text, got:\n{text}"
        );
    }

    #[test]
    fn render_centered_in_area() {
        let mut state = SessionPickerState::default();
        state.open(&sample_sessions(), None);
        let area = Rect::new(0, 0, 100, 30);
        let theme = Theme::default();
        let buf = super::super::render_to_buffer(100, 30, |frame| {
            render_session_picker(frame, area, &state, &theme, 0);
        });

        // Check that column 0 is empty (popup should be centered, not left-aligned)
        let col0_content: String = (0..30)
            .map(|y| buf[(0u16, y)].symbol().to_string())
            .collect::<Vec<_>>()
            .join("");
        let col0_trimmed: String = col0_content
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        assert!(
            col0_trimmed.is_empty(),
            "column 0 should be empty (popup centered), got: '{col0_trimmed}'"
        );
    }
}
