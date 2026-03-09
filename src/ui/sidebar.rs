use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph, Wrap},
};

use crate::ui::message_block::{DiffContent, DiffLine};

use super::status_line::format_tokens;
use super::theme::Theme;

/// A file modified by a write tool, with accumulated line-change counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    /// Path relative to the project root.
    pub path: String,
    /// Total lines added across all changes to this file.
    pub additions: usize,
    /// Total lines removed across all changes to this file.
    pub removals: usize,
}

/// State for the sidebar panel.
pub struct SidebarState {
    pub session_title: String,
    pub model_name: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub session_cost: Option<f64>,
    pub todos: Vec<TodoItem>,
    /// Accumulated file changes from write tools this session.
    pub changes: Vec<FileChange>,
}

/// A todo item displayed in the sidebar.
#[derive(Debug, Clone)]
pub struct TodoItem {
    pub text: String,
    pub done: bool,
}

impl Default for SidebarState {
    fn default() -> Self {
        Self {
            session_title: String::new(),
            model_name: String::new(),
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
            session_cost: None,
            todos: Vec::new(),
            changes: Vec::new(),
        }
    }
}

/// Count addition and removal lines from a `DiffContent`.
/// Returns `(additions, removals)`.
pub fn count_diff_lines(diff: &DiffContent) -> (usize, usize) {
    match diff {
        DiffContent::EditDiff { lines } | DiffContent::PatchDiff { lines } => {
            let mut additions = 0;
            let mut removals = 0;
            for line in lines {
                match line {
                    DiffLine::Addition(_) => additions += 1,
                    DiffLine::Removal(_) => removals += 1,
                    DiffLine::Context(_) | DiffLine::HunkHeader(_) => {}
                }
            }
            (additions, removals)
        }
        DiffContent::WriteSummary { line_count } => (*line_count, 0),
    }
}

impl SidebarState {
    /// Record a file change from a write tool. Accumulates into existing entry
    /// if the path was already seen, otherwise appends. Skips zero-change entries.
    pub fn record_file_change(&mut self, path: String, additions: usize, removals: usize) {
        if additions == 0 && removals == 0 {
            return;
        }
        if let Some(existing) = self.changes.iter_mut().find(|c| c.path == path) {
            existing.additions += additions;
            existing.removals += removals;
        } else {
            self.changes.push(FileChange {
                path,
                additions,
                removals,
            });
        }
    }

    /// Sum all additions and removals across all changed files.
    pub fn total_changes(&self) -> (usize, usize) {
        self.changes
            .iter()
            .fold((0, 0), |(a, r), c| (a + c.additions, r + c.removals))
    }
}

/// Render the sidebar into the given area.
pub fn render_sidebar(
    frame: &mut Frame,
    area: Rect,
    state: &SidebarState,
    theme: &Theme,
) {
    let mut lines: Vec<Line> = Vec::new();

    // -- Changes section (if any files were modified) --
    if !state.changes.is_empty() {
        lines.push(Line::from(Span::styled(
            "Changes",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )));
        for change in &state.changes {
            let mut spans = vec![Span::styled(
                format!("  {}", change.path),
                Style::default().fg(theme.fg),
            )];
            if change.additions > 0 {
                spans.push(Span::styled(
                    format!(" +{}", change.additions),
                    Style::default().fg(theme.success),
                ));
            }
            if change.removals > 0 {
                spans.push(Span::styled(
                    format!(" -{}", change.removals),
                    Style::default().fg(theme.error),
                ));
            }
            lines.push(Line::from(spans));
        }
        // Summary line
        let (total_add, total_rem) = state.total_changes();
        let file_word = if state.changes.len() == 1 { "file" } else { "files" };
        let mut summary_spans: Vec<Span> = vec![Span::styled(
            format!("  {} {file_word}", state.changes.len()),
            Style::default().fg(theme.dim),
        )];
        if total_add > 0 {
            summary_spans.push(Span::styled(
                format!(" +{total_add}"),
                Style::default().fg(theme.success),
            ));
        }
        if total_rem > 0 {
            summary_spans.push(Span::styled(
                format!(" -{total_rem}"),
                Style::default().fg(theme.error),
            ));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(summary_spans));
        lines.push(Line::from(""));
    }

    // -- Session section --
    lines.push(Line::from(Span::styled(
        "Session",
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
    )));
    let title = if state.session_title.is_empty() {
        "(untitled)"
    } else {
        &state.session_title
    };
    lines.push(Line::from(Span::styled(
        format!("  {title}"),
        Style::default().fg(theme.fg),
    )));
    let model = if state.model_name.is_empty() {
        "(none)"
    } else {
        &state.model_name
    };
    lines.push(Line::from(Span::styled(
        format!("  {model}"),
        Style::default().fg(theme.fg),
    )));
    // Cumulative token usage (complementary to input bar's per-call context pressure)
    lines.push(Line::from(Span::styled(
        format!(
            "  in: {}  out: {}",
            format_tokens(state.prompt_tokens),
            format_tokens(state.completion_tokens),
        ),
        Style::default().fg(theme.dim),
    )));
    lines.push(Line::from(Span::styled(
        format!("  total: {}", format_tokens(state.total_tokens)),
        Style::default().fg(theme.dim),
    )));
    match state.session_cost {
        Some(cost) => {
            lines.push(Line::from(Span::styled(
                format!("  cost: ${:.4}", cost),
                Style::default().fg(theme.dim),
            )));
        }
        None => {
            lines.push(Line::from(Span::styled(
                "  cost: N/A",
                Style::default().fg(theme.dim),
            )));
        }
    }
    lines.push(Line::from(""));

    // -- Todos section (if any) --
    if !state.todos.is_empty() {
        lines.push(Line::from(Span::styled(
            "Todos",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )));
        for todo in &state.todos {
            let marker = if todo.done { "✓" } else { "○" };
            let style = if todo.done {
                Style::default().fg(theme.dim)
            } else {
                Style::default().fg(theme.fg)
            };
            lines.push(Line::from(Span::styled(
                format!("  {marker} {}", todo.text),
                style,
            )));
        }
    }

    let block = Block::default();

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: true });

    frame.render_widget(paragraph, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- count_diff_lines tests --

    #[test]
    fn count_diff_lines_edit_diff() {
        let diff = DiffContent::EditDiff {
            lines: vec![
                DiffLine::Removal("old1".into()),
                DiffLine::Removal("old2".into()),
                DiffLine::Addition("new1".into()),
                DiffLine::Addition("new2".into()),
                DiffLine::Addition("new3".into()),
            ],
        };
        assert_eq!(count_diff_lines(&diff), (3, 2));
    }

    #[test]
    fn count_diff_lines_write_summary() {
        let diff = DiffContent::WriteSummary { line_count: 42 };
        assert_eq!(count_diff_lines(&diff), (42, 0));
    }

    #[test]
    fn count_diff_lines_write_summary_zero() {
        let diff = DiffContent::WriteSummary { line_count: 0 };
        assert_eq!(count_diff_lines(&diff), (0, 0));
    }

    #[test]
    fn count_diff_lines_patch_diff() {
        let diff = DiffContent::PatchDiff {
            lines: vec![
                DiffLine::HunkHeader("@@ -1,3 +1,4 @@".into()),
                DiffLine::Context("unchanged".into()),
                DiffLine::Removal("removed".into()),
                DiffLine::Addition("added1".into()),
                DiffLine::Addition("added2".into()),
                DiffLine::Context("also unchanged".into()),
            ],
        };
        assert_eq!(count_diff_lines(&diff), (2, 1));
    }

    #[test]
    fn count_diff_lines_empty_edit_diff() {
        let diff = DiffContent::EditDiff { lines: vec![] };
        assert_eq!(count_diff_lines(&diff), (0, 0));
    }

    #[test]
    fn count_diff_lines_context_only() {
        let diff = DiffContent::PatchDiff {
            lines: vec![
                DiffLine::HunkHeader("@@ -1 +1 @@".into()),
                DiffLine::Context("line1".into()),
                DiffLine::Context("line2".into()),
            ],
        };
        assert_eq!(count_diff_lines(&diff), (0, 0));
    }

    // -- record_file_change tests --

    #[test]
    fn record_file_change_new_file() {
        let mut state = SidebarState::default();
        state.record_file_change("src/main.rs".into(), 3, 1);
        assert_eq!(state.changes.len(), 1);
        assert_eq!(
            state.changes[0],
            FileChange {
                path: "src/main.rs".into(),
                additions: 3,
                removals: 1,
            }
        );
    }

    #[test]
    fn record_file_change_accumulates() {
        let mut state = SidebarState::default();
        state.record_file_change("src/main.rs".into(), 3, 1);
        state.record_file_change("src/main.rs".into(), 5, 2);
        assert_eq!(state.changes.len(), 1);
        assert_eq!(state.changes[0].additions, 8);
        assert_eq!(state.changes[0].removals, 3);
    }

    #[test]
    fn record_file_change_skips_zero() {
        let mut state = SidebarState::default();
        state.record_file_change("src/main.rs".into(), 0, 0);
        assert!(state.changes.is_empty());
    }

    #[test]
    fn record_file_change_preserves_order() {
        let mut state = SidebarState::default();
        state.record_file_change("b.rs".into(), 1, 0);
        state.record_file_change("a.rs".into(), 2, 0);
        state.record_file_change("c.rs".into(), 3, 0);
        assert_eq!(state.changes[0].path, "b.rs");
        assert_eq!(state.changes[1].path, "a.rs");
        assert_eq!(state.changes[2].path, "c.rs");
    }

    // -- total_changes tests --

    #[test]
    fn total_changes_empty() {
        let state = SidebarState::default();
        assert_eq!(state.total_changes(), (0, 0));
    }

    #[test]
    fn total_changes_populated() {
        let mut state = SidebarState::default();
        state.record_file_change("a.rs".into(), 3, 1);
        state.record_file_change("b.rs".into(), 12, 0);
        state.record_file_change("c.rs".into(), 1, 0);
        assert_eq!(state.total_changes(), (16, 1));
    }

    // -- FileChange equality --

    #[test]
    fn file_change_equality() {
        let a = FileChange {
            path: "src/main.rs".into(),
            additions: 3,
            removals: 1,
        };
        let b = FileChange {
            path: "src/main.rs".into(),
            additions: 3,
            removals: 1,
        };
        let c = FileChange {
            path: "src/lib.rs".into(),
            additions: 3,
            removals: 1,
        };
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    // -- SidebarState::default() --

    #[test]
    fn sidebar_state_default_has_empty_changes() {
        let state = SidebarState::default();
        assert!(state.changes.is_empty());
    }

    // -- Buffer rendering tests --

    use ratatui::layout::Rect;

    /// Helper: render sidebar into a buffer and return the text as a single string.
    fn render_sidebar_to_string(width: u16, height: u16, state: &SidebarState) -> String {
        let theme = Theme::default();
        let buf = super::super::render_to_buffer(width, height, |frame| {
            render_sidebar(frame, Rect::new(0, 0, width, height), state, &theme);
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
    fn buffer_sidebar_session_section_shows_model() {
        let state = SidebarState {
            model_name: "gpt-4o".to_string(),
            ..Default::default()
        };
        let text = render_sidebar_to_string(40, 20, &state);
        assert!(text.contains("Session"), "should show 'Session' header");
        assert!(text.contains("gpt-4o"), "should show model name");
    }

    #[test]
    fn buffer_sidebar_token_display() {
        let state = SidebarState {
            prompt_tokens: 12800,
            completion_tokens: 3200,
            total_tokens: 16000,
            ..Default::default()
        };
        let text = render_sidebar_to_string(40, 20, &state);
        assert!(text.contains("in: 12.8k"), "should show formatted prompt tokens");
        assert!(text.contains("out: 3.2k"), "should show formatted completion tokens");
        assert!(text.contains("total: 16.0k"), "should show formatted total tokens");
    }

    #[test]
    fn buffer_sidebar_cost_display() {
        let state = SidebarState {
            session_cost: Some(0.0512),
            ..Default::default()
        };
        let text = render_sidebar_to_string(40, 20, &state);
        assert!(text.contains("cost: $0.0512"), "should show cost");
    }

    #[test]
    fn buffer_sidebar_cost_na_when_not_configured() {
        let state = SidebarState {
            session_cost: None,
            ..Default::default()
        };
        let text = render_sidebar_to_string(40, 20, &state);
        assert!(text.contains("cost: N/A"), "should show N/A when cost not configured");
    }

    #[test]
    fn buffer_sidebar_changes_section() {
        let mut state = SidebarState::default();
        state.record_file_change("src/main.rs".into(), 10, 3);
        let text = render_sidebar_to_string(40, 20, &state);
        assert!(text.contains("Changes"), "should show 'Changes' header");
        assert!(text.contains("src/main.rs"), "should show file path");
        assert!(text.contains("+10"), "should show additions in green");
        assert!(text.contains("-3"), "should show removals in red");
    }

    #[test]
    fn buffer_sidebar_todos_section() {
        let state = SidebarState {
            todos: vec![
                TodoItem { text: "Fix bug".to_string(), done: false },
                TodoItem { text: "Write tests".to_string(), done: true },
            ],
            ..Default::default()
        };
        let text = render_sidebar_to_string(40, 20, &state);
        assert!(text.contains("Todos"), "should show 'Todos' header");
        assert!(text.contains("○ Fix bug"), "pending todo should show ○");
        assert!(text.contains("✓ Write tests"), "done todo should show ✓");
    }

    #[test]
    fn buffer_sidebar_changes_renders_above_session() {
        let mut state = SidebarState {
            model_name: "gpt-4o".to_string(),
            ..Default::default()
        };
        state.record_file_change("file.rs".into(), 1, 0);
        let text = render_sidebar_to_string(40, 20, &state);
        let changes_pos = text.find("Changes").expect("Changes header not found");
        let session_pos = text.find("Session").expect("Session header not found");
        assert!(changes_pos < session_pos, "Changes should render above Session");
    }

    #[test]
    fn buffer_sidebar_no_changes_no_header() {
        let state = SidebarState::default();
        let text = render_sidebar_to_string(40, 20, &state);
        assert!(!text.contains("Changes"), "no changes = no 'Changes' header");
    }

    #[test]
    fn buffer_sidebar_empty_model_shows_none() {
        let state = SidebarState::default();
        let text = render_sidebar_to_string(40, 20, &state);
        assert!(text.contains("(none)"), "empty model should show '(none)'");
    }

    #[test]
    fn buffer_sidebar_session_title_displayed() {
        let state = SidebarState {
            session_title: "My Session".to_string(),
            ..Default::default()
        };
        let text = render_sidebar_to_string(40, 20, &state);
        assert!(text.contains("My Session"), "should show session title");
    }

    #[test]
    fn buffer_sidebar_empty_title_shows_untitled() {
        let state = SidebarState::default();
        let text = render_sidebar_to_string(40, 20, &state);
        assert!(text.contains("(untitled)"), "empty title should show '(untitled)'");
    }

    #[test]
    fn buffer_sidebar_title_renders_before_model() {
        let state = SidebarState {
            session_title: "My Session".to_string(),
            model_name: "gpt-4o".to_string(),
            ..Default::default()
        };
        let text = render_sidebar_to_string(40, 20, &state);
        let title_pos = text.find("My Session").expect("title not found");
        let model_pos = text.find("gpt-4o").expect("model not found");
        assert!(title_pos < model_pos, "title should render before model");
    }
}

