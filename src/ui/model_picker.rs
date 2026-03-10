//! Floating overlay for interactive model selection.
//!
//! Opened by `/models`, allows type-to-filter, arrow navigation, and Enter to select.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem},
};

use super::theme::Theme;

/// A single model entry for display in the picker.
#[derive(Debug, Clone)]
struct ModelEntry {
    /// "provider/model" reference string.
    display_ref: String,
    /// Human-readable model name (e.g. "GPT-4o").
    display_name: String,
}

/// State for the model picker overlay.
#[derive(Debug)]
pub struct ModelPickerState {
    /// Whether the overlay is currently visible.
    pub visible: bool,
    /// Current filter text typed by the user.
    filter: String,
    /// All available models (sorted by display_ref).
    all_models: Vec<ModelEntry>,
    /// Indices into `all_models` matching the current filter.
    filtered: Vec<usize>,
    /// Index into `filtered` for the currently highlighted item.
    selected: usize,
    /// The currently active model ref (shown with ● marker).
    current_ref: Option<String>,
}

impl Default for ModelPickerState {
    fn default() -> Self {
        Self {
            visible: false,
            filter: String::new(),
            all_models: Vec::new(),
            filtered: Vec::new(),
            selected: 0,
            current_ref: None,
        }
    }
}

impl ModelPickerState {
    /// Open the picker with the given models and current model reference.
    pub fn open(&mut self, models: &[(String, String)], current: Option<&str>) {
        self.visible = true;
        self.filter.clear();
        self.current_ref = current.map(|s| s.to_string());

        // Build and sort entries by display_ref
        let mut entries: Vec<ModelEntry> = models
            .iter()
            .map(|(display_ref, display_name)| ModelEntry {
                display_ref: display_ref.clone(),
                display_name: display_name.clone(),
            })
            .collect();
        entries.sort_by(|a, b| a.display_ref.cmp(&b.display_ref));

        self.all_models = entries;
        self.apply_filter();
    }

    /// Close the picker and reset state.
    pub fn close(&mut self) {
        self.visible = false;
        self.filter.clear();
        self.all_models.clear();
        self.filtered.clear();
        self.selected = 0;
        self.current_ref = None;
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
            .all_models
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                if needle.is_empty() {
                    return true;
                }
                entry.display_ref.to_lowercase().contains(&needle)
                    || entry.display_name.to_lowercase().contains(&needle)
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

    /// Get the display_ref of the currently selected model, if any.
    pub fn selected_ref(&self) -> Option<&str> {
        self.filtered
            .get(self.selected)
            .and_then(|&idx| self.all_models.get(idx))
            .map(|entry| entry.display_ref.as_str())
    }

    /// Get the current filter text.
    pub fn filter_text(&self) -> &str {
        &self.filter
    }

    /// Get the list of filtered model entries as (display_ref, display_name, is_current).
    pub fn filtered_models(&self) -> Vec<(&str, &str, bool)> {
        self.filtered
            .iter()
            .map(|&idx| {
                let entry = &self.all_models[idx];
                let is_current = self
                    .current_ref
                    .as_ref()
                    .is_some_and(|c| c == &entry.display_ref);
                (
                    entry.display_ref.as_str(),
                    entry.display_name.as_str(),
                    is_current,
                )
            })
            .collect()
    }
}

/// Maximum number of visible items in the picker list.
const MAX_VISIBLE_ITEMS: u16 = 10;

/// Render the model picker overlay centered in the message area.
pub fn render_model_picker(
    frame: &mut Frame,
    message_area: Rect,
    state: &ModelPickerState,
    theme: &Theme,
    context_pct: u8,
) {
    if !state.visible {
        return;
    }

    let models = state.filtered_models();

    // Calculate popup dimensions
    // Height: 2 (border) + 1 (filter line) + 1 (separator) + items
    let item_count = (models.len() as u16).min(MAX_VISIBLE_ITEMS);
    let inner_height = 2 + item_count; // filter + separator + items
    let popup_height = (inner_height + 2).min(message_area.height.saturating_sub(2)); // +2 for borders
    let popup_width = 60u16.min(message_area.width.saturating_sub(4));

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
    // Compute right-side key hints to fit within the border
    let title_left = " Switch Model ";
    let hints_text = " ↑↓ navigate · Enter select · Esc close ";
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(Line::from(vec![
            Span::styled(title_left, Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)),
        ]))
        .title_bottom(Line::from(vec![
            Span::styled(hints_text, Style::default().fg(theme.dim)),
        ]));

    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    // Filter input line: "> filter▌"
    let filter_display = format!("> {}\u{258c}", state.filter_text());
    let filter_line = Line::from(Span::styled(
        filter_display,
        Style::default().fg(theme.fg),
    ));
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

    // Model list
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

    let items: Vec<ListItem> = models
        .iter()
        .enumerate()
        .skip(scroll_offset as usize)
        .take(list_height as usize)
        .map(|(i, (display_ref, display_name, is_current))| {
            let is_selected = i == state.selected;
            let marker = if *is_current { "\u{25cf} " } else { "  " };

            let ref_style = if is_selected {
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.fg)
            };

            let name_style = if is_selected {
                Style::default().fg(theme.accent)
            } else {
                Style::default().fg(theme.dim)
            };

            let marker_style = if *is_current {
                Style::default().fg(theme.accent)
            } else {
                Style::default().fg(theme.fg)
            };

            ListItem::new(Line::from(vec![
                Span::styled(marker, marker_style),
                Span::styled(*display_ref, ref_style),
                Span::styled(" \u{2014} ", Style::default().fg(theme.dim)),
                Span::styled(*display_name, name_style),
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

    fn sample_models() -> Vec<(String, String)> {
        vec![
            ("openai/gpt-4o".into(), "GPT-4o".into()),
            ("openai/gpt-4o-mini".into(), "GPT-4o Mini".into()),
            ("anthropic/claude-3-opus".into(), "Claude 3 Opus".into()),
            ("anthropic/claude-3-sonnet".into(), "Claude 3 Sonnet".into()),
        ]
    }

    // ─── State tests ───

    #[test]
    fn open_sets_visible_and_sorts() {
        let mut state = ModelPickerState::default();
        state.open(&sample_models(), Some("openai/gpt-4o"));

        assert!(state.visible);
        assert_eq!(state.all_models.len(), 4);
        // Should be sorted by display_ref
        assert_eq!(state.all_models[0].display_ref, "anthropic/claude-3-opus");
        assert_eq!(state.all_models[1].display_ref, "anthropic/claude-3-sonnet");
        assert_eq!(state.all_models[2].display_ref, "openai/gpt-4o");
        assert_eq!(state.all_models[3].display_ref, "openai/gpt-4o-mini");
    }

    #[test]
    fn close_resets_all_state() {
        let mut state = ModelPickerState::default();
        state.open(&sample_models(), None);
        assert!(state.visible);

        state.close();
        assert!(!state.visible);
        assert!(state.all_models.is_empty());
        assert!(state.filtered.is_empty());
        assert_eq!(state.selected, 0);
        assert!(state.current_ref.is_none());
    }

    #[test]
    fn empty_filter_shows_all() {
        let mut state = ModelPickerState::default();
        state.open(&sample_models(), None);
        assert_eq!(state.filtered.len(), 4);
    }

    #[test]
    fn filter_narrows_results() {
        let mut state = ModelPickerState::default();
        state.open(&sample_models(), None);

        state.type_char('g');
        state.type_char('p');
        state.type_char('t');

        // Should match only openai/gpt-* models
        assert_eq!(state.filtered.len(), 2);
        assert!(state.selected_ref().unwrap().contains("gpt"));
    }

    #[test]
    fn filter_case_insensitive() {
        let mut state = ModelPickerState::default();
        state.open(&sample_models(), None);

        state.type_char('C');
        state.type_char('L');
        state.type_char('A');
        state.type_char('U');
        state.type_char('D');
        state.type_char('E');

        // "CLAUDE" should match anthropic/claude-* by name
        assert_eq!(state.filtered.len(), 2);
    }

    #[test]
    fn filter_matches_display_name() {
        let mut state = ModelPickerState::default();
        state.open(&sample_models(), None);

        // Type "Mini" which only appears in the display_name "GPT-4o Mini"
        for c in "Mini".chars() {
            state.type_char(c);
        }

        assert_eq!(state.filtered.len(), 1);
        assert_eq!(state.selected_ref(), Some("openai/gpt-4o-mini"));
    }

    #[test]
    fn backspace_expands_results() {
        let mut state = ModelPickerState::default();
        state.open(&sample_models(), None);

        for c in "gpt".chars() {
            state.type_char(c);
        }
        assert_eq!(state.filtered.len(), 2);

        state.backspace();
        state.backspace();
        state.backspace();
        assert_eq!(state.filtered.len(), 4);
    }

    #[test]
    fn navigation_wraps_forward() {
        let mut state = ModelPickerState::default();
        state.open(&sample_models(), None);

        for _ in 0..4 {
            state.next();
        }
        assert_eq!(state.selected, 0); // Wrapped back to start
    }

    #[test]
    fn navigation_wraps_backward() {
        let mut state = ModelPickerState::default();
        state.open(&sample_models(), None);

        state.prev();
        assert_eq!(state.selected, 3); // Wrapped to last
    }

    #[test]
    fn selected_ref_correctness() {
        let mut state = ModelPickerState::default();
        state.open(&sample_models(), None);

        // First item (sorted): anthropic/claude-3-opus
        assert_eq!(state.selected_ref(), Some("anthropic/claude-3-opus"));

        state.next();
        assert_eq!(state.selected_ref(), Some("anthropic/claude-3-sonnet"));
    }

    #[test]
    fn selected_ref_none_when_no_matches() {
        let mut state = ModelPickerState::default();
        state.open(&sample_models(), None);

        for c in "zzzzz".chars() {
            state.type_char(c);
        }

        assert!(state.filtered.is_empty());
        assert_eq!(state.selected_ref(), None);
    }

    #[test]
    fn filtered_models_marks_current() {
        let mut state = ModelPickerState::default();
        state.open(&sample_models(), Some("openai/gpt-4o"));

        let models = state.filtered_models();
        let current_entry = models.iter().find(|(r, _, _)| *r == "openai/gpt-4o");
        assert!(current_entry.is_some());
        assert!(current_entry.unwrap().2); // is_current = true

        let non_current = models.iter().find(|(r, _, _)| *r == "anthropic/claude-3-opus");
        assert!(non_current.is_some());
        assert!(!non_current.unwrap().2); // is_current = false
    }

    #[test]
    fn filter_clamps_selected_index() {
        let mut state = ModelPickerState::default();
        state.open(&sample_models(), None);

        // Select the last item
        state.selected = 3;

        // Filter to only 2 results
        for c in "gpt".chars() {
            state.type_char(c);
        }
        assert!(state.selected < state.filtered.len());
    }

    #[test]
    fn open_with_no_models() {
        let mut state = ModelPickerState::default();
        state.open(&[], None);

        assert!(state.visible);
        assert!(state.all_models.is_empty());
        assert!(state.filtered.is_empty());
        assert_eq!(state.selected_ref(), None);
    }

    #[test]
    fn filter_text_returns_current_filter() {
        let mut state = ModelPickerState::default();
        state.open(&sample_models(), None);

        state.type_char('a');
        state.type_char('b');
        assert_eq!(state.filter_text(), "ab");

        state.backspace();
        assert_eq!(state.filter_text(), "a");
    }

    // ─── Render tests ───

    use ratatui::layout::Rect;

    /// Helper: render model picker and return buffer text.
    fn render_picker_to_string(
        width: u16,
        height: u16,
        state: &ModelPickerState,
        message_area: Rect,
    ) -> String {
        let theme = Theme::default();
        let buf = super::super::render_to_buffer(width, height, |frame| {
            render_model_picker(frame, message_area, state, &theme, 0);
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
        let state = ModelPickerState::default();
        let area = Rect::new(0, 0, 80, 20);
        let text = render_picker_to_string(80, 20, &state, area);
        let non_space: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(
            non_space.is_empty(),
            "hidden picker should render nothing, got: '{non_space}'"
        );
    }

    #[test]
    fn render_shows_model_names() {
        let mut state = ModelPickerState::default();
        state.open(&sample_models(), None);
        let area = Rect::new(0, 0, 80, 24);
        let text = render_picker_to_string(80, 24, &state, area);

        assert!(
            text.contains("openai/gpt-4o"),
            "should show model ref, got:\n{text}"
        );
        assert!(
            text.contains("GPT-4o"),
            "should show model name, got:\n{text}"
        );
    }

    #[test]
    fn render_shows_title() {
        let mut state = ModelPickerState::default();
        state.open(&sample_models(), None);
        let area = Rect::new(0, 0, 80, 24);
        let text = render_picker_to_string(80, 24, &state, area);

        assert!(
            text.contains("Switch Model"),
            "should show title, got:\n{text}"
        );
    }

    #[test]
    fn render_highlights_selected_with_accent() {
        let mut state = ModelPickerState::default();
        state.open(&sample_models(), None);
        let area = Rect::new(0, 0, 80, 24);
        let theme = Theme::default();
        let buf = super::super::render_to_buffer(80, 24, |frame| {
            render_model_picker(frame, area, &state, &theme, 0);
        });

        // The selected item (first, sorted = "anthropic/claude-3-opus") should have accent color
        let mut found_accent = false;
        for y in 0..24 {
            for x in 0..80 {
                let cell = &buf[(x, y)];
                if cell.symbol() == "a" && cell.fg == theme.accent {
                    // "a" from "anthropic/..."
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
        let mut state = ModelPickerState::default();
        state.open(&sample_models(), Some("openai/gpt-4o"));
        let area = Rect::new(0, 0, 80, 24);
        let text = render_picker_to_string(80, 24, &state, area);

        assert!(
            text.contains("\u{25cf}"),
            "should show ● marker for current model, got:\n{text}"
        );
    }

    #[test]
    fn render_shows_filter_text() {
        let mut state = ModelPickerState::default();
        state.open(&sample_models(), None);
        state.type_char('g');
        state.type_char('p');
        let area = Rect::new(0, 0, 80, 24);
        let text = render_picker_to_string(80, 24, &state, area);

        assert!(
            text.contains("> gp"),
            "should show filter text, got:\n{text}"
        );
    }

    #[test]
    fn render_centered_in_area() {
        let mut state = ModelPickerState::default();
        state.open(&sample_models(), None);
        let area = Rect::new(0, 0, 100, 30);
        let theme = Theme::default();
        let buf = super::super::render_to_buffer(100, 30, |frame| {
            render_model_picker(frame, area, &state, &theme, 0);
        });

        // Check that column 0 is empty (popup should be centered, not left-aligned)
        let col0_content: String = (0..30)
            .map(|y| buf[(0u16, y)].symbol().to_string())
            .collect::<Vec<_>>()
            .join("");
        let col0_trimmed: String = col0_content.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(
            col0_trimmed.is_empty(),
            "column 0 should be empty (popup centered), got: '{col0_trimmed}'"
        );
    }
}
