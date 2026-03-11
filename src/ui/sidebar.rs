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
    /// Number of open tasks (from persistent task store).
    pub open_task_count: usize,
    /// Accumulated file changes from write tools this session.
    pub changes: Vec<FileChange>,
    /// Current git branch name (None if not in a git repo).
    pub git_branch: Option<String>,
    /// Whether the repo has uncommitted changes (None if not in a git repo).
    pub git_dirty: Option<bool>,
    /// Repository name (last path component of repo root).
    pub git_repo_name: Option<String>,
    /// Context window size for the current model.
    pub context_window: u64,
    /// Last-reported prompt tokens (per-call context pressure).
    pub last_prompt_tokens: u64,
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
            open_task_count: 0,
            changes: Vec::new(),
            git_branch: None,
            git_dirty: None,
            git_repo_name: None,
            context_window: 0,
            last_prompt_tokens: 0,
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

/// Maximum visible characters for branch name (sidebar is ~40 cols, indent + status suffix).
const MAX_BRANCH_DISPLAY: usize = 28;

/// Render the sidebar into the given area.
pub fn render_sidebar(
    frame: &mut Frame,
    area: Rect,
    state: &SidebarState,
    theme: &Theme,
    context_pct: u8,
) {
    let mut lines: Vec<Line> = Vec::new();
    let header_color = theme.border_color(context_pct);

    // -- Session section (top — most important context) --
    lines.push(Line::from(Span::styled(
        "Session",
        Style::default()
            .fg(header_color)
            .add_modifier(Modifier::BOLD),
    )));
    let title = if state.session_title.is_empty() {
        "(untitled)"
    } else {
        &state.session_title
    };
    lines.push(Line::from(Span::styled(
        format!(" {title}"),
        Style::default().fg(theme.fg),
    )));
    let model = if state.model_name.is_empty() {
        "(none)"
    } else {
        &state.model_name
    };
    lines.push(Line::from(Span::styled(
        format!(" {model}"),
        Style::default().fg(theme.fg),
    )));
    // Context pressure: Ctx: X/Y (Z%) — replaces verbose in/out/total lines
    if state.context_window > 0 {
        let pct = (state.last_prompt_tokens * 100).checked_div(state.context_window).unwrap_or(0);
        lines.push(Line::from(Span::styled(
            format!(
                " Ctx: {}/{} ({}%)",
                format_tokens(state.last_prompt_tokens),
                format_tokens(state.context_window),
                pct,
            ),
            Style::default().fg(theme.dim),
        )));
    } else {
        // Fallback when context_window is unknown
        lines.push(Line::from(Span::styled(
            format!(" Tokens: {}", format_tokens(state.total_tokens)),
            Style::default().fg(theme.dim),
        )));
    }
    match state.session_cost {
        Some(cost) => {
            lines.push(Line::from(Span::styled(
                format!(" Cost: ${:.4}", cost),
                Style::default().fg(theme.dim),
            )));
        }
        None => {
            lines.push(Line::from(Span::styled(
                " Cost: N/A",
                Style::default().fg(theme.dim),
            )));
        }
    }
    lines.push(Line::from(""));

    // -- Git section (below session, only when branch is known) --
    if let Some(branch) = &state.git_branch {
        lines.push(Line::from(Span::styled(
            "Git",
            Style::default()
                .fg(header_color)
                .add_modifier(Modifier::BOLD),
        )));
        if let Some(repo_name) = &state.git_repo_name {
            lines.push(Line::from(Span::styled(
                format!(" {repo_name}"),
                Style::default().fg(theme.dim),
            )));
        }
        let truncated_branch = if branch.chars().count() > MAX_BRANCH_DISPLAY {
            let s: String = branch.chars().take(MAX_BRANCH_DISPLAY - 1).collect();
            format!("{s}…")
        } else {
            branch.clone()
        };
        let status_text = match state.git_dirty {
            Some(true) => " · dirty",
            Some(false) => " · clean",
            None => "",
        };
        let status_color = match state.git_dirty {
            Some(true) => theme.warning,
            Some(false) => theme.success,
            None => theme.dim,
        };
        lines.push(Line::from(vec![
            Span::styled(format!(" {truncated_branch}"), Style::default().fg(theme.fg)),
            Span::styled(status_text, Style::default().fg(status_color)),
        ]));
        lines.push(Line::from(""));
    }

    // -- Changes section (if any files were modified) --
    if !state.changes.is_empty() {
        lines.push(Line::from(Span::styled(
            "Changes",
            Style::default()
                .fg(header_color)
                .add_modifier(Modifier::BOLD),
        )));
        for change in &state.changes {
            let mut spans = vec![Span::styled(
                format!(" {}", change.path),
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
            format!(" {} {file_word}", state.changes.len()),
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

    // -- Tasks section (if any open tasks) --
    if state.open_task_count > 0 {
        let task_word = if state.open_task_count == 1 { "task" } else { "tasks" };
        lines.push(Line::from(Span::styled(
            "Tasks",
            Style::default()
                .fg(header_color)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            format!(" {} open {task_word}", state.open_task_count),
            Style::default().fg(theme.fg),
        )));
    }

    // 1-char left padding via Block::padding keeps content off the vertical divider.
    let block = Block::default().padding(ratatui::widgets::Padding::new(1, 0, 0, 0));

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
            render_sidebar(frame, Rect::new(0, 0, width, height), state, &theme, 0);
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
    fn buffer_sidebar_token_display_ctx_format() {
        let state = SidebarState {
            last_prompt_tokens: 23400,
            context_window: 128000,
            total_tokens: 30000,
            ..Default::default()
        };
        let text = render_sidebar_to_string(40, 20, &state);
        assert!(text.contains("Ctx: 23.4k/128.0k (18%)"), "should show Ctx: format, got:\n{text}");
    }

    #[test]
    fn buffer_sidebar_cost_display() {
        let state = SidebarState {
            session_cost: Some(0.0512),
            ..Default::default()
        };
        let text = render_sidebar_to_string(40, 20, &state);
        assert!(text.contains("Cost: $0.0512"), "should show cost");
    }

    #[test]
    fn buffer_sidebar_cost_na_when_not_configured() {
        let state = SidebarState {
            session_cost: None,
            ..Default::default()
        };
        let text = render_sidebar_to_string(40, 20, &state);
        assert!(text.contains("Cost: N/A"), "should show N/A when cost not configured");
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
    fn buffer_sidebar_tasks_section() {
        let state = SidebarState {
            open_task_count: 3,
            ..Default::default()
        };
        let text = render_sidebar_to_string(40, 20, &state);
        assert!(text.contains("Tasks"), "should show 'Tasks' header");
        assert!(text.contains("3 open tasks"), "should show open task count");
    }

    #[test]
    fn buffer_sidebar_tasks_section_singular() {
        let state = SidebarState {
            open_task_count: 1,
            ..Default::default()
        };
        let text = render_sidebar_to_string(40, 20, &state);
        assert!(text.contains("1 open task"), "should use singular for 1 task");
    }

    #[test]
    fn buffer_sidebar_no_tasks_no_section() {
        let state = SidebarState::default();
        let text = render_sidebar_to_string(40, 20, &state);
        assert!(!text.contains("Tasks"), "no open tasks = no 'Tasks' header");
    }

    #[test]
    fn buffer_sidebar_session_renders_above_changes() {
        let mut state = SidebarState {
            model_name: "gpt-4o".to_string(),
            ..Default::default()
        };
        state.record_file_change("file.rs".into(), 1, 0);
        let text = render_sidebar_to_string(40, 20, &state);
        let session_pos = text.find("Session").expect("Session header not found");
        let changes_pos = text.find("Changes").expect("Changes header not found");
        assert!(session_pos < changes_pos, "Session should render above Changes");
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

    // -- Git section tests --

    #[test]
    fn buffer_sidebar_git_section_shows_branch() {
        let state = SidebarState {
            git_branch: Some("main".to_string()),
            git_repo_name: Some("steve".to_string()),
            git_dirty: Some(false),
            ..Default::default()
        };
        let text = render_sidebar_to_string(40, 20, &state);
        assert!(text.contains("Git"), "should show 'Git' header");
        assert!(text.contains("main"), "should show branch name");
        assert!(text.contains("steve"), "should show repo name");
    }

    #[test]
    fn buffer_sidebar_git_dirty_shows_dirty() {
        let state = SidebarState {
            git_branch: Some("feature/xyz".to_string()),
            git_dirty: Some(true),
            ..Default::default()
        };
        let text = render_sidebar_to_string(40, 20, &state);
        assert!(text.contains("dirty"), "should show 'dirty' status");
    }

    #[test]
    fn buffer_sidebar_git_clean_shows_clean() {
        let state = SidebarState {
            git_branch: Some("main".to_string()),
            git_dirty: Some(false),
            ..Default::default()
        };
        let text = render_sidebar_to_string(40, 20, &state);
        assert!(text.contains("clean"), "should show 'clean' status");
    }

    #[test]
    fn buffer_sidebar_no_git_no_section() {
        let state = SidebarState::default();
        let text = render_sidebar_to_string(40, 20, &state);
        assert!(!text.contains("Git"), "no git = no 'Git' header");
    }

    #[test]
    fn buffer_sidebar_git_renders_above_changes() {
        let mut state = SidebarState {
            git_branch: Some("main".to_string()),
            git_dirty: Some(false),
            ..Default::default()
        };
        state.record_file_change("file.rs".into(), 1, 0);
        let text = render_sidebar_to_string(40, 25, &state);
        let git_pos = text.find("Git").expect("Git header not found");
        let changes_pos = text.find("Changes").expect("Changes header not found");
        assert!(git_pos < changes_pos, "Git should render above Changes");
    }

    #[test]
    fn buffer_sidebar_context_display() {
        let state = SidebarState {
            last_prompt_tokens: 50000,
            context_window: 128000,
            ..Default::default()
        };
        let text = render_sidebar_to_string(40, 20, &state);
        assert!(text.contains("Ctx:"), "should show Ctx: prefix");
        assert!(text.contains("39%"), "should show percentage");
    }

    #[test]
    fn buffer_sidebar_context_zero_window_fallback() {
        let state = SidebarState {
            total_tokens: 5000,
            context_window: 0,
            ..Default::default()
        };
        let text = render_sidebar_to_string(40, 20, &state);
        assert!(text.contains("Tokens:"), "should fall back to Tokens: format");
        assert!(text.contains("5.0k"), "should show total tokens");
    }

    #[test]
    fn default_sidebar_state_has_no_git_info() {
        let state = SidebarState::default();
        assert!(state.git_branch.is_none());
        assert!(state.git_dirty.is_none());
        assert!(state.git_repo_name.is_none());
        assert_eq!(state.context_window, 0);
        assert_eq!(state.last_prompt_tokens, 0);
    }
}

