//! Command autocomplete popup state and rendering.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem},
};

use crate::command::Command;
use super::theme::Theme;

/// State for the command autocomplete popup.
pub struct AutocompleteState {
    /// Whether the popup is currently visible.
    pub visible: bool,
    /// Matching command names and descriptions.
    matches: Vec<(&'static str, &'static str)>,
    /// Currently selected index.
    pub selected: usize,
}

impl Default for AutocompleteState {
    fn default() -> Self {
        Self {
            visible: false,
            matches: vec![],
            selected: 0,
        }
    }
}

impl AutocompleteState {
    /// Update matches based on current input prefix.
    pub fn update(&mut self, input: &str) {
        if input.starts_with('/') && !input.contains(' ') {
            self.matches = Command::matching_commands(input)
                .into_iter()
                .map(|c| (c.name, c.description))
                .collect();
            self.visible = !self.matches.is_empty();
            if self.selected >= self.matches.len() {
                self.selected = 0;
            }
        } else {
            self.hide();
        }
    }

    /// Hide the popup.
    pub fn hide(&mut self) {
        self.visible = false;
        self.matches.clear();
        self.selected = 0;
    }

    /// Move selection down (wraps).
    pub fn next(&mut self) {
        if !self.matches.is_empty() {
            self.selected = (self.selected + 1) % self.matches.len();
        }
    }

    /// Move selection up (wraps).
    pub fn prev(&mut self) {
        if !self.matches.is_empty() {
            self.selected = if self.selected == 0 {
                self.matches.len() - 1
            } else {
                self.selected - 1
            };
        }
    }

    /// Get the selected command name.
    pub fn selected_command(&self) -> Option<&str> {
        self.matches.get(self.selected).map(|(name, _)| *name)
    }
}

/// Render the autocomplete popup as an overlay above the input area.
pub fn render_autocomplete(
    frame: &mut Frame,
    input_area: Rect,
    state: &AutocompleteState,
    theme: &Theme,
) {
    if !state.visible || state.matches.is_empty() {
        return;
    }

    let item_count = state.matches.len().min(8) as u16;
    let popup_height = item_count + 2; // +2 for borders
    let popup_width = 40u16.min(input_area.width);

    // Position above the input area, offset past "> " chevron.
    // input_area.y is the context line; textarea starts at y+1.
    let popup_area = Rect {
        x: input_area.x + 2, // offset past "> " chevron
        y: input_area.y.saturating_sub(popup_height),
        width: popup_width,
        height: popup_height,
    };

    // Clear the area behind the popup
    frame.render_widget(Clear, popup_area);

    let items: Vec<ListItem> = state.matches.iter().enumerate().map(|(i, (name, desc))| {
        let style = if i == state.selected {
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.fg)
        };
        ListItem::new(Line::from(vec![
            Span::styled(format!("{:<12}", name), style),
            Span::styled(*desc, Style::default().fg(theme.dim)),
        ]))
    }).collect();

    let list = List::new(items)
        .block(Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.border))
        );

    frame.render_widget(list, popup_area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_shows_matches() {
        let mut state = AutocompleteState::default();
        state.update("/m");
        assert!(state.visible);
        assert!(state.matches.len() >= 2); // /model, /models
    }

    #[test]
    fn update_hides_on_no_match() {
        let mut state = AutocompleteState::default();
        state.update("/zzz");
        assert!(!state.visible);
    }

    #[test]
    fn update_hides_on_space() {
        let mut state = AutocompleteState::default();
        state.update("/model something");
        assert!(!state.visible);
    }

    #[test]
    fn update_hides_no_slash() {
        let mut state = AutocompleteState::default();
        state.update("hello");
        assert!(!state.visible);
    }

    #[test]
    fn next_wraps_around() {
        let mut state = AutocompleteState::default();
        state.update("/");
        let count = state.matches.len();
        for _ in 0..count {
            state.next();
        }
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn prev_wraps_around() {
        let mut state = AutocompleteState::default();
        state.update("/");
        state.prev();
        assert_eq!(state.selected, state.matches.len() - 1);
    }

    #[test]
    fn selected_command_returns_name() {
        let mut state = AutocompleteState::default();
        state.update("/e");
        assert_eq!(state.selected_command(), Some("/exit"));
    }

    #[test]
    fn hide_resets_state() {
        let mut state = AutocompleteState::default();
        state.update("/m");
        assert!(state.visible);
        state.hide();
        assert!(!state.visible);
        assert!(state.matches.is_empty());
        assert_eq!(state.selected, 0);
    }
}
