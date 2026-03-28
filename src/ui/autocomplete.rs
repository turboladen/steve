//! Command and file reference autocomplete popup state and rendering.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem},
};

use super::theme::Theme;
use crate::command::Command;

/// Which kind of autocomplete is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutocompleteMode {
    Command,
    FileRef,
}

/// State for the autocomplete popup (commands and file references).
pub struct AutocompleteState {
    /// Whether the popup is currently visible.
    pub visible: bool,
    /// Current mode.
    pub mode: AutocompleteMode,
    /// Matching command names and descriptions (Command mode).
    matches: Vec<(&'static str, &'static str)>,
    /// Matching file paths (FileRef mode).
    file_matches: Vec<String>,
    /// Currently selected index.
    pub selected: usize,
}

impl Default for AutocompleteState {
    fn default() -> Self {
        Self {
            visible: false,
            mode: AutocompleteMode::Command,
            matches: vec![],
            file_matches: vec![],
            selected: 0,
        }
    }
}

impl AutocompleteState {
    /// Update matches based on current input prefix (command mode only).
    ///
    /// Supports multi-word commands (e.g. `/mcp tools`) by matching the full
    /// input prefix against command names.  The menu stays visible as long as
    /// at least one command name starts with the typed text.
    pub fn update(&mut self, input: &str) {
        if input.starts_with('/') {
            self.mode = AutocompleteMode::Command;
            self.matches = Command::matching_commands(input)
                .into_iter()
                .map(|c| (c.name, c.description))
                .collect();
            self.file_matches.clear();
            self.visible = !self.matches.is_empty();
            if self.selected >= self.matches.len() {
                self.selected = 0;
            }
        } else {
            self.hide();
        }
    }

    /// Update matches considering both commands and file references.
    /// Falls through to command autocomplete if no active `@` reference is found.
    pub fn update_with_files(&mut self, input: &str, file_index: &[String]) {
        // Check if we're typing a file reference at the cursor position.
        // Find the last `@` token that might be an active reference.
        if let Some(active) = find_active_file_ref(input) {
            let prefix = &active;
            self.mode = AutocompleteMode::FileRef;
            self.matches.clear();
            self.file_matches = file_index
                .iter()
                .filter(|f| {
                    // Match by prefix or substring on the filename
                    f.starts_with(prefix.as_str())
                        || f.contains(prefix.as_str())
                        || f.rsplit('/')
                            .next()
                            .is_some_and(|name| name.starts_with(prefix.as_str()))
                })
                .take(20) // cap results
                .cloned()
                .collect();
            self.visible = !self.file_matches.is_empty();
            if self.selected >= self.file_matches.len() {
                self.selected = 0;
            }
        } else {
            // Fall through to command autocomplete
            self.update(input);
        }
    }

    /// Hide the popup.
    pub fn hide(&mut self) {
        self.visible = false;
        self.matches.clear();
        self.file_matches.clear();
        self.selected = 0;
    }

    /// Move selection down (wraps).
    pub fn next(&mut self) {
        let len = self.total_matches();
        if len > 0 {
            self.selected = (self.selected + 1) % len;
        }
    }

    /// Move selection up (wraps).
    pub fn prev(&mut self) {
        let len = self.total_matches();
        if len > 0 {
            self.selected = if self.selected == 0 {
                len - 1
            } else {
                self.selected - 1
            };
        }
    }

    /// Get the selected command name (Command mode).
    pub fn selected_command(&self) -> Option<&str> {
        if self.mode == AutocompleteMode::Command {
            self.matches.get(self.selected).map(|(name, _)| *name)
        } else {
            None
        }
    }

    /// Get the selected file path (FileRef mode).
    pub fn selected_file(&self) -> Option<&str> {
        if self.mode == AutocompleteMode::FileRef {
            self.file_matches.get(self.selected).map(|s| s.as_str())
        } else {
            None
        }
    }

    fn total_matches(&self) -> usize {
        match self.mode {
            AutocompleteMode::Command => self.matches.len(),
            AutocompleteMode::FileRef => self.file_matches.len(),
        }
    }
}

/// Find the active `@` file reference being typed at the end of the input.
/// Returns the path portion after `@` or `@!` if one is in progress.
fn find_active_file_ref(input: &str) -> Option<String> {
    // Walk backwards from the end to find the last `@` that could be a file ref
    let bytes = input.as_bytes();
    let mut i = bytes.len();

    // Skip trailing whitespace — if there's whitespace after the @token, it's complete
    // We only want to trigger when the cursor is still in the token
    if i > 0 && bytes[i - 1].is_ascii_whitespace() {
        return None;
    }

    // Find the last @ sign
    while i > 0 {
        i -= 1;
        if bytes[i] == b'@' {
            // Skip if preceded by alphanumeric
            if i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_') {
                return None;
            }

            let mut path_start = i + 1;
            // Skip '!' for inject mode
            if path_start < bytes.len() && bytes[path_start] == b'!' {
                path_start += 1;
            }

            if path_start >= bytes.len() {
                // Just "@" with nothing after — not enough for matching
                return None;
            }

            // Skip if starts with digit
            if bytes[path_start].is_ascii_digit() {
                return None;
            }

            let path = &input[path_start..];
            // Must have at least 1 char
            if !path.is_empty() {
                return Some(path.to_string());
            }
            return None;
        }
        // If we hit whitespace while scanning back, the @ is in an earlier token
        if bytes[i].is_ascii_whitespace() {
            return None;
        }
    }
    None
}

/// Apply file ref autocomplete: replace the `@prefix` at the end of input with the selected path.
pub fn apply_file_completion(input: &str, selected_path: &str) -> String {
    let bytes = input.as_bytes();
    let mut i = bytes.len();

    // Find the start of the current @-token
    while i > 0 {
        i -= 1;
        if bytes[i] == b'@' {
            // Keep @ (and @! if present)
            let inject = i + 1 < bytes.len() && bytes[i + 1] == b'!';
            let prefix = if inject { "@!" } else { "@" };
            let before = &input[..i];
            return format!("{before}{prefix}{selected_path}");
        }
        if bytes[i].is_ascii_whitespace() {
            break;
        }
    }
    // Fallback: shouldn't happen, but return input unchanged
    input.to_string()
}

/// Render the autocomplete popup as an overlay above the input area.
pub fn render_autocomplete(
    frame: &mut Frame,
    input_area: Rect,
    state: &AutocompleteState,
    theme: &Theme,
    context_pct: u8,
) {
    if !state.visible {
        return;
    }

    match state.mode {
        AutocompleteMode::Command => {
            render_command_popup(frame, input_area, state, theme, context_pct)
        }
        AutocompleteMode::FileRef => {
            render_file_popup(frame, input_area, state, theme, context_pct)
        }
    }
}

fn render_command_popup(
    frame: &mut Frame,
    input_area: Rect,
    state: &AutocompleteState,
    theme: &Theme,
    context_pct: u8,
) {
    if state.matches.is_empty() {
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

    let items: Vec<ListItem> = state
        .matches
        .iter()
        .enumerate()
        .map(|(i, (name, desc))| {
            let style = if i == state.selected {
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.fg)
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!("{:<12}", name), style),
                Span::styled(*desc, Style::default().fg(theme.dim)),
            ]))
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.border_color(context_pct))),
    );

    frame.render_widget(list, popup_area);
}

fn render_file_popup(
    frame: &mut Frame,
    input_area: Rect,
    state: &AutocompleteState,
    theme: &Theme,
    context_pct: u8,
) {
    if state.file_matches.is_empty() {
        return;
    }

    let item_count = state.file_matches.len().min(10) as u16;
    let popup_height = item_count + 2; // +2 for borders
    // Dynamic width: sized to longest visible match, min 40, capped by available terminal width.
    let longest_match = state
        .file_matches
        .iter()
        .take(10) // only visible items
        .map(|p| p.len())
        .max()
        .unwrap_or(0);
    let content_width = (longest_match + 2) as u16; // +2 for border padding
    let max_width = input_area.width.saturating_sub(2); // leave room for x offset
    let popup_width = content_width.clamp(40, max_width);

    let popup_area = Rect {
        x: input_area.x + 2,
        y: input_area.y.saturating_sub(popup_height),
        width: popup_width,
        height: popup_height,
    };

    frame.render_widget(Clear, popup_area);

    let items: Vec<ListItem> = state
        .file_matches
        .iter()
        .enumerate()
        .map(|(i, path)| {
            let style = if i == state.selected {
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.fg)
            };
            ListItem::new(Line::from(Span::styled(path.as_str(), style)))
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.border_color(context_pct))),
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
    fn update_hides_when_no_command_matches() {
        let mut state = AutocompleteState::default();
        // No command starts with "/model something", so menu hides.
        state.update("/model something");
        assert!(!state.visible);
    }

    #[test]
    fn update_matches_multiword_commands() {
        let mut state = AutocompleteState::default();
        // "/mcp " should still show the multi-word subcommands.
        state.update("/mcp ");
        assert!(state.visible);
        assert!(state.matches.iter().any(|(name, _)| *name == "/mcp tools"));
        assert!(
            state
                .matches
                .iter()
                .any(|(name, _)| *name == "/mcp resources")
        );
        assert!(
            state
                .matches
                .iter()
                .any(|(name, _)| *name == "/mcp prompts")
        );
    }

    #[test]
    fn update_narrows_multiword_prefix() {
        let mut state = AutocompleteState::default();
        state.update("/mcp t");
        assert!(state.visible);
        assert_eq!(state.matches.len(), 1);
        assert_eq!(state.matches[0].0, "/mcp tools");
    }

    #[test]
    fn update_hides_after_full_multiword_plus_arg() {
        let mut state = AutocompleteState::default();
        // After completing "/mcp tools" and typing an arg, no command matches.
        state.update("/mcp tools my-server");
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
        state.update("/exi");
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

    // ─── FileRef mode ───

    #[test]
    fn file_ref_mode_basic() {
        let files = vec![
            "src/main.rs".into(),
            "src/lib.rs".into(),
            "Cargo.toml".into(),
        ];
        let mut state = AutocompleteState::default();
        state.update_with_files("explain @src/ma", &files);
        assert!(state.visible);
        assert_eq!(state.mode, AutocompleteMode::FileRef);
        assert!(state.file_matches.contains(&"src/main.rs".to_string()));
    }

    #[test]
    fn file_ref_inject_mode() {
        let files = vec!["src/main.rs".into(), "src/lib.rs".into()];
        let mut state = AutocompleteState::default();
        state.update_with_files("show @!src/li", &files);
        assert!(state.visible);
        assert_eq!(state.mode, AutocompleteMode::FileRef);
        assert!(state.file_matches.contains(&"src/lib.rs".to_string()));
    }

    #[test]
    fn file_ref_falls_through_to_command() {
        let files = vec!["src/main.rs".into()];
        let mut state = AutocompleteState::default();
        state.update_with_files("/mod", &files);
        assert!(state.visible);
        assert_eq!(state.mode, AutocompleteMode::Command);
    }

    #[test]
    fn file_ref_no_match_hides() {
        let files = vec!["src/main.rs".into()];
        let mut state = AutocompleteState::default();
        state.update_with_files("tell me about @zzz", &files);
        assert!(!state.visible);
    }

    #[test]
    fn file_ref_basename_match() {
        let files = vec!["src/tool/read.rs".into(), "src/tool/write.rs".into()];
        let mut state = AutocompleteState::default();
        state.update_with_files("look at @read", &files);
        assert!(state.visible);
        assert!(state.file_matches.contains(&"src/tool/read.rs".to_string()));
    }

    #[test]
    fn selected_file_returns_path() {
        let files = vec!["src/main.rs".into(), "src/lib.rs".into()];
        let mut state = AutocompleteState::default();
        state.update_with_files("@src/m", &files);
        assert_eq!(state.selected_file(), Some("src/main.rs"));
    }

    #[test]
    fn selected_file_none_in_command_mode() {
        let mut state = AutocompleteState::default();
        state.update("/m");
        assert_eq!(state.selected_file(), None);
    }

    // ─── find_active_file_ref ───

    #[test]
    fn find_active_ref_basic() {
        assert_eq!(
            find_active_file_ref("look at @src/ma"),
            Some("src/ma".into())
        );
    }

    #[test]
    fn find_active_ref_inject() {
        assert_eq!(find_active_file_ref("show @!lib"), Some("lib".into()));
    }

    #[test]
    fn find_active_ref_trailing_space_none() {
        assert_eq!(find_active_file_ref("look at @src/main.rs "), None);
    }

    #[test]
    fn find_active_ref_email_none() {
        assert_eq!(find_active_file_ref("user@host"), None);
    }

    #[test]
    fn find_active_ref_no_at_none() {
        assert_eq!(find_active_file_ref("just text"), None);
    }

    #[test]
    fn find_active_ref_bare_at_none() {
        assert_eq!(find_active_file_ref("text @"), None);
    }

    // ─── apply_file_completion ───

    #[test]
    fn apply_completion_basic() {
        let result = apply_file_completion("look at @src/ma", "src/main.rs");
        assert_eq!(result, "look at @src/main.rs");
    }

    #[test]
    fn apply_completion_inject() {
        let result = apply_file_completion("show @!li", "src/lib.rs");
        assert_eq!(result, "show @!src/lib.rs");
    }

    #[test]
    fn apply_completion_at_start() {
        let result = apply_file_completion("@Car", "Cargo.toml");
        assert_eq!(result, "@Cargo.toml");
    }

    // -- Buffer rendering tests --

    use ratatui::layout::Rect;

    /// Helper: render autocomplete popup and return buffer text.
    fn render_autocomplete_to_string(
        width: u16,
        height: u16,
        state: &AutocompleteState,
        input_area: Rect,
    ) -> String {
        let theme = Theme::default();
        let buf = super::super::render_to_buffer(width, height, |frame| {
            render_autocomplete(frame, input_area, state, &theme, 0);
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
    fn buffer_hidden_renders_nothing() {
        let state = AutocompleteState::default();
        let input_area = Rect::new(0, 20, 80, 5);
        let text = render_autocomplete_to_string(80, 25, &state, input_area);
        // All cells should be spaces (default empty buffer)
        let non_space: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(
            non_space.is_empty(),
            "hidden autocomplete should render nothing, got: '{non_space}'"
        );
    }

    #[test]
    fn buffer_shows_filtered_matches() {
        let mut state = AutocompleteState::default();
        state.update("/e"); // Should match "/exit"
        let input_area = Rect::new(0, 20, 80, 5);
        let text = render_autocomplete_to_string(80, 25, &state, input_area);
        assert!(
            text.contains("/exit"),
            "should show /exit match, got:\n{text}"
        );
    }

    #[test]
    fn buffer_highlights_selected_item() {
        let mut state = AutocompleteState::default();
        state.update("/m"); // Should match /model, /models
        let selected = state.selected_command().unwrap_or("").to_string();
        let input_area = Rect::new(0, 20, 80, 5);
        let theme = Theme::default();
        let buf = super::super::render_to_buffer(80, 25, |frame| {
            render_autocomplete(frame, input_area, &state, &theme, 0);
        });
        // The selected item should be rendered with accent color
        // Find the first character of the selected command
        let mut found_accent = false;
        for y in 0..25 {
            for x in 0..80 {
                let cell = &buf[(x, y)];
                if cell.symbol() == "/" && cell.fg == theme.accent {
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
            "selected item '{}' should be rendered with accent color",
            selected
        );
    }

    #[test]
    fn buffer_positioned_above_input() {
        let mut state = AutocompleteState::default();
        state.update("/m");
        // Input at the bottom of a 30-row terminal
        let input_area = Rect::new(0, 25, 80, 5);
        let buf = super::super::render_to_buffer(80, 30, |frame| {
            let theme = Theme::default();
            render_autocomplete(frame, input_area, &state, &theme, 0);
        });
        // The popup should be above the input area (y < 25)
        // Check that there's content above the input area
        let mut found_content_above = false;
        for y in 0..25 {
            for x in 0..80 {
                let cell = &buf[(x, y)];
                let sym = cell.symbol();
                if sym != " " && !sym.is_empty() {
                    found_content_above = true;
                    break;
                }
            }
            if found_content_above {
                break;
            }
        }
        assert!(
            found_content_above,
            "autocomplete popup should be positioned above the input area"
        );
    }

    #[test]
    fn buffer_file_ref_popup_shows_paths() {
        let files = vec!["src/main.rs".into(), "src/lib.rs".into()];
        let mut state = AutocompleteState::default();
        state.update_with_files("@src/", &files);
        assert!(state.visible);
        let input_area = Rect::new(0, 20, 80, 5);
        let text = render_autocomplete_to_string(80, 25, &state, input_area);
        assert!(
            text.contains("src/main.rs"),
            "file popup should show paths, got:\n{text}"
        );
    }
}
